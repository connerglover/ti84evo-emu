//! GDB remote stub: `arm-none-eabi-gdb` (and Ghidra or IDA through it) debugs the emulated
//! firmware over TCP with the Remote Serial Protocol, implemented by the `gdbstub` crate.
//!
//! # Registers
//!
//! The target description uses GDB's own feature and register names (`features/arm/*.xml`):
//! `org.gnu.gdb.arm.m-profile` (r0-r12, sp, lr, pc, xpsr), `org.gnu.gdb.arm.m-system` (msp,
//! psp, primask, basepri, faultmask, control, all the current security state's copies) and
//! `org.gnu.gdb.arm.secext` (msp_ns, msp_s, psp_ns, psp_s, msplim_s, psplim_s, msplim_ns,
//! psplim_ns). As in GDB's M-profile description, xpsr is register 25; the m-system registers
//! are 26-31 and the secext ones 32-39 (the numbers `p`/`P` use). `sp` is the stack pointer in
//! use. Writes are masked like `MSR` writes: stack pointers word-aligned, limits 8-byte
//! aligned, BASEPRI to its implemented bits, CONTROL to nPRIV and SPSEL (no FPU). A PC write
//! drops bit 0.
//!
//! # Memory
//!
//! Outside the PPB, reads and writes use `MemoryMap::peek`/`poke`: no device side effects and
//! nothing in the unmapped log. Unmapped (and device) memory reads as zero; a write that
//! touches any is refused (`E` reply) and writes nothing. Flash can be patched. The PPB is read
//! with `Cpu::ppb_peek32` (no read side effects; unimplemented registers read as zero) and
//! written through `Cpu::ppb_write32`, in whole aligned words only, in the view of the CPU's
//! current security state.
//!
//! # Run control
//!
//! Software breakpoints are entries in `Machine::breakpoints`; code is never patched.
//! `continue` runs until a breakpoint, a guest `BKPT` (reported as SIGTRAP), a lockup
//! (SIGSEGV), a sleep nothing can end ([`StopReason::Idle`], SIGTRAP) or a Ctrl-C from GDB
//! (SIGINT), which is polled for every `POLL_STEPS` (4096) CPU steps. Signals GDB passes on resume
//! are ignored. Attaching and every stop are debug halts, which end a WFI/WFE sleep as on
//! hardware ([`Machine::wake`]): the next step runs the instruction after the WFI/WFE.
//!
//! # Transport
//!
//! [`serve`] takes one client and returns when it detaches, kills the target or hangs up,
//! leaving the machine usable. The link layer is handled here, because `gdbstub` ends the
//! session on a bad checksum, a NAK or a stray byte: packets with a bad checksum are NAKed
//! (`-`) and dropped, a reply the client NAKs is sent again, line noise between packets is
//! skipped, and a well-formed packet that `gdbstub` rejects gets an `E01` reply. Memory
//! accesses (`m`, `M`, `X`) get `E01` from the link, without reaching `gdbstub`, when their
//! range wraps past 0xFFFF_FFFF, when an `m` reads more than `MAX_READ` (2048) bytes, and
//! whenever their `addr,len` header is not canonical (1-8 hex digits each).

