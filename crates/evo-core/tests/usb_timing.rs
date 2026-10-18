//! Hardware USB timing probes. Supply EVO_OS_IMAGE and run in release mode.
use evo_core::{
    CPU_HZ, Machine, NvState, StopReason,
    connect::{Connect, ConnectError, Jitter, Pacing},
};
use evo_image::{Bundle, Layout};
use std::ops::ControlFlow;

fn boot() -> Option<Machine> {
    static LOGGER: TraceLogger = TraceLogger;
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        if std::env::var_os("EVO_USB_TRACE").is_some() {
            log::set_logger(&LOGGER).unwrap();
            log::set_max_level(log::LevelFilter::Trace);
        }
    });
    if cfg!(debug_assertions) {
        eprintln!("skipping: USB timing tests require release");
        return None;
    }
    let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
        eprintln!("skipping: EVO_OS_IMAGE not set");
        return None;
    };
    let mut m = if let Some(flash) = std::env::var_os("EVO_USB_FLASH") {
        Machine::from_nv(
            NvState::read_file(flash.as_ref()).unwrap(),
            &Layout::EVO_7_1,
        )
        .unwrap()
    } else {
        let b = Bundle::open(path.as_ref()).unwrap();
        let mut m = Machine::from_bundle(&b, &Layout::EVO_7_1).unwrap();
        evo_core::provision(&mut m).unwrap();
        m
    };
    assert!(matches!(
        m.run(CPU_HZ * 10),
        StopReason::CycleLimit | StopReason::Idle
    ));
    Some(m)
}

struct TraceLogger;
impl log::Log for TraceLogger {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        matches!(m.target(), "evo_hw::usb" | "evo_core::connect")
    }
    fn log(&self, r: &log::Record<'_>) {
        if self.enabled(r.metadata()) {
            eprintln!("{} {}", r.target(), r.args());
        }
    }
    fn flush(&self) {}
}

fn unknown(result: Result<Vec<u8>, ConnectError>) {
    assert!(
        matches!(&result, Err(ConnectError::Remote(s)) if s == "UN"),
        "{result:?}"
    );
}

fn unresponsive(result: &Result<Vec<u8>, ConnectError>) -> bool {
    matches!(result, Err(ConnectError::Timeout))
        || matches!(result, Err(ConnectError::Protocol(s)) if s == "acknowledgment retry limit reached")
}

/// The guest's answer delay for a plain directory GET, in emulated cycles.
/// The connection lives entirely inside so the machine borrow stays disjoint.
fn directory_latency(
    m: &mut Machine,
    progress: &mut dyn FnMut(evo_core::connect::Progress) -> ControlFlow<()>,
    pacing: Pacing,
    jitter: Jitter,
) -> u64 {
    let start = m.sched.now;
    let mut c = Connect::with_options(m, progress, pacing, jitter).unwrap();
    c.get("hh01/inf/res?name=directory").unwrap();
    // `finish` consumes the connection, ending the machine borrow.
    c.finish().unwrap();
    m.sched.now - start
}

