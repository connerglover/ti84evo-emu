//! Raw host USB tokens. Guest firmware supplies every descriptor and protocol response.
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

use evo_core::{Machine, StopReason};
use evo_image::{Bundle, Layout};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Attach { attached: bool },
    Reset {},
    Setup { bytes: [u8; 8] },
    Out { endpoint: u8, bytes: Vec<u8> },
    In { endpoint: u8 },
    Run { cycles: u64 },
    Quit {},
}

fn request(machine: &mut Machine, req: Request) -> Result<Value, String> {
    let value = match req {
        Request::Attach { attached } => {
            machine.usb_attach(attached);
            json!({})
        }
        Request::Reset {} => {
            machine.usb_bus_reset().map_err(|e| format!("{e:?}"))?;
            json!({})
        }
        Request::Setup { bytes } => {
            machine.usb_setup(bytes).map_err(|e| format!("{e:?}"))?;
            json!({})
        }
        Request::Out { endpoint, bytes } => {
            machine
                .usb_out(endpoint, &bytes)
                .map_err(|e| format!("{e:?}"))?;
            json!({})
        }
        Request::In { endpoint } => {
            let bytes = machine.usb_in(endpoint).map_err(|e| format!("{e:?}"))?;
            json!({"bytes": bytes})
        }
        Request::Run { cycles } => {
            // Bound one request, so malformed clients cannot stall shutdown indefinitely.
            if cycles > evo_core::CPU_HZ {
                return Err("one run request may advance at most one guest second".into());
            }
            let stop = machine.run(cycles);
            if !matches!(stop, StopReason::CycleLimit | StopReason::Idle) {
                return Err(format!("guest stopped: {stop:?}"));
            }
            json!({"cycles": machine.sched.now, "stop": format!("{stop:?}")})
        }
        Request::Quit {} => unreachable!("handled by serve"),
    };
    Ok(value)
}

fn serve(
    machine: &mut Machine,
    input: mpsc::Receiver<io::Result<Vec<u8>>>,
    mut output: impl Write,
    interrupted: &AtomicBool,
) -> io::Result<()> {
    // JSON arrays for a maximum-size USB packet fit comfortably within this bound.
    while !interrupted.load(Ordering::Relaxed) {
        let line = match input.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => line?,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let parsed = if line.len() > 16_384 {
            Err("request exceeds 16 KiB".to_string())
        } else {
            serde_json::from_slice::<Request>(&line).map_err(|e| e.to_string())
        };
        let quit = matches!(parsed, Ok(Request::Quit {}));
        let result = match parsed {
            Ok(Request::Quit {}) => Ok(json!({})),
            Ok(req) => request(machine, req),
            Err(e) => Err(e),
        };
        let response = match result {
            Ok(data) => json!({"ok": true, "data": data}),
            Err(error) => json!({"ok": false, "error": error}),
        };
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
        if quit {
            break;
        }
    }
    Ok(())
}

pub fn main(bundle: &Path, flash: Option<&Path>, provision: bool) -> ExitCode {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let interrupted = Arc::new(AtomicBool::new(false));
        let signal = interrupted.clone();
        ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
        let bundle = Bundle::open(bundle)?;
        let mut machine = if let Some(path) = flash {
            let nv = super::run::read_flash_file(path).map_err(|()| "cannot read flash")?;
            match nv {
                Some(nv) => Machine::from_nv(nv, &Layout::EVO_7_1)?,
                None => Machine::from_bundle(&bundle, &Layout::EVO_7_1)?,
            }
        } else {
            Machine::from_bundle(&bundle, &Layout::EVO_7_1)?
        };
        if provision {
            evo_core::provision(&mut machine)?;
        }
        // The secure/BSP boot alone takes roughly 300M cycles. Let image 1 start its
        // file/screenshot services before presenting the serial port to TI Connect.
        let stop = machine.run(evo_core::CPU_HZ * 5);
        if !matches!(stop, StopReason::CycleLimit | StopReason::Idle) {
            return Err(format!("boot stopped: {stop:?}").into());
        }
        eprintln!("usb-bridge: ready");
        let (tx, rx) = mpsc::sync_channel(16);
        std::thread::spawn(move || {
            let mut input = io::stdin().lock();
            loop {
                // Bound allocation before parsing; leave the rest of an oversized line unread.
                let mut line = Vec::new();
                let read = std::io::Read::take(&mut input, 16_385).read_until(b'\n', &mut line);
                match read {
                    Ok(0) => break,
                    Ok(_) if line.len() > 16_384 => {
                        let _ = tx.send(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "request exceeds 16 KiB",
                        )));
                        break;
                    }
                    Ok(_) => {
                        if tx.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error));
                        break;
                    }
                }
            }
        });
        let served = serve(&mut machine, rx, io::stdout().lock(), &interrupted);
        // Persist even after a broken pipe: successful guest transfers changed real NOR.
        if let Some(path) = flash {
            machine.nv_state().write_file(path)?;
        }
        served?;
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("usb-bridge: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_bus_token_shapes() {
        assert!(
            serde_json::from_str::<Request>(r#"{"op":"setup","bytes":[128,6,0,1,0,0,18,0]}"#)
                .is_ok()
        );
        assert!(serde_json::from_str::<Request>(r#"{"op":"setup","bytes":[128]}"#).is_err());
        assert!(
            serde_json::from_str::<Request>(r#"{"op":"out","endpoint":1,"bytes":[256]}"#).is_err()
        );
        assert!(serde_json::from_str::<Request>(r#"{"op":"reset","ignored":1}"#).is_err());
    }
}
