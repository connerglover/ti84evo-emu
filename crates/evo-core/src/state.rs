//! Versioned save states: [`Machine::save_state`] and [`Machine::load_state`].
//!
//! A save state is
//!
//! ```text
//! STATE_MAGIC (8 bytes) | STATE_VERSION (u32 LE) | bundle id (32 bytes)
//!   | flash fingerprint (32 bytes) | body checksum (32 bytes) | body = postcard(MachineState)
//! ```
//!
//! The bundle id is the machine's fingerprint of the OS it booted (SHA-256 over the package
//! bytes and the two slot bases), so a state never lands on a machine running different code.
//! The flash fingerprint is SHA-256(flash bytes ‖ NOR security registers) when the state was
//! taken: the state holds RAM and device state that describe that flash (file systems, caches,
//! the ITS), so it loads only on a machine whose non-volatile state is the same (see
//! [`Machine::nv_fingerprint`]). The body checksum is SHA-256 of the body; a body that does not
//! match it is [`StateError::Corrupt`].
//!
//! The state holds the CPU, the scheduler queue, SRAM, the unmapped log and every device's
//! state. It does not hold the flash or the NOR security registers: those persist separately,
//! in the flash file (spec §6.5; `Spif::save` skips the security registers). Breakpoints and
//! the recent trace are host-side debugging aids and are not saved either.
//!
//! Version history: 1 had no flash fingerprint and no checksum (bundle id, then the body);
//! version 1 states are rejected ([`StateError::Version`]).

use armv8m::{Cpu, SleepKind};
use evo_hw::dev::spif::Spif;
use evo_hw::nor::SECREGS_LEN;
use evo_hw::soc::{FLASH_BUFFER, SRAM_BUFFER};
use evo_hw::{DeviceState, StateError, UnmappedLog};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Scheduler;
use crate::machine::{ERASED, FLASH_SIZE, Machine, RecentSteps};

/// First bytes of every save state.
pub const STATE_MAGIC: &[u8; 8] = b"EVOSTATE";
/// Format version this build writes and reads.
pub const STATE_VERSION: u32 = 2;

/// Header offsets: bundle id, flash fingerprint, body checksum, body.
const ID_AT: usize = 8 + 4;
const NV_AT: usize = ID_AT + 32;
const SUM_AT: usize = NV_AT + 32;
/// Bytes before the postcard body.
const HEADER_LEN: usize = SUM_AT + 32;

/// Everything a save state restores, in the order it is encoded.
#[derive(Serialize, Deserialize)]
struct MachineState {
    cpu: Cpu,
    sched: Scheduler,
    sram: Vec<u8>,
    log: UnmappedLog,
    pc_hint: u32,
    resets: u64,
    boot_vtor: u32,
    sleep: Option<SleepKind>,
    applied_irqs: u64,
    devices: Vec<DeviceState>,
}

/// The fingerprint of an OS image set: SHA-256(package bytes ‖ secure slot base LE ‖
/// non-secure slot base LE).
pub(crate) fn bundle_id(package: &[u8], secure_slot_base: u32, ns_slot_base: u32) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(package);
    h.update(secure_slot_base.to_le_bytes());
    h.update(ns_slot_base.to_le_bytes());
    h.finalize().into()
}

impl Machine {
    /// The fingerprint of the non-volatile state now: SHA-256(flash bytes ‖ NOR security
    /// registers), both read as [`nv_state`](Self::nv_state) reads them (flash cut or padded
    /// with erased bytes to [`FLASH_SIZE`]). It hashes exactly the bytes `nv_state` returns,
    /// so equal fingerprints mean equal [`nv_state`](Self::nv_state)s.
    pub fn nv_fingerprint(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        let flash = self.bus.region_bytes(FLASH_BUFFER).unwrap_or_default();
        let size = FLASH_SIZE as usize;
        h.update(&flash[..flash.len().min(size)]);
        // The map always holds `FLASH_SIZE` bytes of flash; this pads like `nv_state` would.
        let erased = [ERASED; 4096];
        let mut pad = size.saturating_sub(flash.len());
        while pad > 0 {
            let n = pad.min(erased.len());
            h.update(&erased[..n]);
            pad -= n;
        }
        match self
            .bus
            .device::<Spif>("spif")
            .map(|s| s.nor().secregs())
            .filter(|r| r.len() == SECREGS_LEN)
        {
            Some(secregs) => h.update(secregs),
            None => h.update([ERASED; SECREGS_LEN]),
        }
        h.finalize().into()
    }