use std::collections::VecDeque;
use std::convert::Infallible;
use std::io::{self, Read, Write};
use std::marker::PhantomData;
use std::net::{TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::ops::ControlFlow;

use armv8m::Xpsr;
use armv8m::scs::{PRIORITY_MASK, is_ppb};
use gdbstub::arch::{Arch, RegId, Registers};
use gdbstub::common::Signal;
use gdbstub::conn::{Connection, ConnectionExt};
use gdbstub::stub::run_blocking::{BlockingEventLoop, Event, WaitForStopReasonError};
use gdbstub::stub::{GdbStub, SingleThreadStopReason};
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::ext::base::single_register_access::{
    SingleRegisterAccess, SingleRegisterAccessOps,
};
use gdbstub::target::ext::base::singlethread::{
    SingleThreadBase, SingleThreadResume, SingleThreadResumeOps, SingleThreadSingleStep,
    SingleThreadSingleStepOps,
};
use gdbstub::target::ext::breakpoints::{
    Breakpoints, BreakpointsOps, SwBreakpoint, SwBreakpointOps,
};
use gdbstub::target::{Target, TargetError, TargetResult};

use crate::{Machine, StopReason};

/// CPU steps between checks for a Ctrl-C from GDB while the guest runs.
const POLL_STEPS: u32 = 4096;

/// Longest packet accepted from the client: the size of `gdbstub`'s packet buffer (its
/// default), which it advertises as `PacketSize`.
const MAX_PACKET: usize = 4096;

/// `EFAULT`, the error code of a refused memory write.
const EFAULT: u8 = 14;

/// Serve one GDB client on `listener` (which should be bound to a loopback address): accept
/// it, then debug `machine` until the client detaches, kills the target or disconnects. All
/// of those return `Ok`, and the machine stays usable; errors come only from accepting and
/// setting up the connection.
pub fn serve(machine: &mut Machine, listener: TcpListener) -> io::Result<()> {
    listener.set_nonblocking(false)?;
    let (stream, peer) = listener.accept()?;
    log::info!("gdb: {peer} attached");
    let mut link = Link::new(stream)?;
    // Attaching halts the CPU, which ends a WFI/WFE sleep.
    machine.wake();
    let mut target = Debuggee {
        m: machine,
        resume: Resume::Continue,
    };
    loop {
        let stub = GdbStub::new(&mut link);
        match stub.run_blocking::<EventLoop<'_, '_>>(&mut target) {
            Ok(reason) => {
                log::info!("gdb: {peer} ended the session ({reason:?})");
                return Ok(());
            }
            Err(e) if e.is_connection_error() => {
                log::info!("gdb: {peer} is gone: {e}");
                return Ok(());
            }
            // `gdbstub` gave up on a packet; start it afresh on the same connection.
            Err(e) => {
                log::warn!("gdb: bad packet from {peer}: {e}");
                if let Err(e) = link.recover() {
                    log::info!("gdb: {peer} is gone: {e}");
                    return Ok(());
                }
            }
        }
    }
}

// ---- Registers ----

// Registers in `g`-packet order: the order of the target description.
const SP: usize = 13;
const LR: usize = 14;
const PC: usize = 15;
const XPSR: usize = 16;
const MSP: usize = 17;
const PSP: usize = 18;
const PRIMASK: usize = 19;
const BASEPRI: usize = 20;
const FAULTMASK: usize = 21;
const CONTROL: usize = 22;
const MSP_NS: usize = 23;
const MSP_S: usize = 24;
const PSP_NS: usize = 25;
const PSP_S: usize = 26;
const MSPLIM_S: usize = 27;
const PSPLIM_S: usize = 28;
const MSPLIM_NS: usize = 29;
const PSPLIM_NS: usize = 30;
const NUM_REGS: usize = 31;
/// The target description's number for xpsr; the registers after it count on from there.
const XPSR_REGNUM: usize = 25;

/// The target description. Register numbers follow document order, from `regnum` attributes
/// on: r0-pc are 0-15, xpsr 25, and the rest 26-39.
const TARGET_XML: &str = r#"<?xml version="1.0"?>
<!DOCTYPE target SYSTEM "gdb-target.dtd">
<target version="1.0">
<architecture>arm</architecture>
<feature name="org.gnu.gdb.arm.m-profile">
<reg name="r0" bitsize="32" regnum="0" type="uint32" group="general"/>
<reg name="r1" bitsize="32" type="uint32" group="general"/>
<reg name="r2" bitsize="32" type="uint32" group="general"/>
<reg name="r3" bitsize="32" type="uint32" group="general"/>
<reg name="r4" bitsize="32" type="uint32" group="general"/>
<reg name="r5" bitsize="32" type="uint32" group="general"/>
<reg name="r6" bitsize="32" type="uint32" group="general"/>
<reg name="r7" bitsize="32" type="uint32" group="general"/>
<reg name="r8" bitsize="32" type="uint32" group="general"/>
<reg name="r9" bitsize="32" type="uint32" group="general"/>
<reg name="r10" bitsize="32" type="uint32" group="general"/>
<reg name="r11" bitsize="32" type="uint32" group="general"/>
<reg name="r12" bitsize="32" type="uint32" group="general"/>
<reg name="sp" bitsize="32" type="data_ptr" group="general"/>
<reg name="lr" bitsize="32" type="uint32" group="general"/>
<reg name="pc" bitsize="32" type="code_ptr" group="general"/>
<reg name="xpsr" bitsize="32" regnum="25" type="uint32" group="general"/>
</feature>
<feature name="org.gnu.gdb.arm.m-system">
<reg name="msp" bitsize="32" type="data_ptr" group="general"/>
<reg name="psp" bitsize="32" type="data_ptr" group="general"/>
<reg name="primask" bitsize="32" type="uint32" group="general"/>
<reg name="basepri" bitsize="32" type="uint32" group="general"/>
<reg name="faultmask" bitsize="32" type="uint32" group="general"/>
<reg name="control" bitsize="32" type="uint32" group="general"/>
</feature>
<feature name="org.gnu.gdb.arm.secext">
<reg name="msp_ns" bitsize="32" type="data_ptr" group="general"/>
<reg name="msp_s" bitsize="32" type="data_ptr" group="general"/>
<reg name="psp_ns" bitsize="32" type="data_ptr" group="general"/>
<reg name="psp_s" bitsize="32" type="data_ptr" group="general"/>
<reg name="msplim_s" bitsize="32" type="data_ptr" group="general"/>
<reg name="psplim_s" bitsize="32" type="data_ptr" group="general"/>
<reg name="msplim_ns" bitsize="32" type="data_ptr" group="general"/>
<reg name="psplim_ns" bitsize="32" type="data_ptr" group="general"/>
</feature>
</target>
"#;

/// The Cortex-M33 as GDB sees it.
enum CortexM33 {}

impl Arch for CortexM33 {
    type Usize = u32;
    type Registers = Regs;
    /// GDB's breakpoint kind (2 or 3 for Thumb code); breakpoints do not patch code, so it
    /// does not matter.
    type BreakpointKind = usize;
    type RegId = Reg;

    fn target_description_xml() -> Option<&'static str> {
        Some(TARGET_XML)
    }
}

