use evo_core::machine::{FLASH_S, SRAM_S};
use evo_core::{Machine, NvState, StopReason};
use evo_hw::nor::{NOR_SIZE, SECREGS_LEN};
use evo_hw::soc::FLASH_BUFFER;
use evo_image::Layout;
use evo_image::testutil::Img;

fn saved_image(header_size: u16, value: u8) -> NvState {
    let body_address = FLASH_S + Layout::EVO_7_1.secure_slot_base + u32::from(header_size);
    let mut body = vec![0; 0x104];
    body[..4].copy_from_slice(&(SRAM_S + 0x1000).to_le_bytes());
    body[4..8].copy_from_slice(&(body_address + 0x101).to_le_bytes());
    body[0x100..0x102].copy_from_slice(&(0x2000u16 | u16::from(value)).to_le_bytes());
    body[0x102..].copy_from_slice(&0xe7feu16.to_le_bytes()); // b .
    let image = Img::new(header_size, body).full_bytes();
    let mut flash = vec![0xff; NOR_SIZE];
    let start = Layout::EVO_7_1.secure_slot_base as usize;
    flash[start..start + image.len()].copy_from_slice(&image);
    flash[0x90000] = 0xa5;
    flash[Layout::EVO_7_1.ns_slot_base as usize] = 0x5a;
    NvState::new(flash, vec![0x12; SECREGS_LEN]).unwrap()
}

fn modified(nv: NvState, offset: usize, bytes: &[u8]) -> NvState {
    let mut flash = nv.flash().to_vec();
    flash[offset..offset + bytes.len()].copy_from_slice(bytes);
    NvState::new(flash, nv.secregs().to_vec()).unwrap()
}

#[test]
fn persisted_boot_preserves_updated_code_and_uses_its_header() {
    // A persisted image can have a different header/vector location than the original bundle.
    let nv = saved_image(0x200, 42);
    let mut machine = Machine::from_nv(nv.clone(), &Layout::EVO_7_1).unwrap();
    assert_eq!(machine.cpu.regs.pc, 0x100b_0300);
    assert_eq!(machine.nv_state().flash(), nv.flash());
    assert_eq!(machine.nv_state().secregs(), nv.secregs());
    assert_eq!(machine.run(20), StopReason::CycleLimit);
    assert_eq!(machine.cpu.regs.r[0], 42);
    let mut restarted = Machine::from_nv(machine.nv_state(), &Layout::EVO_7_1).unwrap();
    restarted.run(20);
    assert_eq!(restarted.cpu.regs.r[0], 42);
    assert_eq!(restarted.nv_state().flash(), nv.flash());
    assert_eq!(restarted.nv_state().secregs(), nv.secregs());
}

#[test]
fn persisted_state_identity_survives_data_writes_and_restart() {
    let mut machine = Machine::from_nv(saved_image(0x400, 7), &Layout::EVO_7_1).unwrap();
    machine.run(10);
    machine.bus.buffer_mut(FLASH_BUFFER).unwrap()[0x90000] = 0x44;
    let state = machine.save_state();
    let mut restarted = Machine::from_nv(machine.nv_state(), &Layout::EVO_7_1).unwrap();
    restarted.load_state(&state).unwrap();
    assert_eq!(restarted.cpu, machine.cpu);
    let mut other = Machine::from_nv(saved_image(0x400, 8), &Layout::EVO_7_1).unwrap();
    assert!(
        other.load_state(&state).is_err(),
        "other persisted code rejects the state"
    );
}

#[test]
fn corrupt_persisted_headers_and_vectors_are_rejected() {
    let slot = Layout::EVO_7_1.secure_slot_base as usize;
    let cases: Vec<(usize, Vec<u8>)> = vec![
        (slot, 0u32.to_le_bytes().to_vec()),
        (slot + 8, 16u16.to_le_bytes().to_vec()),
        (slot + 8, 0x401u16.to_le_bytes().to_vec()),
        (slot + 12, 4u32.to_le_bytes().to_vec()),
        (slot + 12, u32::MAX.to_le_bytes().to_vec()),
        (slot + 12, 0x400000u32.to_le_bytes().to_vec()),
        (slot + 0x400, 0xffff_fffcu32.to_le_bytes().to_vec()),
        (slot + 0x400, (SRAM_S + 3).to_le_bytes().to_vec()),
        (slot + 0x404, 0x100b_0500u32.to_le_bytes().to_vec()),
        (slot + 0x404, 0x100b_0601u32.to_le_bytes().to_vec()),
    ];
    for (offset, bytes) in cases {
        let nv = modified(saved_image(0x400, 42), offset, &bytes);
        assert!(
            Machine::from_nv(nv, &Layout::EVO_7_1).is_err(),
            "offset {offset:#x}, bytes {bytes:?}"
        );
    }
    assert!(Machine::from_nv(NvState::erased(), &Layout::EVO_7_1).is_err());
    for base in [u32::MAX, NOR_SIZE as u32 - 4] {
        let layout = Layout {
            secure_slot_base: base,
            ..Layout::EVO_7_1
        };
        assert!(Machine::from_nv(saved_image(0x400, 42), &layout).is_err());
    }
}
