//! `evo-cli inspect`: what a `.84b2` holds and where its images go in flash.

use std::io::{self, Write};

use armv8m::disasm;
use evo_image::{Bundle, Layout, McubootImage, Package};

use crate::score::{self, Candidates, SECURE_ALIAS};

/// Names of the first 16 words of an Armv8-M vector table.
const VECTOR_NAMES: [&str; 16] = [
    "SP",
    "Reset",
    "NMI",
    "HardFault",
    "MemManage",
    "BusFault",
    "UsageFault",
    "SecureFault",
    "Reserved",
    "Reserved",
    "Reserved",
    "SVCall",
    "DebugMonitor",
    "Reserved",
    "PendSV",
    "SysTick",
];

/// Instructions listed from each reset handler.
const RESET_LISTING: usize = 8;

/// Rows of each image's score table.
const TOP: usize = 5;

/// Print `bundle`'s metadata, its image table and, with images 0 and 1
/// placed per `layout`, their vector tables and reset handlers. With
/// `score_bases`, also rank candidate slot bases for both images.
pub fn run(
    bundle: &Bundle,
    layout: &Layout,
    score_bases: bool,
    out: &mut impl Write,
) -> io::Result<()> {
    let md = &bundle.metadata;
    writeln!(
        out,
        "bundle: build {}, device {}, prodid {}, kind {}",
        md.build_version, md.target_device, md.prodid, md.kind
    )?;
    let pkg = &bundle.package;
    image_table(pkg, out)?;

    writeln!(
        out,
        "\nlayout: image 0 slot {:#010x}, image 1 slot {:#010x} \
         (flash offsets = Non-secure alias; Secure alias = offset | {SECURE_ALIAS:#010x})",
        layout.secure_slot_base, layout.ns_slot_base
    )?;
    let bases = [layout.secure_slot_base, layout.ns_slot_base];
    for (index, (image, &base)) in pkg.images.iter().zip(&bases).enumerate() {
        vector_table(index, slot(pkg, image), image, base, out)?;
    }

    if score_bases {
        for (index, image) in pkg.images.iter().take(2).enumerate() {
            scores(index, slot(pkg, image), image, out)?;
        }
    }
    Ok(())
}

/// An image's header and payload as they sit in its flash slot. Empty for a
/// `Package` whose image table does not match its bytes.
fn slot<'a>(pkg: &'a Package, image: &McubootImage) -> &'a [u8] {
    pkg.bytes
        .get(image.pkg_offset..image.body.end)
        .unwrap_or_default()
}

fn image_table(pkg: &Package, out: &mut impl Write) -> io::Result<()> {
    writeln!(
        out,
        "package: {:#x} bytes, {} MCUboot images",
        pkg.bytes.len(),
        pkg.images.len()
    )?;
    writeln!(
        out,
        "   #  offset      header  payload     slot+TLVs   version"
    )?;
    for (i, img) in pkg.images.iter().enumerate() {
        let (major, minor, rev, build) = img.version;
        let with_tlvs = img.end_with_tlvs(&pkg.bytes).saturating_sub(img.pkg_offset);
        writeln!(
            out,
            "  {i:2}  {:#010x}  {:<6}  {:#010x}  {with_tlvs:#010x}  {major}.{minor}.{rev}+{build}",
            img.pkg_offset,
            format!("{:#x}", img.header_size),
            img.image_size,
        )?;
    }
    Ok(())
}

/// Slot offset that `addr` (either alias) names when the slot is at `base`.
fn slot_offset(addr: u32, base: u32, slot_len: usize) -> Option<usize> {
    let flash = if addr & SECURE_ALIAS != 0 && base & SECURE_ALIAS == 0 {
        addr - SECURE_ALIAS
    } else {
        addr
    };
    let off = usize::try_from(flash.checked_sub(base)?).ok()?;
    (off < slot_len).then_some(off)
}

