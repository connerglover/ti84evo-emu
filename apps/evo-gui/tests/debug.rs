//! Debug-panel helpers: address parsing, disassembly lines, UART console.

use evo_gui::backend::DisasmLine;
use evo_gui::debug::{DEBUG_W, DebugTab, UART_CAP, UartConsole, format_disasm, parse_addr};

#[test]
fn constants() {
    assert_eq!(UART_CAP, 256 * 1024);
    assert_eq!(DEBUG_W, 420.0);
    assert_eq!(DebugTab::default(), DebugTab::Registers);
}

#[test]
fn parse_addr_cases() {
    assert_eq!(parse_addr("0x2000_0000"), Some(0x2000_0000));
    assert_eq!(parse_addr("20000000"), Some(0x2000_0000));
    assert_eq!(parse_addr(" 0X10 "), Some(0x10));
    assert_eq!(parse_addr("zz"), None);
    assert_eq!(parse_addr(""), None);
    assert_eq!(parse_addr("1_0000_0000"), None);
}

#[test]
fn parse_addr_rejects_signs_and_bare_prefixes() {
    assert_eq!(parse_addr("+10"), None);
    assert_eq!(parse_addr("0x+10"), None);
    assert_eq!(parse_addr("-1"), None);
    assert_eq!(parse_addr("0x"), None);
    assert_eq!(parse_addr("_"), None);
    assert_eq!(parse_addr("ffff_ffff"), Some(0xFFFF_FFFF));
    assert_eq!(parse_addr("0xAbC"), Some(0xABC));
}

#[test]
fn format_disasm_16_and_32() {
    let l16 = DisasmLine {
        addr: 0x1000_0200,
        len: 2,
        raw: 0xBF00,
        text: "nop".to_owned(),
    };
    assert_eq!(
        format_disasm(&l16, true, true),
        "\u{25CF}\u{25B6} 10000200  bf00       nop"
    );
    let l32 = DisasmLine {
        addr: 0x1000_0204,
        len: 4,
        raw: 0xF000_F800,
        text: "bl 0x10000208".to_owned(),
    };
    assert_eq!(
        format_disasm(&l32, false, false),
        "   10000204  f000 f800  bl 0x10000208"
    );
}

#[test]
fn uart_console_strips_cr_and_bounds() {
    let mut c = UartConsole::default();
    c.push(b"Booting TF-M\r\n");
    assert_eq!(c.text(), "Booting TF-M\n");

    c.push(&vec![b'a'; 300 * 1024]);
    c.push(b"END");
    assert_eq!(c.text().len(), UART_CAP);
    assert!(c.text().ends_with("END"));

    c.push(&[0xFF]);
    assert!(c.text().ends_with("END\u{FFFD}"));
}

#[test]
fn uart_console_joins_split_utf8() {
    let mut c = UartConsole::default();
    c.push(b"x");
    c.push(&[0xC3]);
    assert_eq!(c.text(), "x");
    c.push(&[0xA9]);
    assert!(c.text().ends_with("\u{e9}"));
    assert_eq!(c.text(), "x\u{e9}");
}

#[test]
fn uart_console_split_four_byte_char() {
    let mut c = UartConsole::default();
    let crab = "\u{1F980}".as_bytes();
    c.push(&crab[..1]);
    c.push(&crab[1..3]);
    assert_eq!(c.text(), "");
    c.push(&crab[3..]);
    assert_eq!(c.text(), "\u{1F980}");
}

#[test]
fn uart_console_cuts_at_char_boundary() {
    let mut c = UartConsole::default();
    // 2-byte chars: an odd cut would split one.
    let s = "\u{e9}".repeat(UART_CAP / 2 + 1);
    c.push(b"a");
    c.push(s.as_bytes());
    assert!(c.text().len() <= UART_CAP);
    assert!(c.text().chars().all(|ch| ch == '\u{e9}'));
}

#[test]
fn uart_console_clear() {
    let mut c = UartConsole::default();
    c.push(b"hello");
    c.push(&[0xC3]);
    c.clear();
    assert_eq!(c.text(), "");
    // The pending byte went too.
    c.push(&[0xA9]);
    assert_eq!(c.text(), "\u{FFFD}");
}
