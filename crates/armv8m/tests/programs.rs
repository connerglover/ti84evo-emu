//! End-to-end runs of assembled firmware-style programs (`tools/testprogs`): vector tables,
//! exception handlers, SAU setup, SG veneers, BLXNS/BXNS and Non-secure interrupts, on a
//! TrustZone memory map, with a semihosting runner for console output and the exit status.
//!
//! The images in `tools/testprogs/out` are built from the `.S` sources by `build.sh` or
//! `build.ps1` (Arm GNU Toolchain) and committed, so these tests need no toolchain. CI rebuilds
//! them and checks that the committed images are up to date.

use armv8m::{Access, Bus, BusError, Cpu, Sec, StepResult};

// ---- Memory map (must match tools/testprogs/link.ld) ----

const S_FLASH: u32 = 0x1000_0000;
const NS_FLASH: u32 = 0x0020_0000;
const NS_RAM: u32 = 0x2000_0000;
const S_RAM: u32 = 0x3000_0000;
const REGION_SIZE: usize = 0x1_0000;

/// Step budget per program: far above what any of them needs.
const MAX_STEPS: u64 = 10_000_000;

// ---- Semihosting (BKPT 0xAB, operation in r0, argument in r1) ----

const SEMIHOSTING_BKPT: u8 = 0xAB;
const SYS_WRITEC: u32 = 0x03;
const SYS_WRITE0: u32 = 0x04;
const SYS_EXIT: u32 = 0x18;
/// `ADP_Stopped_ApplicationExit`: the SYS_EXIT reason that means success.
const EXIT_PASS: u32 = 0x2_0026;
/// Longest string SYS_WRITE0 prints (guards against a missing terminator).
const MAX_WRITE0: u32 = 4096;

// ---- Test-only bus ----

struct Region {
    base: u32,
    mem: Vec<u8>,
    /// Secure memory: like an MPC, the bus rejects Non-secure transactions to it. The core's
    /// attribution must stop those first, so a rejection here exposes a core bug.
    secure: bool,
    writable: bool,
}

/// Secure flash, Non-secure flash, Non-secure RAM and Secure RAM, 64 KiB each.
struct ProgBus {
    regions: Vec<Region>,
}

impl ProgBus {
    fn new() -> Self {
        let region = |base, secure, writable| Region {
            base,
            mem: vec![0; REGION_SIZE],
            secure,
            writable,
        };
        Self {
            regions: vec![
                region(S_FLASH, true, false),
                region(NS_FLASH, false, false),
                region(NS_RAM, false, true),
                region(S_RAM, true, true),
            ],
        }
    }

    /// The region holding all of `addr..addr + len`, and the offset of `addr` in it.
    fn find(&mut self, addr: u32, len: u32) -> Option<(&mut Region, usize)> {
        self.regions.iter_mut().find_map(|r| {
            let off = addr.checked_sub(r.base)? as usize;
            (off + len as usize <= r.mem.len()).then_some((r, off))
        })
    }

    fn load(&mut self, addr: u32, image: &[u8]) {
        let (r, off) = self
            .find(addr, image.len() as u32)
            .unwrap_or_else(|| panic!("image of {} bytes does not fit at {addr:#x}", image.len()));
        r.mem[off..off + image.len()].copy_from_slice(image);
    }

    /// Host (semihosting) read: no security or permission checks.
    fn peek8(&mut self, addr: u32) -> Option<u8> {
        self.find(addr, 1).map(|(r, off)| r.mem[off])
    }

    fn access(
        &mut self,
        addr: u32,
        n: u32,
        a: Access,
        write: Option<u32>,
    ) -> Result<u32, BusError> {
        let (r, off) = self.find(addr, n).ok_or(BusError::Decode)?;
        if r.secure && !a.secure {
            return Err(BusError::SecureViolation);
        }
        let bytes = &mut r.mem[off..off + n as usize];
        match write {
            Some(_) if !r.writable => Err(BusError::Decode),
            Some(v) => {
                for (i, b) in bytes.iter_mut().enumerate() {
                    *b = (v >> (8 * i)) as u8;
                }
                Ok(0)
            }
            None => Ok(bytes
                .iter()
                .enumerate()
                .fold(0, |v, (i, &b)| v | u32::from(b) << (8 * i))),
        }
    }
}

impl Bus for ProgBus {
    fn read8(&mut self, addr: u32, a: Access) -> Result<u8, BusError> {
        self.access(addr, 1, a, None).map(|v| v as u8)
    }
    fn read16(&mut self, addr: u32, a: Access) -> Result<u16, BusError> {
        self.access(addr, 2, a, None).map(|v| v as u16)
    }
    fn read32(&mut self, addr: u32, a: Access) -> Result<u32, BusError> {
        self.access(addr, 4, a, None)
    }
    fn write8(&mut self, addr: u32, v: u8, a: Access) -> Result<(), BusError> {
        self.access(addr, 1, a, Some(u32::from(v))).map(drop)
    }
    fn write16(&mut self, addr: u32, v: u16, a: Access) -> Result<(), BusError> {
        self.access(addr, 2, a, Some(u32::from(v))).map(drop)
    }
    fn write32(&mut self, addr: u32, v: u32, a: Access) -> Result<(), BusError> {
        self.access(addr, 4, a, Some(v)).map(drop)
    }
}

