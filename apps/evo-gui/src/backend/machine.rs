//! [`MachineBackend`]: the emulated TI-84 Evo behind [`EmuBackend`].
//!
//! This is the only GUI file that names the core crates (`evo_core`, `evo_hw`, `evo_image`,
//! `armv8m`); `tests/boundary.rs` enforces that. Besides Plan 1's `Machine` API (`run`,
//! `step`, `breakpoints`, `bus.peek`, `bus.log`, `cpu.regs`, `sched.now`) it uses Plan 2's
//! host API: `screen`, the backlight `Pwm`, `set_key`, `take_uart_tx`, `save_state` /
//! `load_state`, and the flash file (`from_bundle_or_nv`, `from_nv`, `nv_state`, `NvState`).
//! Plan 4's debugger API: watchpoints, goals (`arm_*`, `disarm`), `write_reg`, `peripherals`, and the
//! bus monitor and `poke` on `bus`; and its I/O API: `inject_uart_rx`, `set_gpio_input`,
//! `set_usb_cc`, `set_rtc` / `rtc_seconds`, `power_state` and `export_ram` / `import_ram`.

use std::path::{Path, PathBuf};

use armv8m::decode::is_32bit;
use armv8m::{Sec, decode16, decode32, disasm};
use evo_core::machine::{FLASH_NS, FLASH_S, FLASH_SIZE, SRAM_NS, SRAM_S, SRAM_SIZE};
use evo_core::{AccessDir as CoreDir, BusRecord as CoreRecord};
use evo_core::{
    Key, Machine, NvBootError, NvError, NvState, StopReason, UartPort, WatchKinds as CoreKinds,
    WatchRange,
};
use evo_hw::dev::gpio::Gpio;
use evo_hw::dev::lcdc::Pwm;
use evo_hw::dev::usb::UsbTypeC;
use evo_image::{Bundle, ImageError, Layout};

use crate::backend::{
    AccessDir, BackendError, BusRecord, CpuView, DeviceView, DisasmLine, EmuBackend, Frame, Goal,
    HardwareView, HwChange, KeyEvent, MemRegion, RegRow, RunStatus, UartId, UnmappedRow,
    WatchKinds, Watchpoint,
};
use crate::connect::types::{
    CalculatorFile, CsvTarget, ReceivedFile, SendOptions, UsbOperation, UsbProgress, UsbResult,
};
use crate::keypad::KeyId;

/// Why a bundle could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("cannot read bundle: {0}")]
    Read(#[from] std::io::Error),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Boot(#[from] NvBootError),
    /// The flash file exists but cannot be read; it is left untouched.
    #[error("{path}: {source}")]
    Flash { path: PathBuf, source: NvError },
}

/// What a bundle's metadata says about the OS it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleInfo {
    /// e.g. `7.1.0.4421`.
    pub build_version: String,
    pub target_device: String,
    pub prodid: u32,
}

/// Parses the bundle in `bytes` and returns its metadata, without booting it.
pub fn inspect_bundle(bytes: &[u8]) -> Result<BundleInfo, OpenError> {
    let meta = Bundle::from_bytes(bytes)?.metadata;
    Ok(BundleInfo {
        build_version: meta.build_version,
        target_device: meta.target_device,
        prodid: meta.prodid,
    })
}

const _: () = assert!(crate::backend::CLOCK_HZ == evo_core::CPU_HZ);
const _: () =
    assert!(crate::input::MIN_HOLD_CYCLES == evo_core::keys::TAP_HOLD_MS * evo_core::CPU_HZ / 1000);

/// Converts RGB888 pixels to RGBA8: clears `dst`, then appends `(r, g, b, 255)` per pixel.
pub fn rgb888_to_rgba(src: &[u8], dst: &mut Vec<u8>) {
    dst.clear();
    dst.reserve(src.len() / 3 * 4);
    for &[r, g, b] in src.as_chunks::<3>().0 {
        dst.extend_from_slice(&[r, g, b, 255]);
    }
}

/// The core key a GUI key presses: the key named [`KeyId::core_name`] in the core's keypad
/// table (`evo_hw::dev::keypad::KEY_NAMES`, the TI-84 Evo matrix of `docs/hardware/keypad.md`
/// "Key map"), so the GUI keeps no matrix of its own. Every `KeyId` names a core key
/// (`every_gui_key_is_the_core_key_of_its_name`).
pub fn core_key(id: KeyId) -> Key {
    Key::from_name(id.core_name())
        .unwrap_or_else(|| unreachable!("{id:?}: no core key {}", id.core_name()))
}

/// 64-bit FNV-1a of `bytes`.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes
        .iter()
        .fold(OFFSET, |h, &b| (h ^ u64::from(b)).wrapping_mul(PRIME))
}

/// The emulated calculator, with persistent storage keyed by its original OS bundle.
pub struct MachineBackend {
    machine: Machine,
    name: String,
    /// `machine:{fnv1a64 of the bundle bytes}`.
    tag: String,
    /// `{nv_dir}/{fnv1a64 of the bundle bytes:016x}.nv`.
    nv_path: PathBuf,
    /// True from an open with no flash file until provisioning succeeds.
    fresh: bool,
    /// [`Machine::nv_fingerprint`] last written to `nv_path` by this backend;
    /// unknown on open because boot may activate a pending update.
    /// `persist` writes once after open, then skips writes while this matches.
    on_disk: Option<[u8; 32]>,
    /// The last LCD image, converted for the GUI.
    frame: Frame,
    /// The bus-monitor range as last set (the core has no getter), so `reset` can keep it.
    monitor: Option<(u32, u32)>,
    /// The board inputs the Hardware panel sets (see [`Board`]).
    board: Board,
    /// [`EmuBackend::set_key`] calls not yet taken by `take_key_events`, oldest first; at most
    /// [`KEY_EVENT_CAP`].
    key_events: Vec<KeyEvent>,
}