/// `"slot+0x…  <first instruction>"` for a handler address, or why not.
fn describe_target(slot: &[u8], base: u32, word: u32) -> String {
    if word == 0 {
        return String::new();
    }
    if word & 1 == 0 {
        return "(even: not a Thumb address)".into();
    }
    let addr = word & !1;
    match slot_offset(addr, base, slot.len()) {
        Some(off) => match score::decode_at(slot, off) {
            Some((instr, len)) => format!("slot+{off:#x}  {}", disasm(&instr, addr, len)),
            None => format!("slot+{off:#x}  (runs past the slot)"),
        },
        None => "(outside the slot)".into(),
    }
}

fn vector_table(
    index: usize,
    slot: &[u8],
    image: &McubootImage,
    base: u32,
    out: &mut impl Write,
) -> io::Result<()> {
    let hdr = usize::from(image.header_size);
    let table = base.wrapping_add(u32::from(image.header_size));
    writeln!(
        out,
        "\nimage {index}: first {} words at slot+{hdr:#x} = {table:#010x} (Secure alias {:#010x})",
        VECTOR_NAMES.len(),
        table | SECURE_ALIAS
    )?;
    for (i, name) in VECTOR_NAMES.iter().enumerate() {
        let Some(word) = score::word_at(slot, hdr + 4 * i) else {
            break;
        };
        let target = if i == 0 {
            String::new()
        } else {
            describe_target(slot, base, word)
        };
        writeln!(out, "  {i:3} {name:<12} {word:#010x}  {target}")?;
    }

    let Some(reset) = score::word_at(slot, hdr + 4) else {
        return Ok(());
    };
    let addr = reset & !1;
    let Some(mut off) = slot_offset(addr, base, slot.len()) else {
        writeln!(
            out,
            "  reset vector {reset:#010x} is outside the slot under this layout"
        )?;
        return Ok(());
    };
    writeln!(out, "  reset handler:")?;
    let mut pc = addr;
    for _ in 0..RESET_LISTING {
        let Some((instr, len)) = score::decode_at(slot, off) else {
            break;
        };
        writeln!(out, "    {pc:08x}:  {}", disasm(&instr, pc, len))?;
        off += usize::from(len);
        pc = pc.wrapping_add(u32::from(len));
    }
    Ok(())
}