/// All registers, in `g`-packet order.
#[derive(Clone, Debug, Default, PartialEq)]
struct Regs([u32; NUM_REGS]);

impl Registers for Regs {
    type ProgramCounter = u32;

    fn pc(&self) -> u32 {
        self.0[PC]
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        for byte in self.0.iter().flat_map(|w| w.to_le_bytes()) {
            write_byte(Some(byte));
        }
    }

    fn gdb_deserialize(&mut self, bytes: &[u8]) -> Result<(), ()> {
        let (words, []) = bytes.as_chunks::<4>() else {
            return Err(());
        };
        if words.len() != NUM_REGS {
            return Err(());
        }
        for (w, &b) in self.0.iter_mut().zip(words) {
            *w = u32::from_le_bytes(b);
        }
        Ok(())
    }
}

/// A register, by its index in `g`-packet order.
#[derive(Clone, Copy, Debug)]
struct Reg(usize);

impl RegId for Reg {
    fn from_raw_id(id: usize) -> Option<(Self, Option<NonZeroUsize>)> {
        let index = match id {
            0..=PC => id,
            _ if (XPSR_REGNUM..XPSR_REGNUM + NUM_REGS - XPSR).contains(&id) => {
                id - XPSR_REGNUM + XPSR
            }
            _ => return None,
        };
        Some((Reg(index), NonZeroUsize::new(4)))
    }
}

/// Register `i` (`g`-packet order).
fn read_reg(m: &Machine, i: usize) -> u32 {
    let r = &m.cpu.regs;
    let sec = r.sec;
    match i {
        0..=12 => r.r[i],
        SP => r.sp(),
        LR => r.lr,
        PC => r.pc,
        XPSR => r.xpsr.to_u32(),
        MSP => *r.msp.get(sec),
        PSP => *r.psp.get(sec),
        PRIMASK => u32::from(*r.primask.get(sec)),
        BASEPRI => u32::from(*r.basepri.get(sec)),
        FAULTMASK => u32::from(*r.faultmask.get(sec)),
        CONTROL => *r.control.get(sec),
        MSP_NS => r.msp.ns,
        MSP_S => r.msp.s,
        PSP_NS => r.psp.ns,
        PSP_S => r.psp.s,
        MSPLIM_S => r.msplim.s,
        PSPLIM_S => r.psplim.s,
        MSPLIM_NS => r.msplim.ns,
        PSPLIM_NS => r.psplim.ns,
        _ => 0,
    }
}

/// Write register `i` (`g`-packet order) as a debugger: see the module docs.
fn write_reg(m: &mut Machine, i: usize, v: u32) {
    if i == PC {
        m.cpu.regs.pc = v & !1;
        // A PC write is a wake event too (the stub only writes while halted, so already awake).
        m.wake();
        return;
    }
    let r = &mut m.cpu.regs;
    let sec = r.sec;
    match i {
        0..=12 => r.r[i] = v,
        SP => r.set_sp(v & !3),
        LR => r.lr = v,
        XPSR => r.xpsr = Xpsr::from_u32(v),
        MSP => *r.msp.get_mut(sec) = v & !3,
        PSP => *r.psp.get_mut(sec) = v & !3,
        PRIMASK => *r.primask.get_mut(sec) = v & 1 != 0,
        BASEPRI => *r.basepri.get_mut(sec) = v as u8 & PRIORITY_MASK,
        FAULTMASK => *r.faultmask.get_mut(sec) = v & 1 != 0,
        // nPRIV and SPSEL; without an FPU, FPCA and SFPA are RES0.
        CONTROL => *r.control.get_mut(sec) = v & 0b11,
        MSP_NS => r.msp.ns = v & !3,
        MSP_S => r.msp.s = v & !3,
        PSP_NS => r.psp.ns = v & !3,
        PSP_S => r.psp.s = v & !3,
        MSPLIM_S => r.msplim.s = v & !7,
        PSPLIM_S => r.psplim.s = v & !7,
        MSPLIM_NS => r.msplim.ns = v & !7,
        PSPLIM_NS => r.psplim.ns = v & !7,
        _ => {}
    }
}