/// The most key events kept undrained; older ones are dropped. The app drains them every
/// frame, so the cap only bounds a backend nobody drains.
const KEY_EVENT_CAP: usize = 4096;

/// GPIO pin of the charger status line (high = no charger).
const PIN_CHARGER: u8 = 9;
/// GPIO pin of the battery comparator (high = the battery is good).
const PIN_BATTERY: u8 = 11;
/// Seconds in a day: the RTC counts seconds of day below it.
const DAY_SECONDS: u32 = 86_400;

fn usb_error(error: impl std::fmt::Display) -> BackendError {
    BackendError::Invalid(error.to_string())
}

/// URI values use UTF-8 percent encoding; calculator names are token code points.
fn uri_value(value: &str) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            write!(out, "%{byte:02X}").expect("writing into a String");
        }
    }
    out
}

fn variable_uri(path: &str, token: &str, kind: u32) -> Result<String, BackendError> {
    if token.is_empty() || !token.len().is_multiple_of(4) || !token.is_ascii() {
        return Err(usb_error("Invalid calculator token name"));
    }
    let mut name = String::new();
    for word in token.as_bytes().as_chunks::<4>().0 {
        let text = std::str::from_utf8(word).map_err(usb_error)?;
        let value = u32::from_str_radix(text, 16).map_err(usb_error)?;
        name.push(char::from_u32(value).ok_or_else(|| usb_error("Invalid calculator token"))?);
    }
    Ok(format!(
        "hh01/{path}/var?name={}&type={kind}",
        uri_value(&name)
    ))
}

fn calculator_file(v: evo_core::connect_files::Variable) -> CalculatorFile {
    CalculatorFile {
        name: v.name,
        token_name: v.token_name,
        kind: v.kind,
        size: v.size,
        archived: v.archived,
        extension: v.extension,
    }
}

fn prepare_send(
    filename: &str,
    bytes: &[u8],
    options: &SendOptions,
) -> Result<Vec<evo_core::connect_files::VariableFile>, BackendError> {
    use evo_core::connect_files as files;
    let path = Path::new(filename);
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let name = if options.name.trim().is_empty() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("PROGRAM")
    } else {
        options.name.trim()
    };
    let file = match extension.as_str() {
        "py" => files::encode_python(name, bytes, options.archived).map_err(usb_error)?,
        "png" => {
            let mut reader = image::ImageReader::with_format(
                std::io::Cursor::new(bytes),
                image::ImageFormat::Png,
            );
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            limits.max_alloc = Some(64 * 1024 * 1024);
            reader.limits(limits);
            let image = reader.decode().map_err(usb_error)?;
            let fitted = image
                .resize(160, 105, image::imageops::FilterType::Nearest)
                .to_rgba8();
            let mut rgba =
                image::RgbaImage::from_pixel(160, 105, image::Rgba([255, 255, 255, 255]));
            image::imageops::overlay(
                &mut rgba,
                &fitted,
                i64::from((160 - fitted.width()) / 2),
                i64::from((105 - fitted.height()) / 2),
            );
            files::encode_image(options.image_slot, rgba.as_raw()).map_err(usb_error)?
        }
        "csv" => {
            let csv = std::str::from_utf8(bytes).map_err(usb_error)?;
            let mode = match options.csv_target {
                CsvTarget::Lists => files::CsvMode::Lists {
                    names: options
                        .name
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect(),
                },
                CsvTarget::Matrix => files::CsvMode::Matrix {
                    name: if options.name.trim().is_empty() {
                        "A".into()
                    } else {
                        options.name.trim().into()
                    },
                    rows: 0,
                    columns: 0,
                },
            };
            return files::encode_csv(csv, mode, options.archived).map_err(usb_error);
        }
        _ => {
            let kind = files::decode_variable(bytes)
                .map_err(usb_error)?
                .variable
                .kind;
            let archived = match kind {
                4 | 5 | 9 | 11 | 18 => true,
                7 | 12 | 13 | 14 => false,
                _ => options.archived,
            };
            files::prepare_native(
                bytes,
                (!options.name.trim().is_empty()).then_some(options.name.trim()),
                Some(archived),
            )
            .map_err(usb_error)?
        }
    };
    Ok(vec![file])
}

/// The OS can enumerate USB before its initial home screen is ready. Retry only
/// read requests explicitly rejected as busy; uploads may already have mutated data.
fn get_ready(
    connection: &mut evo_core::connect::Connect<'_>,
    uri: &str,
) -> Result<Vec<u8>, evo_core::connect::ConnectError> {
    for attempt in 0..20 {
        match connection.get(uri) {
            Err(evo_core::connect::ConnectError::Remote(message))
                if message == "BZ" && attempt < 19 =>
            {
                connection.advance(evo_core::CPU_HZ / 4)?;
            }
            result => return result,
        }
    }
    unreachable!("the final attempt always returns")
}

