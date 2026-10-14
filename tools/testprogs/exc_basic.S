// exc_basic: the exception model in Secure state only (the SAU stays disabled).
//
// Thread code runs on the process stack. It checks SVC (arguments and result through the
// stacked frame, EXC_RETURN), a software-pended PendSV, tail-chaining of two exceptions pended
// inside a handler (taken in priority order, not pend order, each returning straight to Thread
// mode), nested preemption, and the fault handlers reading CFSR/HFSR/BFAR: UsageFault
// UNDEFINSTR and DIVBYZERO, a precise BusFault, and UNDEFINSTR escalated to HardFault.
// Expected console output:
//
//   svc ok / pendsv ok / STP tail-chain ok / PTp nested ok / usagefault ok / divbyzero ok /
//   busfault ok / hardfault ok   (one per line)
//
// Failure codes: 0x1xx SVC, 0x2xx PendSV, 0x3xx tail-chain, 0x4xx nesting, 0x5xx-0x8xx faults.

#include "semihost.S"

// Priorities (3 implemented bits; lower is more urgent): SVCall > SysTick > PendSV.
#define PRI_SVCALL 0x40
#define PRI_SYSTICK 0x80
#define PRI_PENDSV 0xC0

// An address no memory answers.
#define UNMAPPED 0x04000010

// EXC_RETURN to Secure Thread mode on the process stack, and to Secure Handler mode.
#define EXC_RETURN_S_THREAD_PSP 0xFFFFFFFD
#define EXC_RETURN_S_HANDLER 0xFFFFFFF1

// Handler behaviour: QUIET just counts; TAILCHAIN and NESTED also print.
#define QUIET 0
#define TAILCHAIN 1
#define NESTED 2

    .section .s_vectors, "a"
    .word __s_stack_top
    .word s_reset
    .word unexpected            // NMI
    .word fault_handler         // HardFault
    .word unexpected            // MemManage
    .word fault_handler         // BusFault
    .word fault_handler         // UsageFault
    .word unexpected            // SecureFault
    .word 0, 0, 0
    .word svc_handler
    .word unexpected            // DebugMonitor
    .word 0
    .word pendsv_handler
    .word systick_handler

    .section .s_bss, "aw", %nobits
    .balign 4
phase:          .space 4
pendsv_count:   .space 4
systick_count:  .space 4
// EXC_RETURN each handler last ran with.
pendsv_lr:      .space 4
systick_lr:     .space 4
// Fault record, in this order (offsets used by fault_handler and EXPECT_FAULT).
fault_cfsr:     .space 4        // +0
fault_hfsr:     .space 4        // +4
fault_bfar:     .space 4        // +8
fault_ipsr:     .space 4        // +12
fault_count:    .space 4        // +16

    .section .s_text, "ax"