// ---- Memory ----

/// Read guest memory at `start` into `data` as a debugger: see the module docs.
fn read_memory(m: &mut Machine, start: u32, data: &mut [u8]) {
    let sec = m.cpu.regs.sec;
    for (i, byte) in data.iter_mut().enumerate() {
        let addr = start.wrapping_add(i as u32);
        *byte = if is_ppb(addr) {
            let word = m.cpu.ppb_peek32(addr, sec).unwrap_or(0);
            (word >> (8 * (addr & 3))) as u8
        } else {
            m.bus.peek(addr, 1).unwrap_or(0) as u8
        };
    }
}

/// One step of a debugger write.
enum Piece {
    /// A byte of RAM or ROM (or of unwritable space, which refuses the whole write).
    Byte(u32, u8),
    /// An aligned PPB word.
    PpbWord(u32, u32),
}

/// Write `data` to guest memory at `start` as a debugger: see the module docs. Returns false,
/// having written nothing, if any of it is not RAM, ROM or whole PPB words.
fn write_memory(m: &mut Machine, start: u32, data: &[u8]) -> bool {
    let mut pieces = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let addr = start.wrapping_add(i as u32);
        if is_ppb(addr) {
            let Some(word) = data.get(i..i + 4).filter(|_| addr & 3 == 0) else {
                return false;
            };
            pieces.push(Piece::PpbWord(
                addr,
                u32::from_le_bytes([word[0], word[1], word[2], word[3]]),
            ));
            i += 4;
        } else {
            pieces.push(Piece::Byte(addr, data[i]));
            i += 1;
        }
    }
    let writable = pieces.iter().all(|p| match *p {
        Piece::Byte(addr, _) => m.bus.peek(addr, 1).is_some(),
        Piece::PpbWord(..) => true,
    });
    if !writable {
        return false;
    }
    for p in pieces {
        match p {
            Piece::Byte(addr, b) => {
                m.bus.poke(addr, 1, u32::from(b));
            }
            Piece::PpbWord(addr, w) => m.cpu.ppb_write32(addr, w),
        }
    }
    true
}

// ---- Target ----

/// How the client last resumed the target.
#[derive(Clone, Copy)]
enum Resume {
    Continue,
    Step,
}

/// The machine, as `gdbstub` drives it.
struct Debuggee<'m> {
    m: &'m mut Machine,
    resume: Resume,
}

impl Debuggee<'_> {
    /// A debug halt: end any WFI/WFE sleep, and report `stop` to GDB.
    fn halt(&mut self, stop: StopReason) -> SingleThreadStopReason<u32> {
        self.m.wake();
        log::debug!("gdb: stop {stop:?} at pc {:#010x}", self.m.cpu.regs.pc);
        match stop {
            StopReason::Stepped => SingleThreadStopReason::DoneStep,
            StopReason::Breakpoint(_) => SingleThreadStopReason::SwBreak(()),
            StopReason::Halted => SingleThreadStopReason::Signal(Signal::SIGINT),
            StopReason::Lockup { pc } => {
                log::warn!("gdb: the CPU locked up at {pc:#010x}");
                SingleThreadStopReason::Signal(Signal::SIGSEGV)
            }
            // `CycleLimit` cannot happen: a continue has no budget.
            StopReason::Idle | StopReason::Bkpt { .. } | StopReason::CycleLimit => {
                SingleThreadStopReason::Signal(Signal::SIGTRAP)
            }
        }
    }
}

impl Target for Debuggee<'_> {
    type Arch = CortexM33;
    type Error = Infallible;

    fn base_ops(&mut self) -> BaseOps<'_, CortexM33, Infallible> {
        BaseOps::SingleThread(self)
    }

    fn support_breakpoints(&mut self) -> Option<BreakpointsOps<'_, Self>> {
        Some(self)
    }

    // LLDB gets the registers from the target description instead.
    fn use_lldb_register_info(&self) -> bool {
        false
    }
}