fn run_usb(
    machine: &mut Machine,
    operation: UsbOperation,
    progress: &mut dyn FnMut(UsbProgress) -> bool,
) -> Result<UsbResult, BackendError> {
    use evo_core::connect::{Connect, ConnectError, Phase};
    use evo_core::connect_files as files;
    use std::ops::ControlFlow;
    // Decode host files before attaching, so invalid input never changes guest state.
    let prepared = match &operation {
        UsbOperation::Send {
            filename,
            bytes,
            options,
        } => Some(
            prepare_send(filename, bytes, options)?
                .into_iter()
                .map(|file| (file, options.overwrite))
                .collect::<Vec<_>>(),
        ),
        UsbOperation::SendMany(uploads) => {
            let mut prepared = Vec::new();
            for upload in uploads {
                let files = prepare_send(&upload.filename, &upload.bytes, &upload.options)
                    .map_err(|error| usb_error(format!("{}: {error}", upload.filename)))?;
                prepared.extend(
                    files
                        .into_iter()
                        .map(|file| (file, upload.options.overwrite)),
                );
            }
            Some(prepared)
        }
        _ => None,
    };
    let bundle = match &operation {
        UsbOperation::InstallOs(bytes) => Some(Bundle::from_bytes(bytes).map_err(usb_error)?),
        _ => None,
    };
    let mut callback = |p: evo_core::connect::Progress| {
        let phase = match p.phase {
            Phase::Connecting => "Connecting",
            Phase::Sending => "Sending",
            Phase::Receiving => "Receiving",
            Phase::Finishing => "Finishing",
        }
        .into();
        if progress(UsbProgress {
            phase,
            transferred: p.transferred,
            total: p.total,
            detail: p.detail,
        }) {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    };
    let mut connection = Connect::new(machine, &mut callback).map_err(usb_error)?;
    let result = (|| {
        match operation {
            UsbOperation::Refresh => {
                let directory =
                    get_ready(&mut connection, "hh01/inf/res?name=directory&gotohome=1")
                        .map_err(usb_error)?;
                let attributes =
                    get_ready(&mut connection, "hh01/sys/attributes").map_err(usb_error)?;
                let dynamic = get_ready(&mut connection, "hh01/inf/res?name=dynamicinfo")
                    .map_err(usb_error)?;
                let info = serde_json::json!({"attributes": files::parse_info(&attributes).map_err(usb_error)?, "status": files::parse_info(&dynamic).map_err(usb_error)?});
                Ok(UsbResult::Directory {
                    files: files::parse_directory(&directory)
                        .map_err(usb_error)?
                        .into_iter()
                        .map(calculator_file)
                        .collect(),
                    info: device_summary(&info)?,
                })
            }
            UsbOperation::DeviceInfo => {
                let attributes =
                    get_ready(&mut connection, "hh01/sys/attributes").map_err(usb_error)?;
                let dynamic = get_ready(&mut connection, "hh01/inf/res?name=dynamicinfo")
                    .map_err(usb_error)?;
                let info = serde_json::json!({"attributes": files::parse_info(&attributes).map_err(usb_error)?, "status": files::parse_info(&dynamic).map_err(usb_error)?});
                Ok(UsbResult::Text(device_summary(&info)?))
            }
            UsbOperation::Receive(selected) => {
                let mut received = Vec::new();
                let total = selected.len();
                for file in selected {
                    let result = (|| {
                        let bytes = get_ready(
                            &mut connection,
                            &variable_uri("xfr", &file.token_name, file.kind)?,
                        )
                        .map_err(usb_error)?;
                        let (name, bytes) = files::export_variable(&bytes).map_err(usb_error)?;
                        Ok::<_, BackendError>(ReceivedFile { name, bytes })
                    })();
                    match result {
                        Ok(file) => received.push(file),
                        Err(error) => {
                            return Ok(UsbResult::Batch {
                                message: format!(
                                    "Received {} of {total} files; remaining files were not received",
                                    received.len()
                                ),
                                errors: vec![format!("{}: {error}", file.name)],
                                files: received,
                            });
                        }
                    }
                }
                Ok(UsbResult::Files(received))
            }
            UsbOperation::Send { .. } | UsbOperation::SendMany(_) => {
                let prepared = prepared.expect("Send was prepared before connection");
                for (index, (file, overwrite)) in prepared.iter().enumerate() {
                    // CSV uses an ASCII conversion envelope; its header supplies name/type.
                    let uri = format!(
                        "hh01/xfr/var?memtarget={}&policy={}",
                        u8::from(file.variable.archived),
                        u8::from(*overwrite)
                    );
                    if let Err(error) = connection.put(&uri, &file.bytes) {
                        return Ok(UsbResult::Batch {
                            files: Vec::new(),
                            errors: vec![format!("{}: {error}", file.variable.name)],
                            message: format!(
                                "Sent {index} of {} files; later files were not sent",
                                prepared.len()
                            ),
                        });
                    }
                }
                Ok(UsbResult::Done(format!("Sent {} file(s)", prepared.len())))
            }
            UsbOperation::Delete(selected) => {
                for (index, file) in selected.iter().enumerate() {
                    if let Err(error) =
                        connection.put(&variable_uri("del", &file.token_name, file.kind)?, &[0])
                    {
                        return Ok(UsbResult::Batch {
                            files: Vec::new(),
                            errors: vec![format!("{}: {error}", file.name)],
                            message: format!(
                                "Deleted {index} of {} files; later files were not deleted",
                                selected.len()
                            ),
                        });
                    }
                }
                Ok(UsbResult::Done(format!(
                    "Deleted {} file(s)",
                    selected.len()
                )))
            }
            UsbOperation::Screen => {
                let bytes = get_ready(&mut connection, "hh01/sys/screen").map_err(usb_error)?;
                Ok(UsbResult::Screen(Frame {
                    width: 320,
                    height: 240,
                    rgba: files::decode_screen(&bytes).map_err(usb_error)?,
                }))
            }
            UsbOperation::ExitExam => {
                let info = files::parse_info(
                    &get_ready(&mut connection, "hh01/inf/res?name=dynamicinfo")
                        .map_err(usb_error)?,
                )
                .map_err(usb_error)?;
                if !exam_enabled(&info)? {
                    return Ok(UsbResult::Done(
                        "Calculator is already out of exam mode".into(),
                    ));
                }
                match connection.put("hh01/xfr/var?policy=4", b"x") {
                    Ok(()) => (),
                    Err(ConnectError::Remote(message)) if message == "DI" => (),
                    Err(error) => return Err(usb_error(error)),
                }
                connection
                    .advance(evo_core::CPU_HZ * 5)
                    .map_err(usb_error)?;
                let info = files::parse_info(
                    &get_ready(&mut connection, "hh01/inf/res?name=dynamicinfo")
                        .map_err(usb_error)?,
                )
                .map_err(usb_error)?;
                if exam_enabled(&info)? {
                    return Err(usb_error("Calculator remains in exam mode"));
                }
                Ok(UsbResult::Done("Calculator exited exam mode".into()))
            }
            UsbOperation::InstallOs(_) => {
                let bundle = bundle.expect("OS bundle was parsed before connection");
                let uri = format!(
                    "hh01/upd/pkg?bundle=1&prodnum={}&version={}",
                    bundle.metadata.prodid,
                    uri_value(&bundle.metadata.build_version)
                );
                connection
                    .put(&uri, &bundle.package.bytes)
                    .map_err(usb_error)?;
                Ok(UsbResult::Done(
                "OS package transferred. Reset the calculator to activate the installed update."
                    .into(),
            ))
            }
            UsbOperation::GetResource(uri) => {
                validate_resource(&uri)?;
                Ok(UsbResult::Bytes(
                    get_ready(&mut connection, &uri).map_err(usb_error)?,
                ))
            }
            UsbOperation::PutResource { uri, bytes } => {
                validate_resource(&uri)?;
                connection.put(&uri, &bytes).map_err(usb_error)?;
                Ok(UsbResult::Done("Resource written".into()))
            }
            UsbOperation::Logs => {
                let info =
                    get_ready(&mut connection, "hh01/dbg/Get Event Info").map_err(usb_error)?;
                let events =
                    get_ready(&mut connection, "hh01/dbg/Get Events").map_err(usb_error)?;
                Ok(UsbResult::Files(vec![
                    ReceivedFile {
                        name: "info.bin".into(),
                        bytes: info,
                    },
                    ReceivedFile {
                        name: "events.bin".into(),
                        bytes: events,
                    },
                ]))
            }
            UsbOperation::DebugMessage(message) => {
                if message.is_empty() {
                    return Err(usb_error("Enter a debug message"));
                }
                let uri = format!("hh01/dbg/{message}");
                validate_resource(&uri)?;
                connection.put(&uri, b"x").map_err(usb_error)?;
                Ok(UsbResult::Done("Debug message sent".into()))
            }
        }
    })();
    let cleanup = connection.finish().map_err(usb_error);
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(UsbResult::Files(files)), Err(error)) => Ok(UsbResult::Batch {
            files,
            errors: vec![error.to_string()],
            message: "Files received, but calculator disconnect did not finish normally".into(),
        }),
        (
            Ok(UsbResult::Batch {
                files,
                mut errors,
                message,
            }),
            Err(error),
        ) => {
            errors.push(error.to_string());
            Ok(UsbResult::Batch {
                files,
                errors,
                message,
            })
        }
        (Ok(_), Err(error)) => Err(error),
        (Ok(result), Ok(())) => Ok(result),
    }
}