// ---- Runner ----

/// Registers and fault status, for failure reports.
fn dump(cpu: &Cpu) -> String {
    let r = &cpu.regs;
    let scb = &cpu.scs.scb;
    let mut s = format!(
        "pc={:#010x} {:?} xpsr={:#010x} lr={:#010x} locked_up={}\n",
        r.pc,
        r.sec,
        r.xpsr.to_u32(),
        r.lr,
        cpu.locked_up()
    );
    for (i, v) in r.r.iter().enumerate() {
        s += &format!("r{i}={v:#010x} ");
    }
    let cfsr = |sec: Sec| {
        u32::from(*scb.mmfsr.get(sec))
            | (u32::from(scb.bfsr) << 8)
            | (u32::from(*scb.ufsr.get(sec)) << 16)
    };
    s += &format!(
        "\nmsp_s={:#010x} psp_s={:#010x} msp_ns={:#010x} psp_ns={:#010x} control_s={:#x} \
         control_ns={:#x}\ncfsr_s={:#010x} cfsr_ns={:#010x} hfsr={:#010x} bfar={:#010x} \
         sfsr={:#x} sfar={:#010x}",
        r.msp.s,
        r.psp.s,
        r.msp.ns,
        r.psp.ns,
        r.control.s,
        r.control.ns,
        cfsr(Sec::Secure),
        cfsr(Sec::NonSecure),
        scb.hfsr,
        scb.bfar,
        cpu.scs.sau.sfsr,
        cpu.scs.sau.sfar,
    );
    s
}

/// Run a program: the Secure image at `S_FLASH` (its vector table first), the optional
/// Non-secure image at `NS_FLASH`. Returns the console output when the program exits with
/// `ADP_Stopped_ApplicationExit`; any other exit, a lockup, an unexpected breakpoint or running
/// out of steps is an error describing the state.
fn run(s_image: &[u8], ns_image: Option<&[u8]>, max_steps: u64) -> Result<String, String> {
    let mut bus = ProgBus::new();
    bus.load(S_FLASH, s_image);
    if let Some(ns) = ns_image {
        bus.load(NS_FLASH, ns);
    }
    let mut cpu = Cpu::new();
    cpu.set_vtor(Sec::Secure, S_FLASH);
    cpu.reset(&mut bus);

    let mut console = String::new();
    let fail = |why: String, cpu: &Cpu, console: &str| {
        Err(format!("{why}\n{}\nconsole so far:\n{console}", dump(cpu)))
    };
    for _ in 0..max_steps {
        match cpu.step(&mut bus) {
            // WFI/WFE complete immediately: every program sleeps in a loop that rechecks its
            // condition, and the timers keep counting the loop's cycles.
            StepResult::Executed { .. } | StepResult::Sleep => {}
            StepResult::Bkpt(SEMIHOSTING_BKPT) => {
                let (op, arg) = (cpu.regs.r[0], cpu.regs.r[1]);
                match op {
                    SYS_WRITEC => match bus.peek8(arg) {
                        Some(c) => console.push(char::from(c)),
                        None => return fail(format!("SYS_WRITEC of {arg:#x}"), &cpu, &console),
                    },
                    SYS_WRITE0 => {
                        let mut addr = arg;
                        loop {
                            match bus.peek8(addr) {
                                Some(0) => break,
                                Some(c) if addr.wrapping_sub(arg) < MAX_WRITE0 => {
                                    console.push(char::from(c))
                                }
                                _ => {
                                    let why = format!("SYS_WRITE0 of {arg:#x}: bad string");
                                    return fail(why, &cpu, &console);
                                }
                            }
                            addr = addr.wrapping_add(1);
                        }
                    }
                    SYS_EXIT if arg == EXIT_PASS => return Ok(console),
                    SYS_EXIT => return fail(format!("exit code {arg:#x}"), &cpu, &console),
                    _ => return fail(format!("semihosting op {op:#x}"), &cpu, &console),
                }
                cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            }
            StepResult::Bkpt(imm) => return fail(format!("BKPT {imm:#x}"), &cpu, &console),
            StepResult::Lockup => return fail("lockup".into(), &cpu, &console),
        }
    }
    fail(format!("no exit after {max_steps} steps"), &cpu, &console)
}

fn run_ok(s_image: &[u8], ns_image: Option<&[u8]>) -> String {
    run(s_image, ns_image, MAX_STEPS).unwrap_or_else(|e| panic!("{e}"))
}

// ---- Runner self-checks (hand-assembled images) ----