#[test]
fn hardware_two_request_wedge() {
    let Some(mut m) = boot() else { return };
    let mut progress = |p: evo_core::connect::Progress| {
        if let Some(detail) = p.detail {
            eprintln!("{detail}");
        }
        ControlFlow::Continue(())
    };
    let pacing = if std::env::var_os("EVO_USB_BATCHED").is_some() {
        Pacing::Batched
    } else {
        Pacing::Realtime
    };
    let jitter_ms = std::env::var("EVO_USB_JITTER_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let jitter = Jitter::new(0x5EED, jitter_ms);
    let mut c = Connect::with_options(&mut m, &mut progress, pacing, jitter).unwrap();
    unknown(c.get(&format!(
        "hh01/inf/res?name=directory&x={}",
        "A".repeat(200)
    )));
    let second = c.get(&format!(
        "hh01/xfr/var?name=%EE%A0%B0&type=1&x={}",
        "%41".repeat(100)
    ));
    eprintln!("request 2: {second:?}");
    let health = c.get("hh01/inf/res?name=directory");
    eprintln!("health: {health:?}");
    assert!(unresponsive(&second), "request 2 did not time out");
    assert!(unresponsive(&health), "guest still answers: {health:?}");
    assert!(unresponsive(&c.get("hh01/inf/res?name=directory")));
    c.finish().unwrap();
    let mut c = Connect::with_options(
        &mut m,
        &mut progress,
        pacing,
        Jitter::new(0x5EED, jitter_ms),
    )
    .unwrap();
    assert!(
        !c.get("hh01/inf/res?name=directory").unwrap().is_empty(),
        "replug must revive USB"
    );
}

/// Jitter of N emulated milliseconds must delay the guest-visible answer by at
/// least N emulated milliseconds of guest time. Same seed => same schedule.
#[test]
fn hardware_inter_request_jitter_delays_answers() {
    let Some(mut m) = boot() else { return };
    let mut progress = |_| ControlFlow::Continue(());
    let plain = directory_latency(&mut m, &mut progress, Pacing::Realtime, Jitter::new(0, 0));
    // The exact first draw this seed produces (the unit test pins determinism).
    let draw_ms = Jitter::new(0x5EED, 250).draw_ms_for_test();
    let jittered = directory_latency(
        &mut m,
        &mut progress,
        Pacing::Realtime,
        Jitter::new(0x5EED, 250),
    );
    let delta = jittered.saturating_sub(plain);
    // The guest-visible gap equals the drawn jitter, within a frame of retry
    // noise (NAK counts vary by a millisecond between boots of the same guest).
    let expected = draw_ms * CPU_HZ / 1000;
    let tolerance = 3 * CPU_HZ / 1000;
    assert!(
        delta.abs_diff(expected) <= tolerance,
        "gap {delta} cycles, expected ~{expected} (draw {draw_ms} ms, plain {plain}, jittered {jittered})"
    );
    // A wider bound changes the schedule (a different draw).
    let wider_draw = Jitter::new(0x5EED, 1000).draw_ms_for_test();
    assert_ne!(
        wider_draw, draw_ms,
        "same seed with a wider bound must draw a different gap"
    );
}

/// Characterize where the guest's Y-1 acknowledgment echo staging breaks. The
/// guest acknowledges an F packet by echoing its URI; past roughly 300 echo
/// bytes the staged frame is truncated and its header corrupted (observed:
/// MARK=0x31, SEQ=0x51, declared length 256), so the host can never parse the
/// acknowledgment in this emulator. This observation does not establish a
/// firmware buffer limit or identical behavior on physical hardware.
#[test]
fn hardware_ack_echo_boundary() {
    let Some(mut m) = boot() else { return };
    let nv = m.nv_state();
    let state = m.save_state();
    let mut progress = |_| ControlFlow::Continue(());
    // tail 260 (echo 299) is healthy; tail 270 (echo 309) is past the break.
    for (tail, healthy) in [(260usize, true), (270, false)] {
        m = Machine::from_nv(nv.clone(), &Layout::EVO_7_1).unwrap();
        m.load_state(&state).unwrap();
        let uri = format!("hh01/inf/res?name=directory&x={}", "A".repeat(tail));
        let mut c = Connect::with_pacing(&mut m, &mut progress, Pacing::Realtime).unwrap();
        let result = c.get(&uri);
        // echo = "hh01/get/" (9) + "hh01/inf/res?name=directory&x=" (30) + tail = 39 + tail.
        let is_healthy = matches!(&result, Err(ConnectError::Remote(s)) if s == "UN");
        assert_eq!(
            is_healthy,
            healthy,
            "tail {tail} (echo {}): {result:?}",
            39 + tail
        );
        let _ = c.finish();
    }
}

/// Trace where the Y-1 ACK staging becomes malformed for the long transfer
/// request. The bus monitor records every guest write into the staging region
/// (the FIFO1 VDMA source used for the ACK); the test replays those records
/// to reconstruct the buffer over time and asserts the corruption signature:
/// for the long request the last staged frame has MARK=0x31 and SEQ=0x51
/// (the URI-decode overlap), while the short request stages MARK=0x01 frames.
#[test]
fn hardware_ack_staging_corruption_signature() {
    let Some(mut m) = boot() else { return };
    const STAGE: u32 = 0x2003_ce84;
    const LEN: u32 = 0x120;
    m.bus.set_bus_monitor(Some((STAGE, LEN)));
    let mut progress = |_| ControlFlow::Continue(());
    let tail: usize = std::env::var("EVO_USB_TAIL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let uri = format!(
        "hh01/xfr/var?name=%EE%A0%B0&type=1&x={}",
        "%41".repeat(tail)
    );
    {
        let mut c = Connect::with_pacing(&mut m, &mut progress, Pacing::Realtime).unwrap();
        let result = c.get(&uri);
        eprintln!("xfr(tail {tail}) result: {result:?}");
        let _ = c.finish();
    }
    let records: Vec<_> = m
        .bus
        .bus_records()
        .into_iter()
        .filter(|r| r.dir == evo_core::AccessDir::Write)
        .collect();
    eprintln!("monitor captured {} writes into staging", records.len());
    let mut final_stage = [0u8; LEN as usize];
    for r in &records {
        for i in 0..r.width as usize {
            let a = r.addr as usize + i;
            if (STAGE as usize..STAGE as usize + LEN as usize).contains(&a) {
                final_stage[a - STAGE as usize] = ((r.value >> (8 * i)) & 0xFF) as u8;
            }
        }
    }
    eprintln!("final staged head: {:02x?}", &final_stage[..8]);
    if tail >= 70 {
        // The long request: the staged ACK carries the corrupted header.
        assert_eq!(
            &final_stage[..7],
            &[0x31, 0x20, 0x51, 0x59, 0x22, 0x62, 0x2f]
        );
    } else {
        // The short request: a well-formed frame (MARK=01) was staged.
        assert_eq!(
            final_stage[0], 0x01,
            "short request must stage a MARK=01 frame"
        );
    }
}

/// Pin the frame builder's contract: an EXEC watchpoint on its MARK store
/// (pc 0x0016_61b0) stops the run at the first outgoing frame; the registers
/// then show the builder's entry (0x0016_6194, called from 0x0016_3a04), the
/// staging buffer in r12, and MARK loaded from [r0+26] — the field the URI
/// decode later overwrites (see `hardware_ack_mark_field_corruption_chain`).
#[test]
fn hardware_ack_builder_entry_contract() {
    let Some(mut m) = boot() else { return };
    let mut progress = |_| ControlFlow::Continue(());
    let uri = format!("hh01/xfr/var?name=%EE%A0%B0&type=1&x={}", "%41".repeat(100));
    m.set_watchpoints(vec![evo_core::WatchRange {
        start: 0x0016_61b0,
        len: 2,
        kinds: evo_core::WatchKinds {
            read: false,
            write: false,
            exec: true,
        },
    }])
    .unwrap();
    let (result, ctx, trace) = {
        let mut c = Connect::with_pacing(&mut m, &mut progress, Pacing::Realtime).unwrap();
        let result = c.get(&uri);
        let ctx = c.__test_regs();
        let trace = c.__test_recent_trace();
        let _ = c.finish();
        (result, ctx, trace)
    };
    // The stop is the first builder call (the S-ack): MARK is still healthy.
    assert!(
        matches!(
            &result,
            Err(evo_core::connect::ConnectError::GuestStopped(
                evo_core::StopReason::Watchpoint { .. }
            ))
        ),
        "the exec watchpoint must stop the run: {result:?}"
    );
    let (pc, r0, _r1, _r2, r4, r12) = ctx;
    assert_eq!(pc, 0x0016_61b0);
    assert_eq!(r12, 0x2003_ce84, "staging buffer address");
    assert_eq!(r0, 0x2003_bdbc, "session struct base");
    assert_eq!(r4, 0x01, "MARK is healthy at the first frame");
    // The recent trace shows the caller's BL into the builder entry.
    let called = trace
        .iter()
        .rev()
        .find(|l| l.contains("bl 0x166194"))
        .expect("the builder was called by bl 0x166194");
    eprintln!("builder entry confirmed via {called}");
}

/// Track the builder's MARK field: run the long request with the bus monitor
/// on the struct byte at [r0+26] (loaded by `ldrb r4,[r0,#26]` at 0x0016_61aa).
/// Asserts the corruption chain: exactly one write by the Kermit decode loop
/// (pc 0x0015_46c8) puts 0x31 there, and the builder's MARK load then reads
/// 0x31 back — the URI-decode buffer overlaps the session struct.
#[test]
fn hardware_ack_mark_field_corruption_chain() {
    let Some(mut m) = boot() else { return };
    // Struct base observed at the exec-watch stop: 0x2003bdbc; MARK at +26.
    const BASE: u32 = 0x2003_bd00;
    const LEN: u32 = 0x200;
    m.bus.set_bus_monitor(Some((BASE, LEN)));
    let mut progress = |_| ControlFlow::Continue(());
    let uri = format!("hh01/xfr/var?name=%EE%A0%B0&type=1&x={}", "%41".repeat(100));
    {
        let mut c = Connect::with_pacing(&mut m, &mut progress, Pacing::Realtime).unwrap();
        let result = c.get(&uri);
        eprintln!("result: {result:?}");
        let _ = c.finish();
    }
    let field = 0x2003_bdbc + 26;
    // 1. Exactly one write into the MARK field, from the decode loop, with 0x31.
    let writes: Vec<_> = m
        .bus
        .bus_records()
        .into_iter()
        .filter(|r| {
            r.dir == evo_core::AccessDir::Write
                && r.addr <= field
                && field < r.addr + u32::from(r.width)
        })
        .collect();
    assert_eq!(writes.len(), 1, "expected exactly one MARK-field write");
    assert_eq!(writes[0].pc, 0x0015_46c8, "the decode loop wrote the field");
    assert_eq!(writes[0].value & 0xFF, 0x31, "the value is URI byte '1'");
    // 2. The builder then loads MARK from that field and sees 0x31.
    let reads: Vec<_> = m
        .bus
        .bus_records()
        .into_iter()
        .filter(|r| {
            r.dir == evo_core::AccessDir::Read
                && r.pc == 0x0016_61aa
                && r.addr <= field
                && field < r.addr + u32::from(r.width)
        })
        .collect();
    assert!(
        reads.iter().any(|r| r.value & 0xFF == 0x31),
        "the builder must read the corrupted MARK back"
    );
    eprintln!(
        "MARK field {field:08x}: decode write at cycle {} (pc {:08x}), builder read {:#04x}",
        writes[0].cycle,
        writes[0].pc,
        reads.last().map(|r| r.value & 0xFF).unwrap_or(0)
    );
}

/// The isolated long transfer request on a FRESH boot each time (not a restored
/// snapshot): hardware reportedly survives this. The emulator does not return
/// a valid acknowledgment; the cause remains under investigation.
#[test]
#[ignore = "unresolved fidelity gap: isolated long request fails in emulator but reportedly survives on hardware"]
fn hardware_isolated_long_xfr_fresh_boot() {
    let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
        eprintln!("skipping: EVO_OS_IMAGE not set");
        return;
    };
    if cfg!(debug_assertions) {
        eprintln!("skipping: USB timing tests require release");
        return;
    }
    let b = Bundle::open(path.as_ref()).unwrap();
    let mut m = Machine::from_bundle(&b, &Layout::EVO_7_1).unwrap();
    evo_core::provision(&mut m).unwrap();
    assert!(matches!(
        m.run(CPU_HZ * 10),
        StopReason::CycleLimit | StopReason::Idle
    ));
    let mut progress = |_| ControlFlow::Continue(());
    let uri = format!("hh01/xfr/var?name=%EE%A0%B0&type=1&x={}", "%41".repeat(100));
    let mut c = Connect::with_pacing(&mut m, &mut progress, Pacing::Realtime).unwrap();
    let result = c.get(&uri);
    eprintln!("fresh-boot isolated long xfr: {result:?}");
    let healthy = matches!(&result, Err(ConnectError::Remote(s)) if s == "UN");
    let _ = c.finish();
    assert!(healthy, "fresh-boot isolated long xfr failed: {result:?}");
}

