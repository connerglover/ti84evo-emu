//! TRNG and SHA-256 engine models (`docs/hardware/crypto.md`).

use evo_hw::dev::crypto::{Sha256Engine, TRNG_SEED, Trng};
use evo_hw::{DevHarness, Device, MemoryMap, ResetKind, StateError};

const RNG_IMR: u32 = 0x100;
const RNG_ISR: u32 = 0x104;
const RNG_ICR: u32 = 0x108;
const TRNG_CONFIG: u32 = 0x10C;
const EHR_DATA0: u32 = 0x114;
const RND_SOURCE_ENABLE: u32 = 0x12C;
const SAMPLE_CNT1: u32 = 0x130;
const TRNG_DEBUG_CONTROL: u32 = 0x138;

const SHA_CTRL: u32 = 0x00;
const SHA_W0: u32 = 0x10;
const SHA_H0: u32 = 0x60;

/// 24 EHR words: four batches of the six EHR_DATA registers, read in order.
fn ehr_words(h: &mut DevHarness, t: &mut Trng) -> Vec<u32> {
    (0..24)
        .map(|i| h.read(t, EHR_DATA0 + 4 * (i % 6), 4))
        .collect()
}

#[test]
fn trng_valid_follows_enable() {
    let mut h = DevHarness::new();
    let mut t = Trng::new(TRNG_SEED);
    assert_eq!(h.read(&mut t, RNG_ISR, 4), 0);
    h.write(&mut t, RND_SOURCE_ENABLE, 4, 1);
    assert_eq!(h.read(&mut t, RNG_ISR, 4), 1);
    h.write(&mut t, RND_SOURCE_ENABLE, 4, 0);
    assert_eq!(h.read(&mut t, RNG_ISR, 4), 0);
    assert_eq!(h.irq, 0, "IRQ 26 is never asserted");
    assert!(h.log.is_empty());
}

#[test]
fn trng_storage_reads_back() {
    let mut h = DevHarness::new();
    let mut t = Trng::new(TRNG_SEED);
    h.write(&mut t, SAMPLE_CNT1, 4, 0xC8);
    h.write(&mut t, TRNG_DEBUG_CONTROL, 4, 0xA);
    h.write(&mut t, RNG_IMR, 4, 0xFFFF_FFC5);
    h.write(&mut t, TRNG_CONFIG, 4, 2);
    assert_eq!(h.read(&mut t, SAMPLE_CNT1, 4), 0xC8);
    assert_eq!(h.read(&mut t, TRNG_DEBUG_CONTROL, 4), 0xA);
    assert_eq!(h.read(&mut t, RNG_IMR, 4), 0xFFFF_FFC5);
    assert_eq!(h.read(&mut t, TRNG_CONFIG, 4), 2);
    assert!(h.log.is_empty());
}

#[test]
fn trng_not_constant_but_deterministic() {
    let mut h = DevHarness::new();
    let mut a = Trng::new(TRNG_SEED);
    let mut b = Trng::new(TRNG_SEED);
    let wa = ehr_words(&mut h, &mut a);
    let wb = ehr_words(&mut h, &mut b);
    assert!(wa.iter().any(|&w| w != wa[0]), "EHR output is constant");
    assert_eq!(wa, wb);
    // A different seed gives a different stream.
    let mut c = Trng::new(1);
    assert_ne!(ehr_words(&mut h, &mut c), wa);
}

#[test]
fn trng_icr_never_sets_errors() {
    let mut h = DevHarness::new();
    let mut t = Trng::new(TRNG_SEED);
    h.write(&mut t, RNG_ICR, 4, 0xFFFF_FFFF);
    assert_eq!(h.read(&mut t, RNG_ISR, 4), 0);
    h.write(&mut t, RND_SOURCE_ENABLE, 4, 1);
    for v in [4, 8, 0xFFFF_FFFF] {
        h.write(&mut t, RNG_ICR, 4, v);
        assert_eq!(h.read(&mut t, RNG_ISR, 4), 1, "ICR {v:#x}");
    }
    assert!(h.log.is_empty());
}

#[test]
fn trng_system_reset_keeps_generator() {
    let mut h = DevHarness::new();
    let mut t = Trng::new(TRNG_SEED);
    let first = ehr_words(&mut h, &mut t);
    h.write(&mut t, SAMPLE_CNT1, 4, 0xC8);
    h.write(&mut t, RND_SOURCE_ENABLE, 4, 1);
    h.reset(&mut t, ResetKind::System);
    assert_eq!(h.read(&mut t, SAMPLE_CNT1, 4), 0);
    assert_eq!(h.read(&mut t, RNG_ISR, 4), 0);
    // The stream continues: not the same words as a fresh generator.
    assert_ne!(ehr_words(&mut h, &mut t), first);
    // A PowerOn reset restarts the stream from the seed.
    h.reset(&mut t, ResetKind::PowerOn);
    assert_eq!(ehr_words(&mut h, &mut t), first);
}

#[test]
fn trng_unknown_offset_logged() {
    let mut h = DevHarness::new();
    let mut t = Trng::new(TRNG_SEED);
    assert_eq!(h.read(&mut t, 0x1C0, 4), 0);
    h.write(&mut t, 0x0, 4, 1);
    assert_eq!(h.read(&mut t, SAMPLE_CNT1, 1), 0);
    assert_eq!(h.log.len(), 3);
}

fn write_block(h: &mut DevHarness, s: &mut Sha256Engine, words: &[u32]) {
    for (i, &w) in words.iter().enumerate() {
        h.write(s, SHA_W0 + 4 * i as u32, 4, w);
    }
}

