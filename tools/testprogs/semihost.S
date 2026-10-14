// Shared macros for the test programs (included, not built on its own): Arm semihosting
// through BKPT 0xAB (operation in r0, argument in r1), self-checks that exit with a failure
// code, and small helpers. The runner in crates/armv8m/tests/programs.rs implements
// SYS_WRITEC, SYS_WRITE0 and SYS_EXIT.

    .syntax unified
    .thumb

#define SYS_WRITEC 0x03
#define SYS_WRITE0 0x04
#define SYS_EXIT 0x18
// SYS_EXIT reason for success; any other reason is a failure code.
#define ADP_STOPPED_APPLICATION_EXIT 0x20026

// Define a Thumb function `name` (its address has bit 0 set).
.macro FUNC name
    .global \name
    .thumb_func
    .type \name, %function
\name:
.endm

// Load a 32-bit constant or address without a literal pool.
.macro LDC reg, val
    movw \reg, #:lower16:(\val)
    movt \reg, #:upper16:(\val)
.endm

// Print the NUL-terminated string `str`, placed in `rodata` (the read-only section of the
// calling code's security state). Clobbers r0, r1.
.macro PUTS rodata, str
    .pushsection \rodata, "a"
9997:
    .asciz "\str"
    .popsection
    LDC r1, 9997b
    movs r0, #SYS_WRITE0
    bkpt 0xab
.endm

.macro S_PUTS str
    PUTS .s_rodata, "\str"
.endm

.macro NS_PUTS str
    PUTS .ns_rodata, "\str"
.endm

// Print the character `ch` (from the stack, so it works in either security state). Leaves
// `ch` in r0.
.macro PUTC ch
    movs r0, #\ch
    push {r0, r1}
    mov r1, sp
    movs r0, #SYS_WRITEC
    bkpt 0xab
    pop {r0, r1}
.endm

.macro EXIT_PASS
    movs r0, #SYS_EXIT
    LDC r1, ADP_STOPPED_APPLICATION_EXIT
    bkpt 0xab
.endm

// Exit with failure `code`.
.macro EXIT_FAIL code
    LDC r1, \code
    movs r0, #SYS_EXIT
    bkpt 0xab
.endm

// Unless `reg` <cond> `val` (unsigned conditions: eq, ne, hs, lo, ...), exit with failure
// `code`. Clobbers r12 and the flags.
.macro CHECK cond, reg, val, code
    LDC r12, \val
    cmp \reg, r12
    b\cond 9998f
    EXIT_FAIL \code
9998:
.endm

.macro EXPECT reg, val, code
    CHECK eq, \reg, \val, \code
.endm

// Fail with 0xBAD0nn for an unexpected exception nn.
.macro UNEXPECTED_EXCEPTION
    mrs r1, ipsr
    LDC r0, 0xBAD000
    orrs r1, r0
    movs r0, #SYS_EXIT
    bkpt 0xab
.endm

