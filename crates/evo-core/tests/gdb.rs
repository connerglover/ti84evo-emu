//! The GDB remote stub, driven by an in-process client that speaks raw RSP packets over TCP.
//!
//! Each test runs `serve` on the test thread and the client on another; a client assertion
//! failure drops the connection, which ends `serve`, and is then re-raised.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use evo_core::gdb::serve;
use evo_core::{Machine, StopReason};
use evo_image::testutil::secure_image_bundle;
use evo_image::{Bundle, Layout};

/// `VTOR_S` for `Layout::EVO_7_1`: Secure alias of slot 0x000B_0000 plus the 0x400 header.
const VTOR: u32 = 0x100B_0400;
/// Payload offset of the code.
const CODE: u32 = 0x200;
const SP_INIT: u32 = 0x3000_0800;

const SYST_CSR: u32 = 0xE000_E010;
const SYST_RVR: u32 = 0xE000_E014;
const COUNTFLAG: u32 = 1 << 16;

/// `adds r0, #1 ; adds r1, #2 ; subs r2, #1 ; nop ; nop ; b <start>`
const LOOP: &[u16] = &[0x3001, 0x3102, 0x3A01, 0xBF00, 0xBF00, 0xE7F9];
/// `wfi ; adds r0, #1 ; b <start>`, with nothing that could wake the WFI.
const SLEEP: &[u16] = &[0xBF30, 0x3001, 0xE7FC];

/// Address of payload offset `off` (Secure alias).
fn addr(off: u32) -> u32 {
    VTOR + off
}

/// Boot a synthetic bundle whose secure reset handler is `code`, at payload offset `CODE`.
fn boot(code: &[u16]) -> Machine {
    let mut p = vec![0u8; 0x400];
    p[0..4].copy_from_slice(&SP_INIT.to_le_bytes());
    p[4..8].copy_from_slice(&(addr(CODE) | 1).to_le_bytes());
    for (i, hw) in code.iter().enumerate() {
        let o = CODE as usize + 2 * i;
        p[o..o + 2].copy_from_slice(&hw.to_le_bytes());
    }
    let b = Bundle::from_bytes(&secure_image_bundle(p)).expect("synthetic bundle");
    Machine::from_bundle(&b, &Layout::EVO_7_1).expect("boot")
}

