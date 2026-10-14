//! Versioned save states: [`Machine::save_state`] and [`Machine::load_state`].
//!
//! A save state is
//!
//! ```text
//! STATE_MAGIC (8 bytes) | STATE_VERSION (u32 LE) | bundle id (32 bytes) | postcard(MachineState)
//! ```
//!
//! The bundle id is the machine's fingerprint of the OS it booted (SHA-256 over the package
//! bytes and the two slot bases), so a state never lands on a machine running different code.
//! The state holds the CPU, the scheduler queue, SRAM, the unmapped log and every device's
//! state. It does not hold the flash or the NOR security registers: those persist separately,
//! in the flash file (spec §6.5; `Spif::save` skips the security registers). Breakpoints and
//! the recent trace are host-side debugging aids and are not saved either.

use armv8m::{Cpu, SleepKind};
use evo_hw::soc::SRAM_BUFFER;
use evo_hw::{DeviceState, StateError, UnmappedLog};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Scheduler;
use crate::machine::{Machine, RecentSteps};

/// First bytes of every save state.
pub const STATE_MAGIC: &[u8; 8] = b"EVOSTATE";
/// Format version this build writes and reads.
pub const STATE_VERSION: u32 = 1;

/// Bytes before the postcard body: magic, version, bundle id.
const HEADER_LEN: usize = 8 + 4 + 32;

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
        let mut out = Vec::with_capacity(HEADER_LEN + state.sram.len() + 4096);
        out.extend_from_slice(STATE_MAGIC);
        out.extend_from_slice(&STATE_VERSION.to_le_bytes());
        out.extend_from_slice(&self.bundle_id);
        // Encoding plain data into a Vec cannot fail; an empty body would fail to load, loudly.
        out.extend(postcard::to_stdvec(&state).unwrap_or_default());
        out
    }

    /// Restore a state written by [`save_state`](Self::save_state) on a machine booted from
    /// the same bundle. All or nothing: on error the machine is unchanged.
    ///
    /// Checked in order: the magic ([`StateError::BadMagic`]), the version
    /// ([`StateError::Version`]), the bundle id and then, after decoding the whole body, the
    /// SRAM size and the device set ([`StateError::Mismatch`]); a truncated or corrupt body is
    /// a [`StateError::Decode`]. A device that rejects its bytes leaves every device as it
    /// was. Breakpoints are kept; the recent trace starts empty.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        if bytes.len() < STATE_MAGIC.len() || &bytes[..STATE_MAGIC.len()] != STATE_MAGIC {
            return Err(StateError::BadMagic);
        }
        let truncated = || StateError::Decode(postcard::Error::DeserializeUnexpectedEnd);
        let version = bytes
            .get(8..12)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(u32::from_le_bytes)
            .ok_or_else(truncated)?;
        if version != STATE_VERSION {
            return Err(StateError::Version {
                found: version,
                expected: STATE_VERSION,
            });
        }
        let id = bytes.get(12..HEADER_LEN).ok_or_else(truncated)?;
        if id != self.bundle_id {
            return Err(StateError::Mismatch(
                "save state is from another bundle".into(),
            ));
        }
        let (state, rest) = postcard::take_from_bytes::<MachineState>(&bytes[HEADER_LEN..])?;
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