// Check the fault record (and that the handler cleared CFSR and HFSR). Clobbers r0, r1, r4.
.macro EXPECT_FAULT ipsr, cfsr, hfsr, count, code
    LDC r4, fault_cfsr
    ldr r0, [r4, #0]
    EXPECT r0, \cfsr, \code + 0
    ldr r0, [r4, #4]
    EXPECT r0, \hfsr, \code + 1
    ldr r0, [r4, #12]
    EXPECT r0, \ipsr, \code + 2
    ldr r0, [r4, #16]
    EXPECT r0, \count, \code + 3
    LDC r1, CFSR
    ldr r0, [r1]
    EXPECT r0, 0, \code + 4
    LDC r1, HFSR
    ldr r0, [r1]
    EXPECT r0, 0, \code + 5
.endm

.macro SET_PHASE value
    LDC r1, phase
    movs r0, #\value
    str r0, [r1]
.endm

.macro EXPECT_COUNT var, value, code
    LDC r1, \var
    ldr r0, [r1]
    EXPECT r0, \value, \code
.endm

.macro PEND bits
    LDC r1, ICSR
    LDC r0, \bits
    str r0, [r1]
    dsb
    isb
.endm

FUNC s_reset
    // Thread mode on the process stack, as under an RTOS.
    LDC r0, __s_psp_top
    msr psp, r0
    movs r0, #2                 // CONTROL.SPSEL
    msr control, r0
    isb

    LDC r1, SHPR2
    movs r0, #PRI_SVCALL
    strb r0, [r1, #3]           // SHPR2[31:24]: SVCall
    LDC r1, SHPR3
    movs r0, #PRI_PENDSV
    strb r0, [r1, #2]           // SHPR3[23:16]: PendSV
    movs r0, #PRI_SYSTICK
    strb r0, [r1, #3]           // SHPR3[31:24]: SysTick

    // ---- SVC: the handler adds the SVC number to the caller's r0 ----
    LDC r0, 0x1234
    SET_CANARIES
    svc #0x2A
    EXPECT r0, 0x125E, 0x101
    CHECK_CANARIES 0x100
    S_PUTS "svc ok\n"

    // ---- PendSV: pended by software, taken at once from Thread mode ----
    PEND PENDSVSET
    EXPECT_COUNT pendsv_count, 1, 0x201
    S_PUTS "pendsv ok\n"

    // ---- Tail-chaining: the SVC handler pends PendSV, then SysTick; both have a lower
    // priority than SVCall, so they wait for it to return and are then tail-chained, SysTick
    // first. Each handler prints its letter. Both return straight to Thread mode: neither
    // was nested in another handler (as SysTick would be if PendSV were wrongly taken first).
    SET_PHASE TAILCHAIN
    svc #1
    EXPECT_COUNT pendsv_count, 2, 0x301
    EXPECT_COUNT systick_count, 1, 0x302
    EXPECT_COUNT systick_lr, EXC_RETURN_S_THREAD_PSP, 0x303
    EXPECT_COUNT pendsv_lr, EXC_RETURN_S_THREAD_PSP, 0x304
    S_PUTS " tail-chain ok\n"

    // ---- Nesting: the PendSV handler pends SysTick, which preempts it at once ----
    SET_PHASE NESTED
    PEND PENDSVSET
    EXPECT_COUNT pendsv_count, 3, 0x401
    EXPECT_COUNT systick_count, 2, 0x402
    EXPECT_COUNT systick_lr, EXC_RETURN_S_HANDLER, 0x403
    EXPECT_COUNT pendsv_lr, EXC_RETURN_S_THREAD_PSP, 0x404
    S_PUTS " nested ok\n"
    SET_PHASE QUIET

    // ---- Faults: the handler records CFSR, HFSR, BFAR and IPSR, clears the status and
    // resumes after the faulting instruction.
    LDC r1, SHCSR
    LDC r0, USGFAULTENA | BUSFAULTENA
    str r0, [r1]
    LDC r1, CCR
    ldr r0, [r1]
    orr r0, r0, #DIV_0_TRP
    str r0, [r1]
    isb

    udf #0
    EXPECT_FAULT 6, 0x00010000, 0, 1, 0x500         // UsageFault, UFSR.UNDEFINSTR
    S_PUTS "usagefault ok\n"

    movs r1, #7
    movs r2, #0
    sdiv r0, r1, r2
    EXPECT_FAULT 6, 0x02000000, 0, 2, 0x600         // UsageFault, UFSR.DIVBYZERO
    S_PUTS "divbyzero ok\n"

    LDC r1, UNMAPPED
    ldr r0, [r1]
    EXPECT_FAULT 5, 0x00008200, 0, 3, 0x700         // BusFault, BFSR.BFARVALID | PRECISERR
    LDC r4, fault_bfar
    ldr r0, [r4]
    EXPECT r0, UNMAPPED, 0x710
    S_PUTS "busfault ok\n"

    // With UsageFault disabled the same fault escalates to HardFault (HFSR.FORCED); a 32-bit
    // UDF this time.
    LDC r1, SHCSR
    LDC r0, BUSFAULTENA
    str r0, [r1]
    isb
    udf.w #1
    EXPECT_FAULT 3, 0x00010000, 0x40000000, 4, 0x800
    S_PUTS "hardfault ok\n"

    EXIT_PASS

// SVC #n: n = 1 starts the tail-chain test; otherwise the caller's r0 += n.
FUNC svc_handler
    EXPECT lr, EXC_RETURN_S_THREAD_PSP, 0x110
    mrs r0, psp
    ldr r1, [r0, #24]           // stacked return address: just after the SVC
    ldrb r2, [r1, #-2]          // the SVC immediate
    cmp r2, #1
    beq 1f
    ldr r3, [r0]
    add r3, r3, r2
    str r3, [r0]
    bx lr
1:  PUTC 'S'
    LDC r1, ICSR
    LDC r0, PENDSVSET
    str r0, [r1]
    LDC r0, PENDSTSET
    str r0, [r1]
    dsb
    isb
    // Both stay pending until this handler returns.
    EXPECT_COUNT pendsv_count, 1, 0x311
    EXPECT_COUNT systick_count, 0, 0x312
    bx lr

FUNC pendsv_handler
    LDC r1, pendsv_count
    ldr r0, [r1]
    adds r0, #1
    str r0, [r1]
    LDC r1, pendsv_lr
    str lr, [r1]
    LDC r2, phase
    ldr r2, [r2]
    cmp r2, #QUIET
    beq 1f
    PUTC 'P'
    cmp r2, #NESTED
    bne 1f
    PEND PENDSTSET              // SysTick preempts here
    EXPECT_COUNT systick_count, 2, 0x411
    PUTC 'p'
1:  bx lr

FUNC systick_handler
    LDC r1, systick_count
    ldr r0, [r1]
    adds r0, #1
    str r0, [r1]
    LDC r1, systick_lr
    str lr, [r1]
    LDC r2, phase
    ldr r2, [r2]
    cmp r2, #QUIET
    beq 1f
    PUTC 'T'
1:  bx lr

// HardFault, BusFault and UsageFault.
FUNC fault_handler
    tst lr, #4                  // EXC_RETURN.SPSEL: which stack holds the frame
    ite eq
    mrseq r0, msp
    mrsne r0, psp
    LDC r1, fault_cfsr
    LDC r2, CFSR
    ldr r3, [r2]
    str r3, [r1, #0]
    str r3, [r2]                // write one to clear
    LDC r2, HFSR
    ldr r3, [r2]
    str r3, [r1, #4]
    str r3, [r2]
    LDC r2, BFAR
    ldr r3, [r2]
    str r3, [r1, #8]
    mrs r3, ipsr
    str r3, [r1, #12]
    ldr r3, [r1, #16]
    adds r3, #1
    str r3, [r1, #16]
    SKIP_STACKED_INSN r0
    bx lr

FUNC unexpected
    UNEXPECTED_EXCEPTION