/// A register or memory word as RSP hex (target byte order: little-endian).
fn hex(w: u32) -> String {
    w.to_le_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// The little-endian words of a `g` reply.
fn words(hex: &str) -> Vec<u32> {
    assert_eq!(hex.len() % 8, 0, "{hex}");
    (0..hex.len() / 8)
        .map(|i| {
            let bytes: [u8; 4] = std::array::from_fn(|k| {
                let at = 8 * i + 2 * k;
                u8::from_str_radix(&hex[at..at + 2], 16).expect("hex byte")
            });
            u32::from_le_bytes(bytes)
        })
        .collect()
}

/// The signal number (two hex digits) of a stop reply: `S05`, or `T05thread:...`.
fn signal(stop: &str) -> &str {
    assert!(
        (stop.starts_with('S') || stop.starts_with('T')) && stop.len() >= 3,
        "not a stop reply: {stop}"
    );
    &stop[1..3]
}

fn checksum(body: &[u8]) -> u8 {
    body.iter().fold(0, |a, b| a.wrapping_add(*b))
}

fn frame(body: &str) -> String {
    format!("${body}#{:02x}", checksum(body.as_bytes()))
}

/// Undo the stub's run-length encoding: `c*n` is `c` followed by `n - 29` more `c`.
fn rle_decode(body: &[u8]) -> String {
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < body.len() {
        if body[i] == b'*' && i + 1 < body.len() {
            let last = *out.last().expect("a run repeats a character");
            out.extend(std::iter::repeat_n(last, usize::from(body[i + 1] - 29)));
            i += 2;
        } else {
            out.push(body[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("ASCII reply")
}

/// A minimal RSP client.
struct Client {
    stream: TcpStream,
    /// Whether packets are still acknowledged (`QStartNoAckMode` ends that).
    acks: bool,
}

impl Client {
    fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).expect("connect");
        // A stub that stops answering fails the test instead of hanging it.
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .expect("timeout");
        stream.set_nodelay(true).expect("nodelay");
        Client { stream, acks: true }
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).expect("send");
    }

    fn byte(&mut self) -> u8 {
        let mut b = [0];
        self.stream.read_exact(&mut b).expect("receive");
        b[0]
    }

    /// Send a packet; in ack mode, expect the stub's `+`.
    fn send(&mut self, body: &str) {
        self.raw(frame(body).as_bytes());
        if self.acks {
            assert_eq!(self.byte() as char, '+', "ack for `{body}`");
        }
    }

    /// Receive one packet without acknowledging it; nothing may precede its `$`.
    fn packet(&mut self) -> String {
        let start = self.byte();
        assert_eq!(start as char, '$', "start of a reply");
        let mut body = Vec::new();
        loop {
            match self.byte() {
                b'#' => break,
                b => body.push(b),
            }
        }
        let sum = [self.byte(), self.byte()];
        let sum = u8::from_str_radix(std::str::from_utf8(&sum).expect("ASCII"), 16);
        assert_eq!(sum, Ok(checksum(&body)), "reply checksum");
        rle_decode(&body)
    }

    /// Receive a reply and, in ack mode, acknowledge it.
    fn reply(&mut self) -> String {
        let body = self.packet();
        if self.acks {
            self.raw(b"+");
        }
        body
    }

    fn cmd(&mut self, body: &str) -> String {
        self.send(body);
        self.reply()
    }
}

/// Serve one client on a free local port; `client` talks to the stub from another thread.
fn session(m: &mut Machine, client: impl FnOnce(&mut Client) + Send + 'static) -> io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let talk = thread::spawn(move || client(&mut Client::connect(addr)));
    let served = serve(m, listener);
    if let Err(panic) = talk.join() {
        std::panic::resume_unwind(panic);
    }
    served
}

#[test]
fn reads_registers() {
    let mut m = boot(LOOP);
    let r = &mut m.cpu.regs;
    r.r[3] = 0x1234_5678;
    r.msp.ns = 0x2000_0F00;
    r.psp.s = 0x3000_0400;
    r.psp.ns = 0x2000_1000;
    r.msplim.s = 0x3000_0100;
    r.psplim.s = 0x3000_0200;
    r.msplim.ns = 0x2000_0100;
    r.psplim.ns = 0x2000_0200;
    r.primask.s = true;
    r.basepri.s = 0x40;
    r.control.s = 0b10; // SPSEL: Thread mode uses PSP_S
    let (pc, xpsr) = (r.pc, r.xpsr.to_u32());
    session(&mut m, move |c| {
        let regs = words(&c.cmd("g"));
        assert_eq!(regs.len(), 31, "17 core, 6 m-system and 8 secext registers");
        assert_eq!(regs[3], 0x1234_5678);
        assert_eq!(
            regs[13], 0x3000_0400,
            "sp: the current stack pointer, PSP_S"
        );
        assert_eq!(regs[15], pc);
        assert_eq!(regs[16], xpsr);
        assert_eq!(
            regs[17..23],
            [SP_INIT, 0x3000_0400, 1, 0x40, 0, 0b10],
            "msp psp primask basepri faultmask control, of the current (Secure) state"
        );
        assert_eq!(
            regs[23..31],
            [
                0x2000_0F00,
                SP_INIT,
                0x2000_1000,
                0x3000_0400,
                0x3000_0100,
                0x3000_0200,
                0x2000_0100,
                0x2000_0200
            ],
            "msp_ns msp_s psp_ns psp_s msplim_s psplim_s msplim_ns psplim_ns"
        );
        // `p` uses the target description's numbers: xpsr is 25, the m-system registers
        // follow from 26 and the secext ones from 32.
        assert_eq!(c.cmd("pf"), hex(pc));
        assert_eq!(c.cmd("p19"), hex(xpsr));
        assert_eq!(c.cmd("p1a"), hex(SP_INIT), "msp");
        assert_eq!(c.cmd("p22"), hex(0x2000_1000), "psp_ns");
        assert_eq!(c.cmd("p27"), hex(0x2000_0200), "psplim_ns");
        assert_eq!(c.cmd("p10"), "", "16..=24 do not exist");
        assert_eq!(c.cmd("p28"), "", "nor does anything past psplim_ns");
        assert_eq!(c.cmd("D"), "OK");
    })
    .expect("serve");
}

#[test]
fn target_description_names_m_profile_registers() {
    let mut m = boot(LOOP);
    session(&mut m, |c| {
        let supported = c.cmd("qSupported:multiprocess+;swbreak+;xmlRegisters=arm");
        assert!(supported.contains("qXfer:features:read+"), "{supported}");
        // Read the document in parts: `m<data>` while more follows, `l<data>` for the last.
        let mut xml = String::new();
        loop {
            let part = c.cmd(&format!(
                "qXfer:features:read:target.xml:{:x},400",
                xml.len()
            ));
            xml.push_str(&part[1..]);
            match part.as_bytes()[0] {
                b'l' => break,
                b'm' => {}
                _ => panic!("not a qXfer reply: {part}"),
            }
        }
        assert!(
            xml.starts_with("<?xml") && xml.ends_with("</target>"),
            "{xml}"
        );
        for feature in [
            "org.gnu.gdb.arm.m-profile",
            "org.gnu.gdb.arm.m-system",
            "org.gnu.gdb.arm.secext",
        ] {
            assert!(
                xml.contains(&format!("<feature name=\"{feature}\">")),
                "{xml}"
            );
        }
        let names = [
            "r0",
            "r1",
            "r2",
            "r3",
            "r4",
            "r5",
            "r6",
            "r7",
            "r8",
            "r9",
            "r10",
            "r11",
            "r12",
            "sp",
            "lr",
            "pc",
            "xpsr",
            "msp",
            "psp",
            "primask",
            "basepri",
            "faultmask",
            "control",
            "msp_ns",
            "msp_s",
            "psp_ns",
            "psp_s",
            "msplim_s",
            "psplim_s",
            "msplim_ns",
            "psplim_ns",
        ];
        for name in names {
            assert!(
                xml.contains(&format!("<reg name=\"{name}\" ")),
                "{name}: {xml}"
            );
        }
        assert!(
            xml.contains("<reg name=\"xpsr\" bitsize=\"32\" regnum=\"25\""),
            "{xml}"
        );
        assert_eq!(c.cmd("D"), "OK");
    })
    .expect("serve");
}

#[test]
fn sets_breakpoint_and_continues() {
    let mut m = boot(LOOP);
    let bp = addr(CODE + 4); // subs r2, #1
    session(&mut m, move |c| {
        assert_eq!(c.cmd(&format!("Z0,{bp:x},2")), "OK");
        assert_eq!(
            c.cmd(&format!("m{bp:x},2")),
            "013a",
            "the code is not patched"
        );
        let stop = c.cmd("c");
        assert!(
            stop.starts_with("T05") && stop.contains("swbreak"),
            "{stop}"
        );
        assert_eq!(words(&c.cmd("g"))[15], bp);
        // Continuing from the breakpoint runs it and stops there on the next pass.
        assert_eq!(signal(&c.cmd("c")), "05");
        assert_eq!(c.cmd("p0"), hex(2), "r0: two passes");
        assert_eq!(c.cmd(&format!("z0,{bp:x},2")), "OK");
        assert_eq!(c.cmd("D"), "OK");
    })
    .expect("serve");
    assert_eq!(m.cpu.regs.pc, bp);
    assert!(m.breakpoints.is_empty(), "z0 removed it");
}

#[test]
fn malformed_packet_is_nacked() {
    // Review Focus 5.
    let mut m = boot(LOOP);
    session(&mut m, |c| {
        // Bad checksum ("?" sums to 0x3f): NAK, nothing executed or answered.
        c.raw(b"$?#00");
        assert_eq!(c.byte() as char, '-');
        assert_eq!(signal(&c.cmd("?")), "05");
        // Unparsable hex checksum: NAK too.
        c.raw(b"$?#zz");
        assert_eq!(c.byte() as char, '-');
        // Line noise between packets is ignored.
        c.raw(b"\r\nnoise");
        assert_eq!(signal(&c.cmd("?")), "05");
        // Well-formed packets with nonsense inside get an error reply; the session goes on.
        assert!(c.cmd("mzz,4").starts_with('E'));
        assert!(
            c.cmd("m123456789,4").starts_with('E'),
            "address over 32 bits"
        );
        assert!(c.cmd("Gabc").starts_with('E'), "short G");
        assert!(
            c.cmd("qXfer:features:read:other.xml:0,100")
                .starts_with('E')
        );
        assert_eq!(
            c.cmd("vMustReplyEmpty"),
            "",
            "unknown packets are unsupported"
        );
        // A NAK of a reply makes the stub send it again.
        c.send("pf");
        let first = c.packet();
        c.raw(b"-");
        assert_eq!(c.packet(), first);
        c.raw(b"+");
        assert_eq!(c.cmd("pf"), first);
        assert_eq!(c.cmd("D"), "OK");
    })
    .expect("serve");
    assert_eq!(m.sched.now, 0, "nothing ran");
}

/// Memory accesses that wrap past 0xFFFF_FFFF, and reads longer than a reply can hold, get
/// `E01` from the stub itself: `gdbstub` overflows on the first (a panic in debug builds), and
/// would buffer gigabytes for the second.
#[test]
fn oversized_or_wrapping_memory_access_is_refused() {
    let mut m = boot(LOOP);
    session(&mut m, |c| {
        assert_eq!(c.cmd("mfffff000,2000"), "E01", "wraps, over several chunks");
        assert_eq!(c.cmd("mffffffff,2"), "E01", "wraps");
        assert_eq!(c.cmd("m0,ffffffff"), "E01", "4 GiB");
        assert_eq!(c.cmd("m30000000,801"), "E01", "one byte over the limit");
        assert_eq!(c.cmd("Mffffffff,2:0102"), "E01", "write that wraps");
        assert_eq!(c.cmd("Xfffffffe,4:abcd"), "E01", "binary write that wraps");
        // Headers that are not canonical `addr,len` (1-8 hex digits each) are refused too:
        // `gdbstub` would accept them and get around the checks above.
        let bypasses = [
            "m000000000000000000fffff000,2000", // zero-padded address, wraps
            "m30000000,0000000000000000004000", // zero-padded length, too long
            "m00000000000000000,ffffffff",      // both padded, 4 GiB
            "m30000000,4000,0",                 // a third field, too long
            "m0,ffffffff,0",                    // a third field, 4 GiB
            "M0000000000ffffffff,2:0102",       // zero-padded address, wraps
            "M30000000,2,0:0102",               // a third field
            "X00000000030000000,1:a",           // zero-padded address
        ];
        for packet in bypasses {
            assert_eq!(c.cmd(packet), "E01", "{packet}");
        }
        assert_eq!(
            c.cmd("m30001000,4"),
            "00000000",
            "a canonical read still works"
        );
        // GDB's own largest read (PacketSize / 2 bytes) and reads up to the top of memory
        // are served.
        assert_eq!(c.cmd("m30001000,800"), "0".repeat(0x1000));
        assert_eq!(c.cmd("mfffffffe,2"), "0000");
        assert_eq!(
            c.cmd("Mfffffffe,2:0102").chars().next(),
            Some('E'),
            "unmapped"
        );
        assert_eq!(signal(&c.cmd("?")), "05", "the session goes on");
        assert_eq!(c.cmd("D"), "OK");
    })
    .expect("serve");
}

#[test]
fn no_ack_mode() {
    let mut m = boot(LOOP);
    session(&mut m, |c| {
        assert_eq!(c.cmd("QStartNoAckMode"), "OK");
        c.acks = false;
        assert_eq!(signal(&c.cmd("?")), "05", "no `+` before the reply");
        // A corrupt packet is dropped without a NAK.
        c.raw(b"$?#00");
        assert_eq!(c.cmd("pf"), hex(addr(CODE)));
        assert_eq!(c.cmd("D"), "OK");
    })
    .expect("serve");
}

#[test]
fn disconnect_leaves_machine_runnable() {
    // Review Focus 5: dropped while the guest runs.
    let mut m = boot(LOOP);
    session(&mut m, |c| {
        c.send("c");
        thread::sleep(Duration::from_millis(50));
    })
    .expect("a client hanging up is not an error");
    assert!(m.sched.now > 0, "the guest ran");
    let pc = m.cpu.regs.pc;
    assert_eq!(m.step(), StopReason::Stepped);
    assert_ne!(m.cpu.regs.pc, pc);
    assert!(!m.cpu.locked_up());

    // Dropped halfway through a packet.
    let mut m = boot(LOOP);
    session(&mut m, |c| c.raw(b"$m100b06")).expect("serve");
    assert_eq!(m.step(), StopReason::Stepped);
    assert_eq!(m.cpu.regs.r[0], 1);
}

#[test]
fn ctrl_c_interrupts_continue() {
    let mut m = boot(LOOP);
    session(&mut m, |c| {
        c.send("c");
        thread::sleep(Duration::from_millis(20));
        c.raw(&[0x03]);
        let stop = c.reply();
        assert_eq!(signal(&stop), "02", "SIGINT");
        assert_eq!(signal(&c.cmd("?")), "05");
        assert_eq!(c.cmd("D"), "OK");
    })
    .expect("serve");
    assert!(m.sched.now > 0);
}

#[test]
fn idle_guest_stops_continue() {
    let mut m = boot(SLEEP);
    session(&mut m, |c| {
        // The WFI sleeps with nothing to wake it: a stop instead of a hang.
        let stop = c.cmd("c");
        assert_eq!(signal(&stop), "05", "SIGTRAP");
        assert_eq!(c.cmd("pf"), hex(addr(CODE + 2)), "after the WFI");
        // The halt woke the CPU: another continue makes one more pass.
        assert_eq!(signal(&c.cmd("c")), "05");
        assert_eq!(c.cmd("p0"), hex(1));
        assert_eq!(c.cmd("D"), "OK");
    })
    .expect("serve");
    assert!(!m.asleep(), "a debugger halt ends the sleep");
}

#[test]
fn single_steps() {
    let mut m = boot(SLEEP);
    session(&mut m, |c| {
        assert_eq!(signal(&c.cmd("s")), "05", "the WFI");
        assert_eq!(c.cmd("pf"), hex(addr(CODE + 2)));
        // The halt ended the sleep, so the next step runs the next instruction.
        assert_eq!(signal(&c.cmd("vCont;s:1")), "05");
        assert_eq!(c.cmd("p0"), hex(1));
        assert_eq!(c.cmd("pf"), hex(addr(CODE + 4)));
        assert_eq!(c.cmd("D"), "OK");
    })
    .expect("serve");
    assert_eq!(m.sched.now, 2);
}

#[test]
fn memory_access_has_no_side_effects() {
    let mut m = boot(LOOP);
    m.cpu.ppb_write32(SYST_RVR, 9);
    m.cpu.ppb_write32(SYST_CSR, 0b101);
    m.cpu.tick_systick(11); // COUNTFLAG set
    let code = addr(CODE);
    session(&mut m, move |c| {
        assert_eq!(c.cmd(&format!("m{code:x},4")), "01300231");
        assert_eq!(
            c.cmd("m50000000,8"),
            "0000000000000000",
            "unmapped reads as 0"
        );
        assert_eq!(
            c.cmd("me0001000,4"),
            "00000000",
            "unimplemented PPB reads as 0"
        );
        // Reading SYST_CSR leaves COUNTFLAG set.
        assert_eq!(c.cmd("me000e010,4"), hex(0b101 | COUNTFLAG));
        assert_eq!(c.cmd("me000e012,1"), "01", "byte reads of the PPB");
        assert_eq!(c.cmd("me000e010,4"), hex(0b101 | COUNTFLAG));

        assert_eq!(c.cmd("M30000100,4:efbeadde"), "OK");
        assert_eq!(
            c.cmd("m20000100,4"),
            "efbeadde",
            "one SRAM behind both aliases"
        );
        assert_eq!(
            c.cmd(&format!("M{code:x},2:00bf")),
            "OK",
            "flash can be patched"
        );
        assert_eq!(c.cmd("Me000e014,4:63000000"), "OK", "PPB words");
        assert!(c.cmd("M50000000,4:00000000").starts_with('E'), "unmapped");
        assert!(
            c.cmd("M2017fffe,4:01020304").starts_with('E'),
            "partly unmapped"
        );
        assert!(c.cmd("Me000e014,1:00").starts_with('E'), "PPB: words only");
        assert_eq!(c.cmd("D"), "OK");
    })
    .expect("serve");
    assert!(m.bus.log.is_empty(), "debugger accesses are not logged");
    assert_eq!(m.cpu.ppb_read32(SYST_CSR), 0b101 | COUNTFLAG);
    assert_eq!(m.cpu.ppb_read32(SYST_RVR), 99);
    assert_eq!(m.bus.peek(0x3000_0100, 4), Some(0xDEAD_BEEF));
    assert_eq!(m.bus.peek(code, 2), Some(0xBF00));
    assert_eq!(
        m.bus.peek(0x2017_FFFE, 2),
        Some(0),
        "a refused write writes nothing"
    );
}

/// `G` carries all registers, including several names for one register (sp, msp and msp_s in
/// Secure Thread mode with SPSEL clear). Changing any one of them must stick.
#[test]
fn g_write_changes_aliased_registers() {
    fn g_packet(regs: &[u32]) -> String {
        let hexes: String = regs.iter().map(|&w| hex(w)).collect();
        format!("G{hexes}")
    }
    let mut m = boot(LOOP);
    session(&mut m, |c| {
        let mut regs = words(&c.cmd("g"));
        regs[13] = 0x3000_0700;
        assert_eq!(c.cmd(&g_packet(&regs)), "OK", "sp");
        let regs = words(&c.cmd("g"));
        assert_eq!(
            (regs[13], regs[17], regs[24]),
            (0x3000_0700, 0x3000_0700, 0x3000_0700),
            "sp, msp, msp_s"
        );

        let mut regs = words(&c.cmd("g"));
        regs[17] = 0x3000_0600;
        assert_eq!(c.cmd(&g_packet(&regs)), "OK", "msp");
        let regs = words(&c.cmd("g"));
        assert_eq!(
            (regs[13], regs[17], regs[24]),
            (0x3000_0600, 0x3000_0600, 0x3000_0600)
        );

        let mut regs = words(&c.cmd("g"));
        regs[23] = 0x2000_0F00;
        regs[26] = 0x3000_0400;
        assert_eq!(c.cmd(&g_packet(&regs)), "OK", "msp_ns, psp_s");
        let regs = words(&c.cmd("g"));
        assert_eq!(
            (regs[23], regs[26], regs[18]),
            (0x2000_0F00, 0x3000_0400, 0x3000_0400)
        );
        assert_eq!(c.cmd("D"), "OK");
    })
    .expect("serve");
    let r = &m.cpu.regs;
    assert_eq!(
        (r.msp.s, r.msp.ns, r.psp.s),
        (0x3000_0600, 0x2000_0F00, 0x3000_0400)
    );
}

#[test]
fn writes_registers() {
    let mut m = boot(SLEEP);
    assert_eq!(m.run(100), StopReason::Idle);
    session(&mut m, |c| {
        // `G` changes only what differs from the values before it: here r1.
        let mut regs = words(&c.cmd("g"));
        regs[1] = 0xCAFE_F00D;
        let g: String = regs.iter().map(|&w| hex(w)).collect();
        assert_eq!(c.cmd(&format!("G{g}")), "OK");
        assert_eq!(c.cmd(&format!("Pf={}", hex(addr(CODE + 4) | 1))), "OK");
        assert_eq!(c.cmd(&format!("P21={}", hex(0x3000_0903))), "OK", "msp_s");
        assert_eq!(c.cmd(&format!("P1f={}", hex(0b110))), "OK", "control");
        assert_eq!(c.cmd(&format!("Pd={}", hex(0x3000_0700))), "OK", "sp");
        assert_eq!(c.cmd(&format!("P1d={}", hex(0xFF))), "OK", "basepri");
        assert_eq!(
            c.cmd(&format!("P24={}", hex(0x3000_0105))),
            "OK",
            "msplim_s"
        );
        assert!(c.cmd("P0=1234").starts_with('E'), "registers are 4 bytes");
        assert_eq!(c.cmd("D"), "OK");
    })
    .expect("serve");
    let r = &m.cpu.regs;
    assert_eq!((r.r[0], r.r[1]), (0, 0xCAFE_F00D));
    assert_eq!(r.pc, addr(CODE + 4), "bit 0 dropped");
    assert!(r.xpsr.t);
    assert_eq!(r.msp.s, 0x3000_0900, "word aligned");
    assert_eq!(r.control.s, 0b10, "no FPU: FPCA is RES0");
    assert_eq!(r.psp.s, 0x3000_0700, "sp is PSP_S once SPSEL is set");
    assert_eq!(r.basepri.s, 0xE0, "3 priority bits");
    assert_eq!(r.msplim.s, 0x3000_0100, "8-byte aligned");
    assert!(!m.asleep(), "attaching (a debug halt) woke the CPU");
}