    /// The machine's state now, as a versioned save state (see the module docs).
    pub fn save_state(&self) -> Vec<u8> {
        let state = MachineState {
            cpu: self.cpu.clone(),
            sched: self.sched.clone(),
            sram: self
                .bus
                .region_bytes(SRAM_BUFFER)
                .map(<[u8]>::to_vec)
                .unwrap_or_default(),
            log: self.bus.log.clone(),
            pc_hint: self.bus.pc_hint,
            resets: self.resets,
            boot_vtor: self.boot_vtor,
            sleep: self.sleep,
            applied_irqs: self.applied_irqs,
            devices: self.bus.device_states(),
        };
        // Encoding plain data into a Vec cannot fail; an empty body would fail to load, loudly.
        let body = postcard::to_stdvec(&state).unwrap_or_default();
        let mut out = Vec::with_capacity(HEADER_LEN + body.len());
        out.extend_from_slice(STATE_MAGIC);
        out.extend_from_slice(&STATE_VERSION.to_le_bytes());
        out.extend_from_slice(&self.bundle_id);
        out.extend_from_slice(&self.nv_fingerprint());
        out.extend_from_slice(&Sha256::digest(&body));
        out.extend_from_slice(&body);
        out
    }

    /// Restore a state written by [`save_state`](Self::save_state) on a machine booted from
    /// the same bundle, with the same flash and NOR security registers it had then. All or
    /// nothing: on error the machine is unchanged.
    ///
    /// Checked in order: the magic ([`StateError::BadMagic`]), the version
    /// ([`StateError::Version`]), the body checksum ([`StateError::Corrupt`]; a header cut short
    /// is a [`StateError::Decode`]), the bundle id and the flash fingerprint, then, after
    /// decoding the whole body, the SRAM size and the device set ([`StateError::Mismatch`]); a
    /// body that does not decode is a [`StateError::Decode`]. A device that rejects its bytes
    /// leaves every device as it was. Breakpoints are kept; the recent trace starts empty.
    ///
    /// The flash is not in the state, so a state taken before the guest wrote flash no longer
    /// loads after that write: rewinding a machine to an earlier state needs the flash of that
    /// time too (e.g. a flash file saved with the state).
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        if bytes.len() < STATE_MAGIC.len() || &bytes[..STATE_MAGIC.len()] != STATE_MAGIC {
            return Err(StateError::BadMagic);
        }
        let truncated = || StateError::Decode(postcard::Error::DeserializeUnexpectedEnd);
        let version = bytes
            .get(8..ID_AT)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(u32::from_le_bytes)
            .ok_or_else(truncated)?;
        if version != STATE_VERSION {
            return Err(StateError::Version {
                found: version,
                expected: STATE_VERSION,
            });
        }
        if bytes.len() < HEADER_LEN {
            return Err(truncated());
        }
        let body = &bytes[HEADER_LEN..];
        if Sha256::digest(body)[..] != bytes[SUM_AT..HEADER_LEN] {
            return Err(StateError::Corrupt);
        }
        if bytes[ID_AT..NV_AT] != self.bundle_id {
            return Err(StateError::Mismatch(
                "save state is from another bundle".into(),
            ));
        }
        if bytes[NV_AT..SUM_AT] != self.nv_fingerprint() {
            return Err(StateError::Mismatch(
                "save state was taken with other flash contents (flash file or security \
                 registers changed since)"
                    .into(),
            ));
        }
        let (state, rest) = postcard::take_from_bytes::<MachineState>(body)?;
        if !rest.is_empty() {
            return Err(StateError::Mismatch(format!(
                "{} trailing bytes after the save state",
                rest.len()
            )));
        }
        let sram_len = self.bus.region_bytes(SRAM_BUFFER).map_or(0, <[u8]>::len);
        if state.sram.len() != sram_len {
            return Err(StateError::Mismatch(format!(
                "{} bytes of SRAM in the state, {sram_len} in this machine",
                state.sram.len()
            )));
        }
        // The only step that can still fail, so it goes first: `load_device_states` checks the
        // device names and order and is itself all or nothing (a device that rejects its bytes
        // leaves every device with its own state again). Everything after it cannot fail.
        self.bus.load_device_states(&state.devices)?;

        self.cpu.restore_from(state.cpu);
        self.sched = state.sched;
        if let Some(sram) = self.bus.buffer_mut(SRAM_BUFFER) {
            sram.copy_from_slice(&state.sram);
        }
        self.bus.log = state.log;
        self.bus.pc_hint = state.pc_hint;
        self.resets = state.resets;
        self.boot_vtor = state.boot_vtor;
        self.sleep = state.sleep;
        self.applied_irqs = state.applied_irqs;
        // Requests the devices queued before the load belong to the replaced scheduler.
        self.bus.take_pulses();
        self.bus.take_event_requests();
        self.bus.now = self.sched.now;
        self.stopped_at = None;
        self.recent = RecentSteps::default();
        Ok(())
    }
}