impl SingleThreadBase for Debuggee<'_> {
    fn read_registers(&mut self, regs: &mut Regs) -> TargetResult<(), Self> {
        for (i, w) in regs.0.iter_mut().enumerate() {
            *w = read_reg(self.m, i);
        }
        Ok(())
    }

    fn write_registers(&mut self, regs: &Regs) -> TargetResult<(), Self> {
        // `G` carries every register, and some are several names for one (sp, msp and msp_s,
        // say). Write only those that differ from their values before this packet; comparing
        // with the live values instead would let the unchanged old value of `msp` undo a new
        // `sp` written just before it. If a packet changes two names for one register, the
        // later one in `g` order wins.
        let mut before = Regs::default();
        self.read_registers(&mut before)?;
        for (i, (&v, &old)) in regs.0.iter().zip(&before.0).enumerate() {
            if v != old {
                write_reg(self.m, i, v);
            }
        }
        Ok(())
    }

    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, (), Self>> {
        Some(self)
    }

    fn read_addrs(&mut self, start_addr: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        read_memory(self.m, start_addr, data);
        Ok(data.len())
    }

    fn write_addrs(&mut self, start_addr: u32, data: &[u8]) -> TargetResult<(), Self> {
        if write_memory(self.m, start_addr, data) {
            Ok(())
        } else {
            Err(TargetError::Errno(EFAULT))
        }
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl SingleRegisterAccess<()> for Debuggee<'_> {
    fn read_register(&mut self, _tid: (), reg: Reg, buf: &mut [u8]) -> TargetResult<usize, Self> {
        let bytes = read_reg(self.m, reg.0).to_le_bytes();
        let out = buf.get_mut(..4).ok_or(TargetError::NonFatal)?;
        out.copy_from_slice(&bytes);
        Ok(4)
    }

    fn write_register(&mut self, _tid: (), reg: Reg, val: &[u8]) -> TargetResult<(), Self> {
        let bytes: [u8; 4] = val.try_into().map_err(|_| TargetError::NonFatal)?;
        write_reg(self.m, reg.0, u32::from_le_bytes(bytes));
        Ok(())
    }
}

impl SingleThreadResume for Debuggee<'_> {
    fn resume(&mut self, _signal: Option<Signal>) -> Result<(), Infallible> {
        self.resume = Resume::Continue;
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl SingleThreadSingleStep for Debuggee<'_> {
    fn step(&mut self, _signal: Option<Signal>) -> Result<(), Infallible> {
        self.resume = Resume::Step;
        Ok(())
    }
}

impl Breakpoints for Debuggee<'_> {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        Some(self)
    }
}

impl SwBreakpoint for Debuggee<'_> {
    fn add_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        self.m.breakpoints.insert(addr & !1);
        Ok(true)
    }

    fn remove_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        Ok(self.m.breakpoints.remove(&(addr & !1)))
    }
}

/// Runs the machine for `gdbstub` while the client waits for a stop.
struct EventLoop<'m, 'l>(PhantomData<(&'m mut Machine, &'l mut Link)>);

impl<'m, 'l> BlockingEventLoop for EventLoop<'m, 'l> {
    type Target = Debuggee<'m>;
    type Connection = &'l mut Link;
    type StopReason = SingleThreadStopReason<u32>;

    fn wait_for_stop_reason(
        target: &mut Debuggee<'m>,
        conn: &mut &'l mut Link,
    ) -> Result<Event<Self::StopReason>, WaitForStopReasonError<Infallible, io::Error>> {
        let stop = match target.resume {
            Resume::Step => target.m.step(),
            Resume::Continue => {
                // The client's input first: the rest of a packet, or a Ctrl-C.
                if let Some(b) = conn.poll().map_err(WaitForStopReasonError::Connection)? {
                    return Ok(Event::IncomingData(b));
                }
                let mut steps = 0u32;
                let mut input = Ok(None);
                let stop = target.m.run_with(u64::MAX, |_| {
                    steps = steps.wrapping_add(1);
                    if steps.is_multiple_of(POLL_STEPS) {
                        input = conn.poll();
                    }
                    match input {
                        Ok(None) => ControlFlow::Continue(()),
                        _ => ControlFlow::Break(()),
                    }
                });
                match input {
                    Ok(Some(b)) => return Ok(Event::IncomingData(b)),
                    Err(e) => return Err(WaitForStopReasonError::Connection(e)),
                    Ok(None) => stop,
                }
            }
        };
        Ok(Event::TargetStopped(target.halt(stop)))
    }

    fn on_interrupt(target: &mut Debuggee<'m>) -> Result<Option<Self::StopReason>, Infallible> {
        Ok(Some(target.halt(StopReason::Halted)))
    }
}

// ---- Link ----

/// Where the link is in framing the client's input.
#[derive(Clone, Copy, PartialEq)]
enum Rx {
    /// Between packets.
    Idle,
    /// In a packet's body, after its `$`.
    Body,
    /// After the `#`, with this many checksum digits read.
    Checksum(u8),
}