fn device_summary(info: &serde_json::Value) -> Result<String, BackendError> {
    let attributes = &info["attributes"];
    let status = &info["status"];
    let text = |value: &serde_json::Value| {
        value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string())
    };
    // These are firmware CBOR keys, before the official JS library renames them.
    let version = attributes["pkg-version"].as_str().unwrap_or("Unknown");
    let exam = exam_enabled(status)
        .map(|active| if active { "On" } else { "Off" })
        .unwrap_or("Unknown");
    Ok(format!(
        "OS version: {version}\nSerial number: {}\nFree RAM: {} bytes\nFree archive: {} bytes\nExam mode: {exam}\n\nDevice details\n{}",
        text(&attributes["id"]),
        text(&status["ram"]),
        text(&status["archive"]),
        serde_json::to_string_pretty(info).map_err(usb_error)?
    ))
}

fn validate_resource(uri: &str) -> Result<(), BackendError> {
    if !uri.starts_with("hh01/") || uri.len() > 2048 || uri.chars().any(char::is_control) {
        return Err(usb_error(
            "Resource must start with hh01/ and contain no control characters",
        ));
    }
    Ok(())
}

fn exam_enabled(info: &serde_json::Value) -> Result<bool, BackendError> {
    for key in ["p2t", "ptt"] {
        if let serde_json::Value::Bool(value) = &info[key] {
            return Ok(*value);
        }
        if let Some(value) = info[key].as_str() {
            if value == "true" {
                return Ok(true);
            }
            if value == "false" {
                return Ok(false);
            }
        }
    }
    Err(usb_error("Calculator did not report its exam-mode state"))
}

#[cfg(test)]
mod usb_adapter_tests {
    use super::*;

    #[test]
    fn token_query_uses_unicode_not_hex_text() {
        assert_eq!(
            variable_uri("xfr", "E830", 1).unwrap(),
            "hh01/xfr/var?name=%EE%A0%B0&type=1"
        );
        assert!(variable_uri("xfr", "E830&policy=1", 1).is_err());
        assert!(variable_uri("xfr", "D800", 1).is_err());
        assert_eq!(uri_value("a&b=?"), "a%26b%3D%3F");
    }