// Advance the stacked return address in the exception frame at `frame` past the instruction
// it points at (16 or 32 bits). Clobbers r1, r2 and the flags.
.macro SKIP_STACKED_INSN frame
    ldr r1, [\frame, #24]
    ldrh r2, [r1]
    adds r1, #2
    lsrs r2, r2, #11
    cmp r2, #0x1D               // first halfword 0b11101..., 0b11110... or 0b11111...
    it hs
    addhs r1, #2
    str r1, [\frame, #24]
.endm

// Callee-saved registers r4-r11 set to recognisable values, and checked (code + 4..11).
.macro SET_CANARIES
    LDC r4, 0xC0DE0004
    LDC r5, 0xC0DE0005
    LDC r6, 0xC0DE0006
    LDC r7, 0xC0DE0007
    LDC r8, 0xC0DE0008
    LDC r9, 0xC0DE0009
    LDC r10, 0xC0DE000A
    LDC r11, 0xC0DE000B
.endm

.macro CHECK_CANARIES code
    EXPECT r4, 0xC0DE0004, \code + 4
    EXPECT r5, 0xC0DE0005, \code + 5
    EXPECT r6, 0xC0DE0006, \code + 6
    EXPECT r7, 0xC0DE0007, \code + 7
    EXPECT r8, 0xC0DE0008, \code + 8
    EXPECT r9, 0xC0DE0009, \code + 9
    EXPECT r10, 0xC0DE000A, \code + 10
    EXPECT r11, 0xC0DE000B, \code + 11
.endm

// Overwrite r4-r11 (without saving them).
.macro CLOBBER_CALLEE_SAVED
    LDC r4, 0xDEADBEEF
    mov r5, r4
    mov r6, r4
    mov r7, r4
    mov r8, r4
    mov r9, r4
    mov r10, r4
    mov r11, r4
.endm

// System Control Space registers.
#define ICSR 0xE000ED04
#define VTOR 0xE000ED08
#define CCR 0xE000ED14
#define SHPR2 0xE000ED1C
#define SHPR3 0xE000ED20
#define SHCSR 0xE000ED24
#define CFSR 0xE000ED28
#define HFSR 0xE000ED2C
#define BFAR 0xE000ED38
#define SAU_CTRL 0xE000EDD0
#define SAU_RNR 0xE000EDD8
#define SFSR 0xE000EDE4
#define SFAR 0xE000EDE8
#define SYST_CSR 0xE000E010
#define NVIC_ISER0 0xE000E100
#define NVIC_ISPR0 0xE000E200
#define NVIC_ITNS0 0xE000E380
#define NVIC_IPR0 0xE000E400
// Offset of the Non-secure alias of the SCS (Secure state only).
#define SCS_NS_ALIAS 0x00020000

#define PENDSVSET (1 << 28)
#define PENDSTSET (1 << 26)
#define MEMFAULTENA (1 << 16)
#define BUSFAULTENA (1 << 17)
#define USGFAULTENA (1 << 18)
#define SECUREFAULTENA (1 << 19)
#define DIV_0_TRP (1 << 4)
// SYST_CSR: ENABLE | TICKINT | CLKSOURCE (processor clock).
#define SYST_ON 7

// EXC_RETURN.S: the interrupted context is Secure.
#define EXC_RETURN_S (1 << 6)

// Program the SAU (Secure state): region 0 Non-secure flash, region 1 Non-secure RAM, then
// enable it. Everything else stays Secure. Clobbers r0, r1, r3.
.macro SAU_NS_REGIONS
    LDC r3, SAU_RNR             // RNR, RBAR, RLAR follow at +4, +8
    movs r0, #0
    str r0, [r3]
    LDC r0, __ns_flash_start
    str r0, [r3, #4]
    LDC r0, __ns_flash_end - 32 // RLAR holds the base of the last 32-byte granule
    orr r0, r0, #1              // ENABLE
    str r0, [r3, #8]
    movs r0, #1
    str r0, [r3]
    LDC r0, __ns_ram_start
    str r0, [r3, #4]
    LDC r0, __ns_ram_end - 32
    orr r0, r0, #1
    str r0, [r3, #8]
.endm

.macro SAU_ENABLE
    LDC r1, SAU_CTRL
    movs r0, #1
    str r0, [r1]
    dsb
    isb
.endm

// Point VTOR_NS at the Non-secure vector table `ns_vectors` and load MSP_NS from its first
// word. Clobbers r0, r1.
.macro NS_VECTORS
    LDC r1, VTOR + SCS_NS_ALIAS
    LDC r0, ns_vectors
    str r0, [r1]
    ldr r0, [r0]
    msr msp_ns, r0
.endm

// Call the Non-secure reset handler of `ns_vectors` with BLXNS (target bit 0 clear selects
// Non-secure state; LR becomes FNC_RETURN). Clobbers r0 and the caller-saved registers.
.macro CALL_NS_RESET
    LDC r0, ns_vectors
    ldr r0, [r0, #4]
    bic r0, r0, #1
    blxns r0
.endm
