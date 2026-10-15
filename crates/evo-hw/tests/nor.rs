//! NOR flash command engine and flash file (`docs/hardware/spif-nor.md` §6.2, §8.5-8.6).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use evo_hw::nor::{DataPhase, JEDEC_ID, NOR_SIZE, NorFlash, NvError, NvState, SECREGS_LEN};

const WEL: u8 = 0x02;

fn erased() -> Vec<u8> {
    vec![0xFF; NOR_SIZE]
}

/// Opens a read phase and pops `n` bytes.
fn read(nor: &mut NorFlash, op: u8, addr: u32, n: u32, flash: &mut [u8]) -> Vec<u8> {
    assert_eq!(nor.begin(op, addr, n, flash), DataPhase::Read);
    (0..n).map(|_| nor.read_byte(flash)).collect()
}

/// Runs a no-data command.
fn cmd(nor: &mut NorFlash, op: u8, addr: u32, flash: &mut [u8]) {
    assert_eq!(nor.begin(op, addr, 0, flash), DataPhase::None);
}

/// Opens a write phase and pushes `data`; returns the completion flag of each byte.
fn write(nor: &mut NorFlash, op: u8, addr: u32, data: &[u8], flash: &mut [u8]) -> Vec<bool> {
    assert_eq!(
        nor.begin(op, addr, data.len() as u32, flash),
        DataPhase::Write
    );
    data.iter().map(|&b| nor.write_byte(b, flash)).collect()
}

/// A flash-file path in the cargo test scratch directory that no other test (or test process)
/// uses, removed together with its `.tmp` sibling when dropped. Tests run in parallel, so a
/// name shared by two tests lets one delete or overwrite the other's file.
struct TmpFile(PathBuf);

impl TmpFile {
    /// `PATH.tmp`, where `NvState::write_file` writes first.
    fn tmp_sibling(&self) -> PathBuf {
        let mut name = self.0.clone().into_os_string();
        name.push(".tmp");
        PathBuf::from(name)
    }
}

impl std::ops::Deref for TmpFile {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TmpFile {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TmpFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.tmp_sibling());
    }
}