/// The client's side of the protocol, between the socket and `gdbstub`: acknowledgments and
/// NAKs, retransmission, framing. `gdbstub` sees only whole packets with good checksums and
/// Ctrl-C bytes; its own `+` acknowledgments are dropped, since the link sends them.
struct Link {
    stream: TcpStream,
    /// Whether `stream` is in non-blocking mode.
    nonblocking: bool,
    /// Whether the client still exchanges `+`/`-` acknowledgments (until `QStartNoAckMode`).
    acks: bool,
    rx: Rx,
    /// The packet being received, from its `$` (at most `MAX_PACKET` bytes of it).
    rx_buf: Vec<u8>,
    /// The packet being received is longer than `MAX_PACKET`.
    rx_overflow: bool,
    /// Verified packets and Ctrl-C bytes for `gdbstub`, and how far it has read the first.
    inbox: VecDeque<Vec<u8>>,
    inbox_pos: usize,
    /// The packet `gdbstub` is writing, from its `$`.
    tx_buf: Vec<u8>,
    /// The last packet sent, for when the client NAKs it.
    last_sent: Vec<u8>,
    /// `gdbstub` has read a packet and not yet answered.
    unanswered: bool,
}

impl Link {
    fn new(stream: TcpStream) -> io::Result<Self> {
        // Accepted sockets may inherit the listener's mode.
        stream.set_nonblocking(false)?;
        Ok(Link {
            stream,
            nonblocking: false,
            acks: true,
            rx: Rx::Idle,
            rx_buf: Vec::new(),
            rx_overflow: false,
            inbox: VecDeque::new(),
            inbox_pos: 0,
            tx_buf: Vec::new(),
            last_sent: Vec::new(),
            unanswered: false,
        })
    }

    fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()> {
        if self.nonblocking != nonblocking {
            self.stream.set_nonblocking(nonblocking)?;
            self.nonblocking = nonblocking;
        }
        Ok(())
    }

    fn send_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.set_nonblocking(false)?;
        Write::write_all(&mut self.stream, bytes)
    }

    /// Read what the client has sent (waiting for something if `block`) and frame it.
    fn fill(&mut self, block: bool) -> io::Result<()> {
        self.set_nonblocking(!block)?;
        let mut buf = [0u8; 1024];
        let n = loop {
            match Read::read(&mut self.stream, &mut buf) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the client closed the connection",
                    ));
                }
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if !block && e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            }
        };
        for &b in &buf[..n] {
            self.receive(b)?;
        }
        Ok(())
    }

    /// Frame one byte from the client.
    fn receive(&mut self, b: u8) -> io::Result<()> {
        match self.rx {
            Rx::Idle => match b {
                b'$' => self.start_packet(),
                0x03 => self.inbox.push_back(vec![0x03]),
                b'-' if self.acks => self.resend()?,
                // `+` acknowledgments, and line noise.
                _ => {}
            },
            // An unescaped `$` cannot be in a body: the client started over.
            Rx::Body if b == b'$' => self.start_packet(),
            Rx::Body => {
                self.push_rx(b);
                if b == b'#' {
                    self.rx = Rx::Checksum(0);
                }
            }
            Rx::Checksum(0) => {
                self.push_rx(b);
                self.rx = Rx::Checksum(1);
            }
            Rx::Checksum(_) => {
                self.push_rx(b);
                self.rx = Rx::Idle;
                self.packet_received()?;
            }
        }
        Ok(())
    }

    fn start_packet(&mut self) {
        self.rx_buf.clear();
        self.rx_buf.push(b'$');
        self.rx_overflow = false;
        self.rx = Rx::Body;
    }

    fn push_rx(&mut self, b: u8) {
        if self.rx_buf.len() < MAX_PACKET {
            self.rx_buf.push(b);
        } else {
            self.rx_overflow = true;
        }
    }

    /// A whole packet arrived in `rx_buf`: acknowledge it and pass it on, or NAK it.
    fn packet_received(&mut self) -> io::Result<()> {
        let packet = std::mem::take(&mut self.rx_buf);
        let body = match packet.as_slice() {
            [b'$', body @ .., b'#', hi, lo]
                if !self.rx_overflow && hex_byte(*hi, *lo) == Some(checksum(body)) =>
            {
                body
            }
            _ => {
                log::debug!("gdb: NAK: bad checksum, or longer than {MAX_PACKET} bytes");
                if self.acks {
                    self.send_raw(b"-")?;
                }
                return Ok(());
            }
        };
        let no_ack_mode = body == b"QStartNoAckMode";
        if self.acks {
            self.send_raw(b"+")?;
        }
        if refused(body) {
            log::debug!(
                "gdb: E01 for a memory access past 0xffffffff or a read over {MAX_READ} bytes"
            );
            return self.send_packet(ERROR_REPLY.to_vec());
        }
        // The `+` above was the last one: `gdbstub` agrees to it and replies `OK`.
        if no_ack_mode {
            self.acks = false;
        }
        self.inbox.push_back(packet);
        Ok(())
    }

    /// Send the last packet again, for a client NAK.
    fn resend(&mut self) -> io::Result<()> {
        let packet = std::mem::take(&mut self.last_sent);
        let sent = self.send_raw(&packet);
        self.last_sent = packet;
        sent
    }

    /// The next byte for `gdbstub`, if any has arrived.
    fn next_byte(&mut self) -> Option<u8> {
        let item = self.inbox.front()?;
        let b = item[self.inbox_pos];
        self.inbox_pos += 1;
        if self.inbox_pos == item.len() {
            if item[0] == b'$' {
                self.unanswered = true;
            }
            self.inbox.pop_front();
            self.inbox_pos = 0;
        }
        Some(b)
    }

    /// The next byte for `gdbstub` without waiting, if the client has sent one.
    fn poll(&mut self) -> io::Result<Option<u8>> {
        if self.inbox.is_empty() {
            self.fill(false)?;
        }
        Ok(self.next_byte())
    }

    /// Take a byte of `gdbstub`'s output. Whole packets are sent; anything outside a packet is
    /// one of its acknowledgments, which the link sends itself.
    fn take_output(&mut self, b: u8) -> io::Result<()> {
        if self.tx_buf.is_empty() && b != b'$' {
            return Ok(());
        }
        self.tx_buf.push(b);
        let n = self.tx_buf.len();
        // `gdbstub` escapes `#` in bodies, so the first one ends the packet. Replies are
        // bounded: the longest is an `m` reply, and `refused` caps those.
        if n >= 4 && self.tx_buf[n - 3] == b'#' {
            let packet = std::mem::take(&mut self.tx_buf);
            self.send_packet(packet)?;
            self.unanswered = false;
        }
        Ok(())
    }

    /// Send a whole packet, keeping it for a client NAK.
    fn send_packet(&mut self, packet: Vec<u8>) -> io::Result<()> {
        self.send_raw(&packet)?;
        self.last_sent = packet;
        Ok(())
    }

    /// After `gdbstub` rejected a packet: drop what is left of it, and answer it with an error
    /// if `gdbstub` did not.
    fn recover(&mut self) -> io::Result<()> {
        if self.inbox_pos > 0 {
            self.inbox.pop_front();
            self.inbox_pos = 0;
        }
        self.tx_buf.clear();
        if std::mem::take(&mut self.unanswered) {
            self.send_packet(ERROR_REPLY.to_vec())?;
        }
        Ok(())
    }
}