    #[test]
    fn png_dimensions_are_limited_before_pixel_expansion() {
        let mut png = Vec::new();
        image::ImageEncoder::write_image(
            image::codecs::png::PngEncoder::new(&mut png),
            &vec![255; 9000 * 4],
            9000,
            1,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
        assert!(
            png.len() < 1000,
            "compressed input is small despite its width"
        );
        let error = prepare_send("wide.png", &png, &SendOptions::default()).unwrap_err();
        assert!(
            error.to_string().to_lowercase().contains("limit"),
            "{error}"
        );
    }

    #[test]
    fn archive_only_native_image_ignores_generic_ram_default() {
        let image = evo_core::connect_files::encode_image(1, &vec![255; 160 * 105 * 4]).unwrap();
        let prepared = prepare_send("Image1.8ca2", &image.bytes, &SendOptions::default()).unwrap();
        assert_eq!(prepared.len(), 1);
        assert!(prepared[0].variable.archived);
    }

    #[test]
    fn information_uses_actual_firmware_attribute_names() {
        let info = serde_json::json!({"attributes": {"pkg-version": "7.1.0.4421", "id": 1163284273u32}, "status": {"ram": 502100, "archive": 3107976, "ptt": false}});
        let summary = device_summary(&info).unwrap();
        assert!(summary.contains("OS version: 7.1.0.4421"));
        assert!(summary.contains("Serial number: 1163284273"));
        assert!(summary.contains("Exam mode: Off"));
    }
}

/// The board conditions the host sets: they belong to the board, not the guest, so the
/// backend keeps them and drives them again into every new machine (reset, flash import). A
/// state load does not drive them: it restores the state exactly, and the backend reads the
/// levels back from it ([`Board::read`]).
#[derive(Clone, Copy, Debug)]
struct Board {
    /// GPIO 9 level.
    charger_line: bool,
    /// GPIO 11 level.
    battery_ok: bool,
    /// USB-C attached in either orientation.
    usb_attached: bool,
}

impl Board {
    /// What the core's machine starts with: pins 9 and 11 high (`BOARD_INPUTS_HIGH`), no
    /// cable.
    const POWER_ON: Self = Self {
        charger_line: true,
        battery_ok: true,
        usb_attached: false,
    };

    /// Drives these conditions into `m`.
    fn apply(self, m: &mut Machine) {
        m.set_gpio_input(PIN_CHARGER, Some(self.charger_line));
        m.set_gpio_input(PIN_BATTERY, Some(self.battery_ok));
        if self.usb_attached {
            m.set_usb_cc(1, 0);
        } else {
            m.set_usb_cc(0, 0);
        }
    }

    /// The conditions `m` has now: the levels of pins 9 and 11, and whether either CC is attached.
    /// A device that `m` does not have keeps the value in `self`.
    fn read(self, m: &Machine) -> Self {
        let gpio = m.bus.device::<Gpio>("gpio");
        let usb = m.bus.device::<UsbTypeC>("usbtc");
        Self {
            charger_line: gpio.map_or(self.charger_line, |g| g.pin_level(PIN_CHARGER)),
            battery_ok: gpio.map_or(self.battery_ok, |g| g.pin_level(PIN_BATTERY)),
            usb_attached: usb.map_or(self.usb_attached, |u| u.cc() != (0, 0)),
        }
    }
}

impl MachineBackend {
    /// Reads and boots the bundle at `path`; the backend is named after its file name. Its
    /// flash file lives in `nv_dir` (see [`from_bytes`](Self::from_bytes)).
    pub fn open(path: &Path, nv_dir: &Path) -> Result<Self, OpenError> {
        let bytes = std::fs::read(path)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self::from_bytes(&bytes, name, nv_dir)
    }

    /// Boots the bundle in `bytes`; `name` is for display only. If the bundle's flash file
    /// [`nv_path`](Self::nv_path) exists, the machine starts from it; a flash file that cannot
    /// be read is an [`OpenError::Flash`] and is left untouched. Otherwise flash starts erased
    /// and the backend [`needs_provision`](EmuBackend::needs_provision).
    pub fn from_bytes(bytes: &[u8], name: String, nv_dir: &Path) -> Result<Self, OpenError> {
        let bundle = Bundle::from_bytes(bytes)?;
        let hash = fnv1a64(bytes);
        let nv_path = nv_dir.join(format!("{hash:016x}.nv"));
        let fresh = !nv_path.exists();
        let nv = if !fresh {
            match NvState::read_file(&nv_path) {
                Ok(nv) => Some(nv),
                Err(source) => {
                    return Err(OpenError::Flash {
                        path: nv_path,
                        source,
                    });
                }
            }
        } else {
            None
        };
        let machine = Machine::from_bundle_or_nv(&bundle, &Layout::EVO_7_1, nv)?;
        Ok(Self {
            machine,
            name,
            tag: format!("machine:{hash:016x}"),
            nv_path,
            fresh,
            // A pending USB update may have changed flash while booting. Its new
            // fingerprint must not stand in for the original file's fingerprint.
            on_disk: None,
            frame: Frame {
                width: 0,
                height: 0,
                rgba: Vec::new(),
            },
            monitor: None,
            board: Board::POWER_ON,
            key_events: Vec::new(),
        })
    }

    /// Replaces the machine with a new one booted from the firmware in `nv`. The debugger's
    /// breakpoints, watchpoints and bus-monitor range and the board inputs carry over; the
    /// goal is dropped. On `Err` the machine is kept.
    fn rebuild(&mut self, nv: NvState) -> Result<(), NvBootError> {
        let mut m = Machine::from_nv(nv, &Layout::EVO_7_1)?;
        m.breakpoints = std::mem::take(&mut self.machine.breakpoints);
        if let Err(e) = m.set_watchpoints(self.machine.watchpoints()) {
            // They were validated when set, so this does not happen.
            log::error!("the new machine dropped the watchpoints: {e}");
        }
        m.bus.set_bus_monitor(self.monitor);
        self.board.apply(&mut m);
        self.machine = m;
        Ok(())
    }

    /// Where [`EmuBackend::persist`] writes the flash file.
    pub fn nv_path(&self) -> &Path {
        &self.nv_path
    }