fn scores(index: usize, slot: &[u8], image: &McubootImage, out: &mut impl Write) -> io::Result<()> {
    let hdr = usize::from(image.header_size);
    // Image 0's words at the payload start are a vector table; image 1's
    // stop looking like one after the reset vector, so criterion 1 is off.
    let vectors = index == 0;
    let cands = Candidates::WIDE;
    let ranked = score::score_bases(slot, hdr, cands, vectors);

    writeln!(
        out,
        "\nimage {index}: slot base scores, bases {:#010x}..{:#010x} step {:#x}, \
         each tried at base and base | {SECURE_ALIAS:#010x}",
        cands.start, cands.end, cands.step
    )?;
    if vectors {
        writeln!(
            out,
            "  criterion 1 (x10): of {} vector-table entries, those landing in the slot that decode",
            score::vector_entries(slot, hdr).len()
        )?;
    } else {
        writeln!(
            out,
            "  criterion 1: not scored (no vector table at the payload start)"
        )?;
    }
    writeln!(
        out,
        "  criterion 2 (x1): literals equal to the address of one of {} strings",
        score::string_starts(slot, hdr).len()
    )?;
    writeln!(
        out,
        "  criterion 3 (x1): BLs to a PUSH in the first 64 KiB (PC-relative: the same for every base)"
    )?;
    writeln!(
        out,
        "  rank  base         total  vectors NS/S  strings NS/S  distinct  BL->PUSH"
    )?;
    for (rank, s) in ranked.iter().take(TOP).enumerate() {
        let v = if vectors {
            format!("{}/{}", s.vectors.ns, s.vectors.s)
        } else {
            "-".into()
        };
        writeln!(
            out,
            "  {:4}  {:#010x}  {:5}  {v:>12}  {:>12}  {:8}  {:8}",
            rank + 1,
            s.base,
            s.total(),
            format!("{}/{}", s.strings.ns, s.strings.s),
            s.distinct_strings,
            s.bl_prologues
        )?;
    }

    let Some(v) = score::verdict(&ranked) else {
        return writeln!(out, "image {index}: STOP no candidates");
    };
    let call = if v.accepted { "ACCEPT" } else { "STOP" };
    let hits = v.winner.strings.total();
    let rival = match v.runner_up {
        Some(r) => format!("{} at runner-up {:#010x}", r.strings.total(), r.base),
        None => "no other candidate".into(),
    };
    let margin = match v.margin() {
        Some(m) => format!("{m:.1}x"),
        None => "unbounded".into(),
    };
    writeln!(
        out,
        "image {index}: {call} {:#010x}: string hits {hits} vs {rival} (margin {margin}, need 2x); \
         {} different strings vs at most {} elsewhere; vector entries {}; total {}",
        v.winner.base,
        v.winner.distinct_strings,
        v.best_other_distinct,
        v.winner.vectors.total(),
        v.winner.total()
    )?;
    if let Some(reset) = score::word_at(slot, hdr + 4) {
        writeln!(
            out,
            "  reset vector {reset:#010x} at this base: {}",
            describe_target(slot, v.winner.base, reset)
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use evo_image::{Metadata, Package};

    const MAGIC: u32 = 0x96f3_b83d;
    const LAYOUT: Layout = Layout {
        secure_slot_base: 0x1_0000,
        ns_slot_base: 0x2_0000,
    };
    const S_SLOT: u32 = 0x1_0000 | 0x1000_0000;
    const NS_SLOT: u32 = 0x2_0000;

    fn put(buf: &mut [u8], off: usize, v: u32) {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn halves(buf: &mut [u8], off: usize, hs: &[u16]) {
        for (i, h) in hs.iter().enumerate() {
            buf[off + 2 * i..off + 2 * i + 2].copy_from_slice(&h.to_le_bytes());
        }
    }

    /// An MCUboot image: header (magic, sizes, version) then `body`.
    fn mcuboot(hdr: u16, body: &[u8], build: u32) -> Vec<u8> {
        let mut img = vec![0u8; usize::from(hdr)];
        put(&mut img, 0, MAGIC);
        img[8..10].copy_from_slice(&hdr.to_le_bytes());
        put(&mut img, 12, body.len() as u32);
        img[20] = 7;
        img[21] = 1;
        put(&mut img, 24, build);
        img.extend_from_slice(body);
        img
    }

    /// `push {r4, lr}; movs r0, #1; adds r0, #2; nop; movs r1, r0; bx lr;
    /// nop; b .; b .`: nine instructions, the listing shows eight.
    const HANDLER: [u16; 9] = [
        0xB510, 0x2001, 0x3002, 0xBF00, 0x0001, 0x4770, 0xBF00, 0xE7FE, 0xE7FE,
    ];

    fn bundle() -> Bundle {
        // Image 0: secure, header 0x400, linked at the Secure alias.
        let mut secure = vec![0u8; 0x2000];
        put(&mut secure, 0, 0x3000_0800); // SP
        put(&mut secure, 4, (S_SLOT + 0x500) | 1); // reset: slot+0x500 = body+0x100
        put(&mut secure, 8, (S_SLOT + 0x540) | 1); // NMI
        put(&mut secure, 0x40, 0x4770_4770); // end of the table
        halves(&mut secure, 0x100, &HANDLER);
        halves(&mut secure, 0x140, &[0x4770]);
        secure[0x1000..0x100D].copy_from_slice(b"secure world\0");
        put(&mut secure, 0x1800, S_SLOT + 0x1400);
        put(&mut secure, 0x1804, S_SLOT + 0x1400);

        // Image 1: non-secure, header 0x200, linked at the NS alias.
        let mut ns = vec![0u8; 0x1000];
        put(&mut ns, 0, 0x2000_4000);
        put(&mut ns, 4, (NS_SLOT + 0x280) | 1); // reset: slot+0x280 = body+0x80
        halves(&mut ns, 0x80, &HANDLER);
        ns[0x400..0x40D].copy_from_slice(b"normal world\0");
        for i in 0..3 {
            put(&mut ns, 0x800 + 4 * i, NS_SLOT + 0x600);
        }

        let mut bytes = mcuboot(0x400, &secure, 4413);
        bytes.resize(0x4000, 0xFF);
        bytes.extend(mcuboot(0x200, &ns, 4421));
        Bundle {
            metadata: Metadata {
                build_version: "9.8.7.6543".into(),
                target_device: "84XX".into(),
                prodid: 42,
                kind: "RESTORE".into(),
            },
            package: Package::parse(bytes).expect("synthetic package"),
        }
    }

    fn inspect(score_bases: bool) -> String {
        let mut out = Vec::new();
        run(&bundle(), &LAYOUT, score_bases, &mut out).expect("write to a Vec");
        String::from_utf8(out).expect("utf-8")
    }

    fn line_with<'a>(text: &'a str, needle: &str) -> &'a str {
        text.lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no line containing {needle:?} in:\n{text}"))
    }

    #[test]
    fn inspect_prints_metadata_images_and_vectors() {
        let text = inspect(false);
        assert!(text.contains("9.8.7.6543"), "{text}");
        assert!(text.contains("84XX"), "{text}");

        // Image table: offset, header, payload, version.
        let row = line_with(&text, "0x00004000");
        assert!(row.contains("0x200"), "{row}");
        assert!(row.contains("0x00001000"), "{row}");
        assert!(row.contains("7.1.0+4421"), "{row}");

        // Vector table of image 0 under the layout.
        let reset = line_with(&text, "0x10010501");
        assert!(reset.contains("Reset"), "{reset}");
        assert!(reset.contains("slot+0x500"), "{reset}");
        assert!(reset.contains("push {r4, lr}"), "{reset}");
        assert!(line_with(&text, "0x10010541").contains("NMI"));
        assert!(line_with(&text, "0x30000800").contains("SP"));

        // Reset handlers: eight instructions each, at the vector's address.
        for start in [0x1001_0500u32, 0x2_0280] {
            let first = format!("{start:08x}:");
            let at = text
                .find(&first)
                .unwrap_or_else(|| panic!("{first} in:\n{text}"));
            let listing: Vec<&str> = text[at..].lines().take(9).collect();
            assert!(listing[0].ends_with("push {r4, lr}"), "{listing:?}");
            assert!(listing[1].ends_with("movs r0, #1"), "{listing:?}");
            assert!(
                listing[7].contains(&format!("{:08x}:", start + 14)),
                "{listing:?}"
            );
            assert!(
                listing[7].ends_with(&format!("b {:#x}", start + 14)),
                "{listing:?}"
            );
            let ninth = format!("{:08x}:", start + 16);
            assert!(
                listing.get(8).is_none_or(|l| !l.contains(&ninth)),
                "{listing:?}"
            );
        }
        assert!(!text.contains("score"), "no scoring unless asked:\n{text}");
    }

    #[test]
    fn inspect_scores_bases_on_request() {
        let text = inspect(true);
        let secure = line_with(&text, "image 0: ACCEPT");
        assert!(secure.contains("0x00010000"), "{secure}");
        let ns = line_with(&text, "image 1: ACCEPT");
        assert!(ns.contains("0x00020000"), "{ns}");
        assert!(
            secure.contains("1 different strings vs at most 0"),
            "{secure}"
        );
        // The top row of each table is the winner, with its breakdown.
        let top = line_with(&text, "   1  0x00010000");
        assert!(top.contains("0/2"), "string hits NS/S: {top}");
    }

    #[test]
    fn reset_outside_the_slot_is_reported_not_disassembled() {
        let elsewhere = Layout {
            secure_slot_base: 0x40_0000,
            ns_slot_base: 0x50_0000,
        };
        let mut out = Vec::new();
        run(&bundle(), &elsewhere, false, &mut out).expect("write");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(
            line_with(&text, "0x10010501").contains("outside the slot"),
            "{text}"
        );
        assert!(!text.contains("push {r4, lr}"), "{text}");
    }
}