/// A fresh [`TmpFile`] named after `name`, the process id and a per-process counter.
fn tmp(name: &str) -> TmpFile {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let file = format!("nor-{name}-{}-{n}.evoflash", std::process::id());
    let p = TmpFile(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(file));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn jedec_and_status_defaults() {
    let mut f = erased();
    let mut nor = NorFlash::new();
    assert_eq!(
        read(&mut nor, 0x9F, 0, 4, &mut f),
        vec![0xEF, 0x40, 0x18, 0x00]
    );
    assert_eq!(JEDEC_ID, [0xEF, 0x40, 0x18]);
    assert_eq!(read(&mut nor, 0x05, 0, 2, &mut f), vec![0x00, 0x00]);
    assert_eq!(read(&mut nor, 0x35, 0, 1, &mut f), vec![0x02]);
    assert_eq!(read(&mut nor, 0x15, 0, 1, &mut f), vec![0x60]);
    assert_eq!((nor.sr1, nor.sr2, nor.sr3), (0x00, 0x02, 0x60));
    // Read phase exhausted: no more data.
    assert_eq!(nor.remaining(), 0);
    assert_eq!(nor.read_byte(&f), 0);
}

#[test]
fn wel_set_and_clear() {
    let mut f = erased();
    let mut nor = NorFlash::new();
    cmd(&mut nor, 0x06, 0, &mut f);
    assert_eq!(nor.sr1 & WEL, WEL);
    assert_eq!(read(&mut nor, 0x05, 0, 1, &mut f), vec![WEL]);
    cmd(&mut nor, 0x04, 0, &mut f);
    assert_eq!(nor.sr1 & WEL, 0);
}

#[test]
fn erase_needs_wel() {
    let mut f = vec![0x00; NOR_SIZE];
    let mut nor = NorFlash::new();
    cmd(&mut nor, 0x20, 0x3000, &mut f);
    assert!(f.iter().all(|&b| b == 0));
    cmd(&mut nor, 0x06, 0, &mut f);
    cmd(&mut nor, 0x20, 0x3000, &mut f);
    assert!(f[0x3000..0x4000].iter().all(|&b| b == 0xFF));
    assert_eq!(f[0x2FFF], 0);
    assert_eq!(f[0x4000], 0);
    assert_eq!(nor.sr1 & WEL, 0);
}

#[test]
fn erase_sizes_align_down() {
    let mut f = vec![0x00; NOR_SIZE];
    let mut nor = NorFlash::new();
    cmd(&mut nor, 0x06, 0, &mut f);
    cmd(&mut nor, 0xD8, 0x1_2345, &mut f);
    assert!(f[0x1_0000..0x2_0000].iter().all(|&b| b == 0xFF));
    assert_eq!(f[0x0_FFFF], 0);
    assert_eq!(f[0x2_0000], 0);

    let mut f = vec![0x00; NOR_SIZE];
    cmd(&mut nor, 0x06, 0, &mut f);
    cmd(&mut nor, 0x52, 0x1_2345, &mut f);
    assert!(f[0x1_0000..0x1_8000].iter().all(|&b| b == 0xFF));
    assert_eq!(f[0x0_FFFF], 0);
    assert_eq!(f[0x1_8000], 0);

    let mut f = vec![0x00; NOR_SIZE];
    cmd(&mut nor, 0x06, 0, &mut f);
    cmd(&mut nor, 0xC7, 0, &mut f);
    assert!(f.iter().all(|&b| b == 0xFF));
    assert_eq!(nor.sr1 & WEL, 0);
    let mut f = vec![0x00; NOR_SIZE];
    cmd(&mut nor, 0x06, 0, &mut f);
    cmd(&mut nor, 0x60, 0, &mut f);
    assert!(f.iter().all(|&b| b == 0xFF));
}

#[test]
fn program_is_and_and_wraps_page() {
    let mut f = erased();
    f[0xFE] = 0xF0;
    f[0xFF] = 0xF0;
    f[0x00] = 0xF0;
    f[0x01] = 0xF0;
    let mut nor = NorFlash::new();
    cmd(&mut nor, 0x06, 0, &mut f);
    let done = write(&mut nor, 0x02, 0xFE, &[0x0F; 4], &mut f);
    assert_eq!(done, vec![false, false, false, true]);
    assert_eq!(&f[0xFE..0x100], &[0x00, 0x00]);
    assert_eq!(&f[0x00..0x02], &[0x00, 0x00]);
    // The page wrap stays in page 0: page 1 is untouched.
    assert_eq!(f[0x100], 0xFF);
    assert_eq!(nor.sr1 & WEL, 0);
    assert_eq!(nor.remaining(), 0);
}

#[test]
fn program_needs_wel() {
    let mut f = erased();
    let mut nor = NorFlash::new();
    let done = write(&mut nor, 0x32, 0x40, &[0x00; 2], &mut f);
    assert_eq!(done, vec![false, true]);
    assert!(f.iter().all(|&b| b == 0xFF));
}

#[test]
fn reads_wrap_and_quad_ops_read_data() {
    let mut f = erased();
    f[NOR_SIZE - 1] = 0x11;
    f[0] = 0x22;
    let mut nor = NorFlash::new();
    for op in [0x03, 0x0B, 0x3B, 0x6B, 0xBB, 0xEB] {
        assert_eq!(
            read(&mut nor, op, (NOR_SIZE - 1) as u32, 2, &mut f),
            vec![0x11, 0x22]
        );
    }
}

#[test]
fn secreg_cycle() {
    let mut f = erased();
    let mut nor = NorFlash::new();
    cmd(&mut nor, 0x06, 0, &mut f);
    cmd(&mut nor, 0x44, 0x2000, &mut f);
    cmd(&mut nor, 0x06, 0, &mut f);
    let done = write(&mut nor, 0x42, 0x201C, &[0x42, 0x00, 0xDE, 0xC0], &mut f);
    assert_eq!(done.last(), Some(&true));
    assert_eq!(
        read(&mut nor, 0x48, 0x201C, 4, &mut f),
        vec![0x42, 0x00, 0xDE, 0xC0]
    );
    assert_eq!(
        &nor.secregs()[256 + 28..256 + 32],
        &[0x42, 0x00, 0xDE, 0xC0]
    );
    // The other registers and the flash are untouched.
    assert!(nor.secregs()[..256].iter().all(|&b| b == 0xFF));
    assert!(nor.secregs()[512..].iter().all(|&b| b == 0xFF));
    assert!(f.iter().all(|&b| b == 0xFF));
    // Erase without WEL keeps the data; with WEL it is gone.
    cmd(&mut nor, 0x44, 0x2000, &mut f);
    assert_eq!(nor.secregs()[256 + 28], 0x42);
    cmd(&mut nor, 0x06, 0, &mut f);
    cmd(&mut nor, 0x44, 0x2000, &mut f);
    assert!(nor.secregs().iter().all(|&b| b == 0xFF));
}

#[test]
fn secreg_read_out_of_range_is_ff() {
    let mut f = erased();
    let mut nor = NorFlash::new();
    assert_eq!(read(&mut nor, 0x48, 0x0000, 2, &mut f), vec![0xFF, 0xFF]);
    assert_eq!(read(&mut nor, 0x48, 0x4000, 2, &mut f), vec![0xFF, 0xFF]);
}

#[test]
fn secreg_program_needs_wel() {
    let mut f = erased();
    let mut nor = NorFlash::new();
    cmd(&mut nor, 0x44, 0x2000, &mut f);
    let done = write(&mut nor, 0x42, 0x2000, &[0x00; 4], &mut f);
    assert_eq!(done.last(), Some(&true));
    assert!(nor.secregs().iter().all(|&b| b == 0xFF));
    assert_eq!(nor.secregs().len(), SECREGS_LEN);
}

#[test]
fn wrsr_needs_wel() {
    let mut f = erased();
    let mut nor = NorFlash::new();
    assert_eq!(write(&mut nor, 0x01, 0, &[0xFC], &mut f), vec![true]);
    assert_eq!(write(&mut nor, 0x31, 0, &[0x00], &mut f), vec![true]);
    assert_eq!((nor.sr1, nor.sr2, nor.sr3), (0x00, 0x02, 0x60));

    // With WEL: SR1 keeps bits 0-1 as computed (WEL cleared after the write).
    cmd(&mut nor, 0x06, 0, &mut f);
    assert_eq!(write(&mut nor, 0x01, 0, &[0xFF], &mut f), vec![true]);
    assert_eq!(nor.sr1, 0xFC);
    cmd(&mut nor, 0x06, 0, &mut f);
    write(&mut nor, 0x31, 0, &[0x03], &mut f);
    assert_eq!(nor.sr2, 0x03);
    cmd(&mut nor, 0x06, 0, &mut f);
    write(&mut nor, 0x11, 0, &[0x00], &mut f);
    assert_eq!(nor.sr3, 0x00);
    assert_eq!(nor.sr1 & WEL, 0);
}

#[test]
fn read_phase_is_lazy() {
    let mut f = erased();
    f[0..3].copy_from_slice(&[1, 2, 3]);
    let mut nor = NorFlash::new();
    assert_eq!(nor.begin(0x03, 0, u32::MAX, &mut f), DataPhase::Read);
    assert_eq!(nor.remaining(), u32::MAX);
    let got: Vec<u8> = (0..3).map(|_| nor.read_byte(&f)).collect();
    assert_eq!(got, &f[0..3]);
    assert_eq!(nor.remaining(), u32::MAX - 3);
    nor.abort();
    assert_eq!(nor.remaining(), 0);
    assert_eq!(nor.read_byte(&f), 0);
}

#[test]
fn begin_aborts_open_phase_and_unknown_ops_have_no_data() {
    let mut f = erased();
    let mut nor = NorFlash::new();
    cmd(&mut nor, 0x06, 0, &mut f);
    assert_eq!(nor.begin(0x02, 0, 4, &mut f), DataPhase::Write);
    for op in [0xFF, 0x66, 0x99, 0xAB, 0xB9, 0x5A] {
        assert_eq!(nor.begin(op, 0, 16, &mut f), DataPhase::None);
        assert_eq!(nor.remaining(), 0);
    }
    assert!(!nor.write_byte(0x00, &mut f));
    assert!(f.iter().all(|&b| b == 0xFF));
    // A zero-length program opens nothing.
    assert_eq!(nor.begin(0x02, 0, 0, &mut f), DataPhase::None);
}

#[test]
fn short_flash_does_not_panic() {
    let mut f = vec![0x00; 0x100];
    let mut nor = NorFlash::new();
    cmd(&mut nor, 0x06, 0, &mut f);
    cmd(&mut nor, 0xD8, 0x00FF_0000, &mut f);
    assert_eq!(
        read(&mut nor, 0x03, 0x00FF_FFFF, 2, &mut f),
        vec![0xFF, 0xFF]
    );
    let mut empty: Vec<u8> = Vec::new();
    assert_eq!(read(&mut nor, 0x03, 0, 2, &mut empty), vec![0xFF, 0xFF]);
    cmd(&mut nor, 0x06, 0, &mut empty);
    write(&mut nor, 0x02, 0, &[0x00], &mut empty);
    cmd(&mut nor, 0x06, 0, &mut empty);
    cmd(&mut nor, 0x20, 0, &mut empty);
}

#[test]
fn set_secregs_checks_length() {
    let mut nor = NorFlash::new();
    nor.set_secregs(&[0x00; 10]);
    assert!(nor.secregs().iter().all(|&b| b == 0xFF));
    nor.set_secregs(&[0x5A; SECREGS_LEN]);
    assert!(nor.secregs().iter().all(|&b| b == 0x5A));
}

fn sample_nv() -> NvState {
    let mut flash = erased();
    flash[0] = 0x12;
    flash[NOR_SIZE - 1] = 0x34;
    let mut sec = vec![0xFF; SECREGS_LEN];
    sec[300] = 0x56;
    NvState::new(flash, sec).expect("valid lengths")
}

#[test]
fn nvstate_file_roundtrip() {
    let p = tmp("roundtrip");
    let nv = sample_nv();
    nv.write_file(&p).expect("write");
    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(&bytes[0..8], b"EVOFLASH");
    assert_eq!(&bytes[8..12], &1u32.to_le_bytes());
    assert_eq!(&bytes[12..16], &0x0100_0000u32.to_le_bytes());
    assert_eq!(bytes.len(), 16 + NOR_SIZE + SECREGS_LEN);
    // No temporary file left behind.
    assert!(!p.tmp_sibling().exists());

    let back = NvState::read_file(&p).expect("read");
    assert_eq!(back.flash(), nv.flash());
    assert_eq!(back.secregs(), nv.secregs());

    let erased = NvState::erased();
    assert_eq!(erased.flash().len(), NOR_SIZE);
    assert!(erased.flash().iter().all(|&b| b == 0xFF));
    assert_eq!(erased.secregs(), &[0xFF; SECREGS_LEN][..]);
}

/// Writes `bytes` to a fresh file, expects `read_file` to fail and the file to be unchanged.
fn rejects(name: &str, bytes: &[u8]) -> NvError {
    let p = tmp(name);
    std::fs::write(&p, bytes).unwrap();
    let err = match NvState::read_file(&p) {
        Ok(_) => panic!("{name}: accepted"),
        Err(e) => e,
    };
    assert_eq!(std::fs::read(&p).unwrap(), bytes, "{name}: file changed");
    err
}

fn good_file_bytes() -> Vec<u8> {
    let p = tmp("good-source");
    sample_nv().write_file(&p).unwrap();
    std::fs::read(&p).unwrap()
}

#[test]
fn nvstate_rejects_bad_magic() {
    let mut b = good_file_bytes();
    b[0] = b'X';
    assert!(matches!(rejects("bad-magic", &b), NvError::BadMagic));
    assert!(matches!(rejects("garbage", b"hello"), NvError::BadMagic));
}

#[test]
fn nvstate_rejects_version_2() {
    let mut b = good_file_bytes();
    b[8..12].copy_from_slice(&2u32.to_le_bytes());
    assert!(matches!(rejects("version-2", &b), NvError::Version(2)));
}

#[test]
fn nvstate_rejects_truncated() {
    let b = good_file_bytes();
    let e = rejects("truncated", &b[..b.len() - 1]);
    assert!(
        matches!(e, NvError::Size { expected, found } if expected == b.len() && found == b.len() - 1),
        "{e:?}"
    );
    assert!(matches!(
        rejects("header-only", &b[..12]),
        NvError::Size { .. }
    ));
    // A wrong flash-length field.
    let mut w = b.clone();
    w[12..16].copy_from_slice(&0x0080_0000u32.to_le_bytes());
    assert!(matches!(rejects("wrong-len", &w), NvError::Size { .. }));
    // Trailing bytes are also a size error.
    let mut long = b.clone();
    long.push(0);
    assert!(matches!(rejects("too-long", &long), NvError::Size { .. }));
}

#[test]
fn nvstate_rejects_oversized_file_by_its_size() {
    // A valid header on a file 256 MiB too long: rejected from its metadata, with the size
    // the file has.
    let p = tmp("oversized");
    let good = good_file_bytes();
    std::fs::write(&p, &good[..64]).unwrap();
    let len = good.len() + (256 << 20);
    std::fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_len(len as u64)
        .unwrap();
    let e = NvState::read_file(&p).expect_err("accepted");
    assert!(
        matches!(e, NvError::Size { expected, found } if expected == good.len() && found == len),
        "{e:?}"
    );
}

#[test]
fn nvstate_missing_file_is_io() {
    let p = tmp("missing");
    assert!(matches!(NvState::read_file(&p), Err(NvError::Io(_))));
}

#[test]
fn nvstate_new_checks_lengths() {
    assert!(matches!(
        NvState::new(vec![0xFF; 8 << 20], vec![0xFF; SECREGS_LEN]),
        Err(NvError::Size {
            expected: NOR_SIZE,
            found: 0x80_0000
        })
    ));
    assert!(matches!(
        NvState::new(erased(), vec![0xFF; 256]),
        Err(NvError::Size {
            expected: SECREGS_LEN,
            found: 256
        })
    ));
    assert!(NvState::new(erased(), vec![0xFF; SECREGS_LEN]).is_ok());
}

#[test]
fn serde_skips_secregs() {
    let mut f = erased();
    let mut nor = NorFlash::new();
    let mut sec = vec![0xFF; SECREGS_LEN];
    sec[0] = 0x12;
    nor.set_secregs(&sec);
    nor.sr1 = 0x1C;
    assert_eq!(nor.begin(0x03, 0x100, 50, &mut f), DataPhase::Read);
    nor.read_byte(&f);
    let bytes = postcard::to_stdvec(&nor).unwrap();
    let back: NorFlash = postcard::from_bytes(&bytes).unwrap();
    assert_eq!((back.sr1, back.sr2, back.sr3), (nor.sr1, nor.sr2, nor.sr3));
    assert_eq!(back.remaining(), 49);
    assert!(back.secregs().is_empty());
}

#[test]
fn secreg_ops_after_restore_without_set_secregs_do_not_panic() {
    let mut f = erased();
    let bytes = postcard::to_stdvec(&NorFlash::new()).unwrap();
    let mut nor: NorFlash = postcard::from_bytes(&bytes).unwrap();
    assert!(nor.secregs().is_empty());
    // 0x48 read gives ERASED.
    assert_eq!(read(&mut nor, 0x48, 0x1000, 4, &mut f), vec![0xFF; 4]);
    // 0x44 erase with WEL is a no-op and still clears WEL.
    cmd(&mut nor, 0x06, 0, &mut f);
    cmd(&mut nor, 0x44, 0x2000, &mut f);
    assert_eq!(nor.sr1 & WEL, 0);
    // 0x42 program with WEL is a no-op and still completes and clears WEL.
    cmd(&mut nor, 0x06, 0, &mut f);
    assert_eq!(nor.begin(0x42, 0x3000, 2, &mut f), DataPhase::Write);
    assert!(!nor.write_byte(0x00, &mut f));
    assert!(nor.write_byte(0x00, &mut f));
    assert_eq!(nor.sr1 & WEL, 0);
    assert!(nor.secregs().is_empty());
    assert!(f.iter().all(|&b| b == 0xFF));
}