/// The error reply the link sends itself.
const ERROR_REPLY: &[u8] = b"$E01#a6";

/// Longest `m` read served. GDB asks for at most `PacketSize / 2` bytes per `m` (`remote.c`,
/// `remote_read_bytes_1`); the reply, two hex digits a byte, is then about `MAX_PACKET`
/// bytes, and `gdbstub` reads it in one chunk.
const MAX_READ: u64 = MAX_PACKET as u64 / 2;

/// Whether the link answers `body` with `E01` itself instead of passing it to `gdbstub`. It
/// does for every memory access (`m`, `M`, `X`) except one with a canonical header, `addr,len`
/// with 1 to 8 hex digits each (then `:` and the data for `M` and `X`), whose range ends by
/// 0xFFFF_FFFF and which, for `m`, reads at most `MAX_READ` bytes.
///
/// `gdbstub` 0.7.10 overflows its address on a wrapping read of several chunks (a panic in
/// debug builds), and the link holds a whole reply in memory (8 GiB for `m0,ffffffff`). The
/// check fails closed because `gdbstub` parses more liberally than it does: it accepts any
/// number of leading zeros and ignores fields after the length.
fn refused(body: &[u8]) -> bool {
    let Some((&kind @ (b'm' | b'M' | b'X'), rest)) = body.split_first() else {
        return false;
    };
    let header = if kind == b'm' {
        rest
    } else {
        match rest.iter().position(|&b| b == b':') {
            Some(colon) => &rest[..colon],
            None => return true,
        }
    };
    let Some((addr, len)) = canonical_range(header) else {
        return true;
    };
    let wraps = addr + len > 1 << 32;
    wraps || (kind == b'm' && len > MAX_READ)
}

/// `addr,len` with 1 to 8 hex digits each, and nothing else.
fn canonical_range(header: &[u8]) -> Option<(u64, u64)> {
    let mut fields = header.split(|&b| b == b',');
    let (Some(addr), Some(len), None) = (fields.next(), fields.next(), fields.next()) else {
        return None;
    };
    Some((parse_hex32(addr)?, parse_hex32(len)?))
}