    /// The instruction at `addr`, read through `bus.peek` as `Machine::disasm_at` does.
    fn disasm_one(&self, addr: u32) -> Option<DisasmLine> {
        let bus = &self.machine.bus;
        let hw1 = bus.peek(addr, 2)? as u16;
        let (instr, len, raw) = if is_32bit(hw1) {
            let hw2 = bus.peek(addr.wrapping_add(2), 2)? as u16;
            (decode32(hw1, hw2), 4, u32::from(hw1) << 16 | u32::from(hw2))
        } else {
            (decode16(hw1), 2, u32::from(hw1))
        };
        Some(DisasmLine {
            addr,
            len,
            raw,
            text: disasm(&instr, addr, len),
        })
    }
}

fn access_dir(dir: CoreDir) -> AccessDir {
    match dir {
        CoreDir::Read => AccessDir::Read,
        CoreDir::Write => AccessDir::Write,
        CoreDir::Exec => AccessDir::Exec,
    }
}

fn run_status(stop: StopReason) -> RunStatus {
    match stop {
        StopReason::CycleLimit | StopReason::Halted | StopReason::Stepped => RunStatus::Running,
        StopReason::Idle => RunStatus::Idle,
        StopReason::Breakpoint(a) => RunStatus::Breakpoint(a),
        StopReason::Lockup { pc } => RunStatus::Lockup { pc },
        StopReason::Bkpt { pc, imm } => RunStatus::Bkpt { pc, imm },
        StopReason::Watchpoint {
            addr,
            dir,
            value,
            pc,
        } => RunStatus::Watchpoint {
            addr,
            dir: access_dir(dir),
            value,
            pc,
        },
        StopReason::Goal { pc } => RunStatus::Goal { pc },
    }
}

fn watch_range(w: Watchpoint) -> WatchRange {
    WatchRange {
        start: w.start,
        len: w.len,
        kinds: CoreKinds {
            read: w.kinds.read,
            write: w.kinds.write,
            exec: w.kinds.exec,
        },
    }
}

fn watchpoint(r: WatchRange) -> Watchpoint {
    Watchpoint {
        start: r.start,
        len: r.len,
        kinds: WatchKinds {
            read: r.kinds.read,
            write: r.kinds.write,
            exec: r.kinds.exec,
        },
    }
}

fn bus_record(r: CoreRecord) -> BusRecord {
    BusRecord {
        addr: r.addr,
        width: r.width,
        dir: access_dir(r.dir),
        value: r.value,
        pc: r.pc,
        cycle: r.cycle,
    }
}

/// The core's name for [`EmuBackend::set_core_reg`] index `i` (`None` past xPSR).
fn core_reg_name(i: usize) -> Option<String> {
    Some(match i {
        0..=12 => format!("r{i}"),
        13 => "sp".into(),
        14 => "lr".into(),
        15 => "pc".into(),
        16 => "xpsr".into(),
        _ => return None,
    })
}

impl EmuBackend for MachineBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn state_tag(&self) -> String {
        self.tag.clone()
    }

    fn framebuffer(&mut self) -> Option<&Frame> {
        let f = self.machine.screen()?;
        rgb888_to_rgba(&f.rgb, &mut self.frame.rgba);
        self.frame.width = f.width;
        self.frame.height = f.height;
        self.frame.is_valid().then_some(&self.frame)
    }

    fn backlight(&self) -> f32 {
        self.machine
            .bus
            .device::<Pwm>("pwm")
            .map_or(1.0, |p| p.brightness())
    }

    fn set_key(&mut self, key: KeyId, pressed: bool) {
        self.machine.set_key(core_key(key), pressed);
        if self.key_events.len() >= KEY_EVENT_CAP {
            self.key_events.remove(0);
        }
        self.key_events.push(KeyEvent {
            key,
            pressed,
            cycle: self.machine.sched.now,
        });
    }

    fn run_for(&mut self, cycles: u64) -> RunStatus {
        run_status(self.machine.run(cycles))
    }

    fn step(&mut self) -> RunStatus {
        run_status(self.machine.step())
    }

    fn cycles(&self) -> u64 {
        self.machine.sched.now
    }

    fn take_uart(&mut self) -> Vec<u8> {
        // UART0 is the TF-M / BSP console. UART2 is a link transport the GUI does not show;
        // its TX buffer drops its oldest bytes by itself.
        self.machine.take_uart_tx(UartPort::Uart0)
    }

    fn save_state(&self) -> Result<Vec<u8>, BackendError> {
        Ok(self.machine.save_state())
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), BackendError> {
        self.machine
            .load_state(bytes)
            .map_err(|e| BackendError::BadState(e.to_string()))?;
        // A pure restore. Driving the panel's levels in would latch level-type GPIO status
        // that the guest had cleared; the panel follows the loaded state's levels instead.
        self.board = self.board.read(&self.machine);
        Ok(())
    }

    fn reset(&mut self) {
        // Flash is kept: the new machine starts from the current non-volatile state.
        if let Err(e) = self.rebuild(self.machine.nv_state()) {
            // Preserve the running machine if its flash no longer contains bootable firmware.
            log::error!("reset failed, machine kept: {e}");
        }
    }