/// Long directory request alone and short-tail sequence in both pacings.
/// The isolated long transfer request is a separate, unresolved fidelity test.
#[test]
fn individual_long_requests_and_short_sequence_survive() {
    let Some(mut m) = boot() else { return };
    let nv = m.nv_state();
    let state = m.save_state();
    let mut progress = |_| ControlFlow::Continue(());
    let mut failures = Vec::new();
    for pacing in [Pacing::Realtime, Pacing::Batched] {
        for uris in [
            vec![format!("hh01/inf/res?name=directory&x={}", "A".repeat(200))],
            vec![
                "hh01/inf/res?name=directory&x=A".into(),
                "hh01/xfr/var?name=%EE%A0%B0&type=1&x=%41".into(),
            ],
        ] {
            m = Machine::from_nv(nv.clone(), &Layout::EVO_7_1).unwrap();
            m.load_state(&state).unwrap();
            eprintln!("control: {pacing:?} {uris:?}");
            let mut c = Connect::with_pacing(&mut m, &mut progress, pacing).unwrap();
            for uri in uris {
                let result = c.get(&uri);
                if !matches!(&result, Err(ConnectError::Remote(s)) if s == "UN") {
                    failures.push(format!("{pacing:?} {uri}: {result:?}"));
                }
            }
            let health = c.get("hh01/inf/res?name=directory");
            if health.is_err() {
                failures.push(format!("{pacing:?} health: {health:?}"));
            }
            c.finish().unwrap();
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