/// A hex number of 1 to 8 digits.
fn parse_hex32(digits: &[u8]) -> Option<u64> {
    if !(1..=8).contains(&digits.len()) {
        return None;
    }
    digits.iter().try_fold(0u64, |n, &c| {
        Some(n << 4 | u64::from(char::from(c).to_digit(16)?))
    })
}

fn checksum(body: &[u8]) -> u8 {
    body.iter().fold(0, |a, b| a.wrapping_add(*b))
}

/// The byte two hex digits spell (either case), if they are hex digits.
fn hex_byte(hi: u8, lo: u8) -> Option<u8> {
    let digit = |c: u8| char::from(c).to_digit(16);
    Some((digit(hi)? << 4 | digit(lo)?) as u8)
}

impl Connection for &mut Link {
    type Error = io::Error;

    fn write(&mut self, byte: u8) -> io::Result<()> {
        self.take_output(byte)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.stream)
    }

    fn on_session_start(&mut self) -> io::Result<()> {
        self.stream.set_nodelay(true)
    }
}

impl ConnectionExt for &mut Link {
    fn read(&mut self) -> io::Result<u8> {
        loop {
            if let Some(b) = self.next_byte() {
                return Ok(b);
            }
            self.fill(true)?;
        }
    }

    fn peek(&mut self) -> io::Result<Option<u8>> {
        if self.inbox.is_empty() {
            self.fill(false)?;
        }
        Ok(self.inbox.front().map(|item| item[self.inbox_pos]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_numbers_follow_the_target_description() {
        // Each `<reg>` in document order, with its number: from `regnum` if given, else the
        // previous number plus one.
        let mut next = 0;
        let mut numbers = Vec::new();
        for reg in TARGET_XML.split("<reg ").skip(1) {
            if let Some(at) = reg.find("regnum=\"") {
                let digits: String = reg[at + 8..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                next = digits.parse().expect("regnum");
            }
            numbers.push(next);
            next += 1;
        }
        assert_eq!(numbers.len(), NUM_REGS);
        for (index, &number) in numbers.iter().enumerate() {
            let (reg, size) = Reg::from_raw_id(number).expect("known register");
            assert_eq!((reg.0, size.map(NonZeroUsize::get)), (index, Some(4)));
        }
        assert!(Reg::from_raw_id(16).is_none());
        assert!(Reg::from_raw_id(24).is_none());
        assert!(Reg::from_raw_id(40).is_none());
    }

    #[test]
    fn refused_memory_accesses() {
        let refused = |body: &str| refused(body.as_bytes());
        // Up to and including the last byte of the address space is fine.
        assert!(!refused("mfffff800,800"));
        assert!(!refused("Mffffffff,1:aa"));
        assert!(!refused("X30000000,0:"));
        assert!(refused("mfffff801,800"));
        assert!(refused("Mffffffff,2:aabb"));
        assert!(refused("Xffffffff,2:,:"), "data may hold `,` and `:`");
        assert!(refused("m0,801"));
        assert!(
            !refused("M30000000,1000:00"),
            "long writes are bounded by the packet"
        );
        assert!(refused("mffffffffffffffff,1"), "16 digits");
        assert!(!refused("M100b0600,2:00Bf"), "upper-case data");
        assert!(!refused("mA,B"), "upper-case hex");
        // Anything but a canonical `addr,len` header (1-8 hex digits each) is refused, since
        // `gdbstub` parses more liberally than this check.
        assert!(refused("m10000000000000000,1"), "17 digits");
        assert!(
            refused("m000000000000000000fffff000,2000"),
            "zero-padded address"
        );
        assert!(
            refused("m30000000,0000000000000000004000"),
            "zero-padded length"
        );
        assert!(refused("m030000000,4"), "9 digits");
        assert!(refused("m30000000,000000004"), "9 digits");
        assert!(refused("m1,2,3"), "a third field");
        assert!(refused("m0,ffffffff,0"), "a third field");
        assert!(refused("M30000000,1,0:aa"), "a third field");
        assert!(refused("mzz,4"));
        assert!(refused("m10"));
        assert!(refused("m,4"));
        assert!(refused("m10,"));
        assert!(refused("m"));
        assert!(refused("m10,4:"), "`m` has no data");
        assert!(refused("M30000000,1"), "no `:` before the data");
        assert!(refused("X+1,1:a"));
        // Not a memory access: left to `gdbstub`.
        assert!(!refused("qSupported:multiprocess+"));
        assert!(!refused("vMustReplyEmpty"));
    }
}