fn digest(h: &mut DevHarness, s: &mut Sha256Engine) -> [u32; 8] {
    std::array::from_fn(|i| h.read(s, SHA_H0 + 4 * i as u32, 4))
}

#[test]
fn sha_abc() {
    let mut h = DevHarness::new();
    let mut s = Sha256Engine::new();
    let mut w = [0u32; 16];
    w[0] = 0x6162_6380;
    w[15] = 0x18;
    write_block(&mut h, &mut s, &w);
    h.write(&mut s, SHA_CTRL, 4, 0x0E);
    let want = [
        0xba7816bf, 0x8f01cfea, 0x414140de, 0x5dae2223, 0xb00361a3, 0x96177a9c, 0xb410ff61,
        0xf20015ad,
    ];
    assert_eq!(digest(&mut h, &mut s), want);
    assert_eq!(s.state(), want);
    assert_eq!(h.irq, 0, "IRQ 23 is never asserted");
    assert!(h.log.is_empty());
}

#[test]
fn sha_two_blocks() {
    let msg = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    let mut padded = msg.to_vec();
    padded.push(0x80);
    padded.resize(120, 0);
    padded.extend_from_slice(&((msg.len() as u64) * 8).to_be_bytes());
    assert_eq!(padded.len(), 128);
    let words: Vec<u32> = padded
        .chunks(4)
        .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let mut h = DevHarness::new();
    let mut s = Sha256Engine::new();
    write_block(&mut h, &mut s, &words[..16]);
    h.write(&mut s, SHA_CTRL, 4, 0x0E);
    write_block(&mut h, &mut s, &words[16..]);
    h.write(&mut s, SHA_CTRL, 4, 0x0A);
    assert_eq!(
        digest(&mut h, &mut s),
        [
            0x248d6a61, 0xd20638b8, 0xe5c02693, 0x0c3e6039, 0xa33ce459, 0x64ff2167, 0xf6ecedd4,
            0x19db06c1
        ]
    );
}

#[test]
fn sha_ctrl_not_busy() {
    let mut h = DevHarness::new();
    let mut s = Sha256Engine::new();
    h.write(&mut s, SHA_CTRL, 4, 0x0E);
    assert_eq!(h.read(&mut s, SHA_CTRL, 4), 0x0E);
    // BUSY (bit 11) never reads 1, even if the guest writes it.
    h.write(&mut s, SHA_CTRL, 4, 0x0800);
    assert_eq!(h.read(&mut s, SHA_CTRL, 4), 0);
}

#[test]
fn sha_ctrl_without_start_does_not_hash() {
    let mut h = DevHarness::new();
    let mut s = Sha256Engine::new();
    h.write(&mut s, SHA_W0, 4, 0x6162_6380);
    h.write(&mut s, SHA_CTRL, 4, 0x0C);
    assert_eq!(s.state(), [0; 8]);
}

#[test]
fn sha_reset_clears() {
    let mut h = DevHarness::new();
    let mut s = Sha256Engine::new();
    h.write(&mut s, SHA_W0, 4, 0x6162_6380);
    h.write(&mut s, SHA_CTRL, 4, 0x0E);
    h.reset(&mut s, ResetKind::System);
    assert_eq!(s.state(), [0; 8]);
    assert_eq!(h.read(&mut s, SHA_CTRL, 4), 0);
    assert_eq!(h.read(&mut s, SHA_W0, 4), 0);
}

#[test]
fn sha_unknown_offset_logged() {
    let mut h = DevHarness::new();
    let mut s = Sha256Engine::new();
    assert_eq!(h.read(&mut s, 0x04, 4), 0);
    h.write(&mut s, 0x80, 4, 1);
    h.write(&mut s, SHA_CTRL, 1, 0x0E);
    assert_eq!(s.state(), [0; 8]);
    assert_eq!(h.log.len(), 3);
}

#[test]
fn state_roundtrip() {
    let mut h = DevHarness::new();

    let mut t = Trng::new(TRNG_SEED);
    h.write(&mut t, SAMPLE_CNT1, 4, 0xC8);
    h.write(&mut t, RND_SOURCE_ENABLE, 4, 1);
    ehr_words(&mut h, &mut t);
    let st = t.save();
    let mut t2 = Trng::new(1);
    t2.load(&st).unwrap();
    assert_eq!(h.read(&mut t2, SAMPLE_CNT1, 4), 0xC8);
    assert_eq!(h.read(&mut t2, RNG_ISR, 4), 1);
    assert_eq!(ehr_words(&mut h, &mut t2), ehr_words(&mut h, &mut t));
    assert!(t2.load(&[0xFF; 3]).is_err());
    assert!(matches!(
        t2.load(&[st.clone(), vec![0]].concat()),
        Err(StateError::Mismatch(_))
    ));

    let mut s = Sha256Engine::new();
    h.write(&mut s, SHA_W0, 4, 0x6162_6380);
    h.write(&mut s, SHA_CTRL, 4, 0x0E);
    let ss = s.save();
    let mut s2 = Sha256Engine::new();
    s2.load(&ss).unwrap();
    assert_eq!(s2.state(), s.state());
    assert_eq!(h.read(&mut s2, SHA_CTRL, 4), 0x0E);
    assert_eq!(h.read(&mut s2, SHA_W0, 4), 0x6162_6380);
    assert!(s2.load(&[0xFF; 3]).is_err());
    assert_eq!(s2.state(), s.state());
}

#[test]
fn soc_attaches_crypto() {
    let mut map = MemoryMap::new();
    evo_hw::soc::attach(&mut map);
    assert!(map.device::<Trng>("trng").is_some());
    assert!(map.device::<Sha256Engine>("sha").is_some());
}