/// A Secure image: vector table (MSP at the top of Secure RAM, reset at 0x40) and `code` there.
fn tiny_image(code: &[u16]) -> Vec<u8> {
    let mut image = vec![0u8; 0x40];
    image[0..4].copy_from_slice(&(S_RAM + REGION_SIZE as u32).to_le_bytes());
    image[4..8].copy_from_slice(&(S_FLASH + 0x41).to_le_bytes());
    image.extend(code.iter().flat_map(|hw| hw.to_le_bytes()));
    image
}

#[test]
fn runner_reports_semihosting_output_and_pass() {
    let mut image = tiny_image(&[
        0xA102, // 0x40: adr r1, 0x4C (the string)
        0x2004, // 0x42: movs r0, #4 (SYS_WRITE0)
        0xBEAB, // 0x44: bkpt 0xab
        0x2018, // 0x46: movs r0, #0x18 (SYS_EXIT)
        0x4901, // 0x48: ldr r1, [pc, #4] (0x50: the exit reason)
        0xBEAB, // 0x4A: bkpt 0xab
    ]);
    image.extend(b"hi\n\0");
    image.extend(EXIT_PASS.to_le_bytes());
    assert_eq!(run(&image, None, 100), Ok("hi\n".to_string()));
}

#[test]
fn runner_reports_failure_exit() {
    // movs r0, #0x18; movs r1, #1; bkpt 0xab
    let image = tiny_image(&[0x2018, 0x2101, 0xBEAB]);
    let err = run(&image, None, 100).unwrap_err();
    assert!(err.starts_with("exit code 0x1\n"), "{err}");
}

#[test]
fn runner_reports_runaway() {
    // b .
    let image = tiny_image(&[0xE7FE]);
    let err = run(&image, None, 1000).unwrap_err();
    assert!(err.starts_with("no exit after 1000 steps"), "{err}");
}

#[test]
fn runner_reports_lockup() {
    // udf #0 with no handlers: HardFault's vector is 0, so its first fetch faults INVSTATE
    // inside HardFault -> lockup.
    let image = tiny_image(&[0xDE00]);
    let err = run(&image, None, 100).unwrap_err();
    assert!(err.starts_with("lockup\n"), "{err}");
}

// ---- Programs ----

const EXC_BASIC: &[u8] = include_bytes!("../../../tools/testprogs/out/exc_basic.bin");
const TZ_ROUNDTRIP: &[u8] = include_bytes!("../../../tools/testprogs/out/tz_roundtrip.bin");
const TZ_ROUNDTRIP_NS: &[u8] = include_bytes!("../../../tools/testprogs/out/tz_roundtrip_ns.bin");
const SYSTICK: &[u8] = include_bytes!("../../../tools/testprogs/out/systick.bin");
const SYSTICK_NS: &[u8] = include_bytes!("../../../tools/testprogs/out/systick_ns.bin");

/// Secure only: SVC (arguments and return value through the stacked frame, EXC_RETURN on the
/// process stack), PendSV, tail-chaining in priority order, nested preemption, and the fault
/// handlers reading CFSR/HFSR/BFAR (UsageFault UNDEFINSTR and DIVBYZERO, precise BusFault,
/// escalation to HardFault).
#[test]
fn exc_basic() {
    assert_eq!(
        run_ok(EXC_BASIC, None),
        "svc ok\n\
         pendsv ok\n\
         STP tail-chain ok\n\
         PTp nested ok\n\
         usagefault ok\n\
         divbyzero ok\n\
         busfault ok\n\
         hardfault ok\n"
    );
}

/// Secure boot configures the SAU (Non-secure code and RAM, a Non-secure callable veneer
/// region), VTOR_NS, MSP_NS and ITNS, then calls Non-secure code with BLXNS. Non-secure code
/// calls back through the SG veneer (the Secure function returns with BXNS), takes a
/// SecureFault for touching Secure RAM, and returns with FNC_RETURN. Secure thread code then
/// pends a Non-secure interrupt, which preempts it and runs in Non-secure state with the Secure
/// registers hidden and restored.
#[test]
fn tz_roundtrip() {
    assert_eq!(
        run_ok(TZ_ROUNDTRIP, Some(TZ_ROUNDTRIP_NS)),
        "S>NS>S ok\n\
         securefault ok\n\
         NS irq ok\n"
    );
}

/// Both SysTick instances fire: the Secure one while Secure and while Non-secure code runs,
/// the Non-secure one while Non-secure and while Secure code runs (the Secure registers hidden
/// from it and restored after it). Then the two tail-chain into each other over Secure code,
/// both ways (Non-secure to Secure with EXC_RETURN.DCRS = 0).
#[test]
fn systick() {
    assert_eq!(
        run_ok(SYSTICK, Some(SYSTICK_NS)),
        "S tick ok\n\
         NS tick ok\n\
         both banks ok\n\
         NS over S ok\n\
         tail-chains ok\n"
    );
}