    fn persist(&mut self) -> Result<(), BackendError> {
        // Until provisioning succeeds the flash file must not exist: it is what tells the
        // next launch that the unit is provisioned.
        if self.fresh {
            return Ok(());
        }
        // The periodic save runs on the UI thread: hashing the flash costs far less than
        // copying and writing 16 MiB with a sync, and the guest rarely writes flash.
        let now = self.machine.nv_fingerprint();
        if self.on_disk == Some(now) && self.nv_path.exists() {
            return Ok(());
        }
        if let Some(dir) = self.nv_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| BackendError::Persist(e.to_string()))?;
        }
        self.machine
            .nv_state()
            .write_file(&self.nv_path)
            .map_err(|e| BackendError::Persist(e.to_string()))?;
        self.on_disk = Some(now);
        Ok(())
    }

    fn needs_provision(&self) -> bool {
        self.fresh
    }

    fn usb_available(&self) -> bool {
        !self.fresh
    }

    fn usb(
        &mut self,
        operation: UsbOperation,
        progress: &mut dyn FnMut(UsbProgress) -> bool,
    ) -> Result<UsbResult, BackendError> {
        if self.fresh {
            return Err(BackendError::Invalid(
                "Finish calculator setup before using USB".into(),
            ));
        }
        let result = run_usb(&mut self.machine, operation, progress);
        // Connect owns cable transitions, including cleanup on failure/cancellation.
        self.board = self.board.read(&self.machine);
        result
    }

    fn provision(&mut self) -> Result<(), BackendError> {
        // `evo_core::provision` uses breakpoints of its own and clears them, and fails on any
        // other stop: the debugger's breakpoints, watchpoints, bus monitor and goal are put
        // aside while it runs (the goal is dropped, as by `reset`).
        let breakpoints = std::mem::take(&mut self.machine.breakpoints);
        let watchpoints = self.machine.watchpoints();
        self.machine.disarm();
        let cleared = self.machine.set_watchpoints(Vec::new());
        debug_assert!(cleared.is_ok(), "an empty set is valid");
        self.machine.bus.set_bus_monitor(None);
        let result = evo_core::provision(&mut self.machine);
        self.machine.breakpoints = breakpoints;
        if let Err(e) = self.machine.set_watchpoints(watchpoints) {
            // They were validated when set, so this does not happen.
            log::error!("provisioning dropped the watchpoints: {e}");
        }
        self.machine.bus.set_bus_monitor(self.monitor);
        match result {
            Ok(report) => {
                log::info!("provisioned: {report:?}");
                self.fresh = false;
                Ok(())
            }
            Err(e) => Err(BackendError::Provision(e.to_string())),
        }
    }

    fn cpu(&self) -> CpuView {
        let r = &self.machine.cpu.regs;
        let mut core = [0; 16];
        core[..13].copy_from_slice(&r.r);
        core[13] = r.sp();
        core[14] = r.lr;
        core[15] = r.pc;
        CpuView {
            core,
            xpsr: r.xpsr.to_u32(),
            secure: r.sec == Sec::Secure,
            handler: r.handler_mode(),
            banked: vec![
                ("msp_s", r.msp.s),
                ("msp_ns", r.msp.ns),
                ("psp_s", r.psp.s),
                ("psp_ns", r.psp.ns),
                ("msplim_s", r.msplim.s),
                ("msplim_ns", r.msplim.ns),
                ("psplim_s", r.psplim.s),
                ("psplim_ns", r.psplim.ns),
                ("control_s", r.control.s),
                ("control_ns", r.control.ns),
                ("primask_s", u32::from(r.primask.s)),
                ("primask_ns", u32::from(r.primask.ns)),
                ("basepri_s", u32::from(r.basepri.s)),
                ("basepri_ns", u32::from(r.basepri.ns)),
                ("faultmask_s", u32::from(r.faultmask.s)),
                ("faultmask_ns", u32::from(r.faultmask.ns)),
            ],
        }
    }

    fn peek(&self, addr: u32, len: usize) -> Vec<Option<u8>> {
        (0..len)
            .map(|i| {
                let a = u32::try_from(i).ok().and_then(|i| addr.checked_add(i))?;
                self.machine.bus.peek(a, 1).map(|v| v as u8)
            })
            .collect()
    }

    fn disasm(&self, addr: u32, count: usize) -> Vec<DisasmLine> {
        let mut lines = Vec::with_capacity(count.min(1024));
        let mut at = addr;
        for _ in 0..count {
            let Some(line) = self.disasm_one(at) else {
                break;
            };
            at = at.wrapping_add(u32::from(line.len));
            lines.push(line);
        }
        lines
    }

    fn unmapped(&self) -> Vec<UnmappedRow> {
        self.machine
            .bus
            .log
            .entries()
            .map(|(a, count)| UnmappedRow {
                addr: a.addr,
                width: a.width,
                kind: a.kind.as_str(),
                value: a.write,
                pc: a.pc,
                secure: a.secure,
                count,
            })
            .collect()
    }

    fn breakpoints(&self) -> Vec<u32> {
        self.machine.breakpoints.iter().copied().collect()
    }

    fn toggle_breakpoint(&mut self, addr: u32) {
        let bps = &mut self.machine.breakpoints;
        if !bps.remove(&addr) {
            bps.insert(addr);
        }
    }

    fn poke(&mut self, addr: u32, bytes: &[u8]) -> Result<(), BackendError> {
        // Every byte is checked before any is written, so a refused write changes nothing.
        let at = |i: usize| u32::try_from(i).ok().and_then(|i| addr.checked_add(i));
        for i in 0..bytes.len() {
            match at(i) {
                Some(a) if self.machine.bus.peek(a, 1).is_some() => {}
                // Past 0xffffffff: the first address that does not exist wraps to 0.
                bad => return Err(BackendError::Unwritable(bad.unwrap_or(0))),
            }
        }
        for (i, &b) in bytes.iter().enumerate() {
            let a = at(i).unwrap_or_else(|| unreachable!("checked above"));
            if !self.machine.bus.poke(a, 1, u32::from(b)) {
                // `peek` succeeded on this byte just now, so this does not happen.
                return Err(BackendError::Unwritable(a));
            }
        }
        Ok(())
    }

    fn memory_regions(&self) -> Vec<MemRegion> {
        [
            ("Flash (NS)", FLASH_NS, FLASH_SIZE),
            ("Flash (S)", FLASH_S, FLASH_SIZE),
            ("SRAM (NS)", SRAM_NS, SRAM_SIZE),
            ("SRAM (S)", SRAM_S, SRAM_SIZE),
        ]
        .into_iter()
        .map(|(name, start, len)| MemRegion {
            name: name.into(),
            start,
            len,
        })
        .collect()
    }

    fn set_core_reg(&mut self, index: usize, value: u32) -> Result<(), BackendError> {
        let name =
            core_reg_name(index).ok_or_else(|| BackendError::BadRegister(index.to_string()))?;
        self.set_banked_reg(&name, value)
    }

    fn set_banked_reg(&mut self, name: &str, value: u32) -> Result<(), BackendError> {
        self.machine
            .write_reg(name, value)
            .map_err(|_| BackendError::BadRegister(name.to_owned()))
    }

    fn set_breakpoints(&mut self, addrs: Vec<u32>) {
        self.machine.breakpoints = addrs.into_iter().collect();
    }

    fn begin_goal(&mut self, goal: Goal) -> Result<Option<RunStatus>, BackendError> {
        Ok(match goal {
            Goal::StepOver => self.machine.arm_step_over().map(run_status),
            Goal::StepOut => {
                self.machine.arm_step_out();
                None
            }
            Goal::RunTo(addr) => {
                self.machine.arm_run_to(addr);
                None
            }
        })
    }

    fn cancel_goal(&mut self) {
        self.machine.disarm();
    }

    fn watchpoints(&self) -> Vec<Watchpoint> {
        self.machine
            .watchpoints()
            .into_iter()
            .map(watchpoint)
            .collect()
    }

    fn set_watchpoints(&mut self, w: Vec<Watchpoint>) -> Result<(), BackendError> {
        self.machine
            .set_watchpoints(w.into_iter().map(watch_range).collect())
            .map_err(|e| BackendError::Invalid(e.to_string()))
    }

    fn set_bus_monitor(&mut self, range: Option<(u32, u32)>) {
        self.monitor = range;
        self.machine.bus.set_bus_monitor(range);
    }

    fn bus_records(&self) -> Vec<BusRecord> {
        self.machine
            .bus
            .bus_records()
            .into_iter()
            .map(bus_record)
            .collect()
    }

    fn clear_bus_records(&mut self) {
        self.machine.bus.clear_bus_records();
    }

    fn peripherals(&self) -> Vec<DeviceView> {
        self.machine
            .peripherals()
            .into_iter()
            .map(|d| DeviceView {
                name: d.name.to_owned(),
                base: d.base,
                regs: d
                    .regs
                    .into_iter()
                    .map(|r| RegRow {
                        name: r.name,
                        offset: r.offset,
                        value: r.value,
                        note: r.note,
                    })
                    .collect(),
            })
            .collect()
    }

    fn take_uart2(&mut self) -> Vec<u8> {
        self.machine.take_uart_tx(UartPort::Uart2)
    }

    fn send_uart(&mut self, port: UartId, bytes: &[u8]) {
        let port = match port {
            UartId::Uart0 => UartPort::Uart0,
            UartId::Uart2 => UartPort::Uart2,
        };
        self.machine.inject_uart_rx(port, bytes);
    }

    fn take_key_events(&mut self) -> Vec<KeyEvent> {
        std::mem::take(&mut self.key_events)
    }

    fn hardware(&self) -> HardwareView {
        let power = self.machine.power_state();
        HardwareView {
            backlight: self.backlight(),
            charger_line: self.board.charger_line,
            battery_ok: self.board.battery_ok,
            usb_attached: self.board.usb_attached,
            power_hold: power.hold,
            deep_sleep: power.deep_sleep,
            rtc_seconds: self.machine.rtc_seconds(),
        }
    }

    fn set_hardware(&mut self, change: HwChange) -> Result<(), BackendError> {
        let m = &mut self.machine;
        match change {
            HwChange::ChargerLine(on) => {
                self.board.charger_line = on;
                m.set_gpio_input(PIN_CHARGER, Some(on));
            }
            HwChange::BatteryOk(ok) => {
                self.board.battery_ok = ok;
                m.set_gpio_input(PIN_BATTERY, Some(ok));
            }
            HwChange::UsbAttached(on) => {
                self.board.usb_attached = on;
                m.set_usb_cc(u8::from(on), 0);
            }
            HwChange::RtcSeconds(s) if s >= DAY_SECONDS => {
                return Err(BackendError::Invalid(
                    "RTC time must be below 24 h".to_owned(),
                ));
            }
            HwChange::RtcSeconds(s) => m.set_rtc(s),
        }
        Ok(())
    }

    fn export_ram(&self) -> Result<Vec<u8>, BackendError> {
        Ok(self.machine.export_ram())
    }

    fn import_ram(&mut self, bytes: &[u8]) -> Result<(), BackendError> {
        self.machine
            .import_ram(bytes)
            .map_err(|e| BackendError::BadState(e.to_string()))
    }

    fn export_flash(&self, path: &Path) -> Result<(), BackendError> {
        self.machine
            .nv_state()
            .write_file(path)
            .map_err(|e| BackendError::Io(e.to_string()))
    }

    fn import_flash(&mut self, path: &Path) -> Result<(), BackendError> {
        // `read_file` checks the header and the file size before it reads the body, so a
        // file of the wrong size (however large) is refused unread.
        let bad = |e: &dyn std::fmt::Display| BackendError::BadState(e.to_string());
        let nv = NvState::read_file(path).map_err(|e| bad(&e))?;
        self.rebuild(nv).map_err(|e| bad(&e))?;
        // The unit now boots from the imported flash; the file at `nv_path` does not hold
        // it yet, so the next `persist` writes it.
        self.fresh = false;
        self.on_disk = None;
        Ok(())
    }
}
