// tz_roundtrip: TrustZone end to end.
//
// Secure boot programs the SAU (Non-secure flash and RAM, and a Non-secure callable region
// holding an SG veneer), VTOR_NS, MSP_NS and routes IRQ 5 to Non-secure state (ITNS), then
// calls the Non-secure reset handler with BLXNS. Non-secure code enables IRQ 5, calls the
// Secure service through its SG veneer (the service returns with BXNS), reads Secure RAM (a
// SecureFault: the Secure handler records SFSR/SFAR and skips the access) and returns to
// Secure state through FNC_RETURN. Secure Thread code then pends IRQ 5, which preempts it and
// runs in Non-secure state with the Secure registers hidden, and restored afterwards.
// Expected console output:
//
//   S>NS>S ok
//   securefault ok
//   NS irq ok
//
// Failure codes: 0x1xx Secure, 0x2xx Non-secure, 0x3xx SecureFault, 0x4xx interrupt.

#include "semihost.S"

#define IRQ 5
#define PRI_IRQ 0x40

// SFSR.AUVIOL | SFSR.SFARVALID
#define SFSR_AUVIOL_SFARVALID 0x48
// EXC_RETURN for a Secure exception over Non-secure Thread mode (main stack), and for a
// Non-secure exception over Secure Thread mode (main stack, callee registers stacked).
#define EXC_RETURN_S_OVER_NS_THREAD 0xFFFFFFB9
#define EXC_RETURN_NS_OVER_S_THREAD 0xFFFFFFF8

// ======================================================================== Secure

    .section .s_vectors, "a"
    .word __s_stack_top
    .word s_reset
    .rept 5                     // NMI, HardFault, MemManage, BusFault, UsageFault
    .word s_unexpected
    .endr
    .word s_securefault
    .rept 8 + 16                // 8..15, IRQ 0..15
    .word s_unexpected
    .endr

    .section .s_bss, "aw", %nobits
    .balign 4
s_secret:       .space 4
// SecureFault record, in this order.
sf_sfsr:        .space 4        // +0
sf_sfar:        .space 4        // +4
sf_lr:          .space 4        // +8
sf_count:       .space 4        // +12

    .section .s_text, "ax"

FUNC s_reset
    // Report faults as themselves rather than as HardFault.
    LDC r1, SHCSR
    LDC r0, SECUREFAULTENA | USGFAULTENA | BUSFAULTENA
    str r0, [r1]

    // SAU: region 0 Non-secure flash, region 1 Non-secure RAM, region 2 the SG veneers
    // (Non-secure callable); everything else stays Secure.
    SAU_NS_REGIONS
    movs r0, #2
    str r0, [r3]
    LDC r0, __nsc_start
    str r0, [r3, #4]
    LDC r0, __nsc_end - 32
    orr r0, r0, #3              // NSC | ENABLE
    str r0, [r3, #8]
    SAU_ENABLE

    NS_VECTORS
    LDC r1, NVIC_ITNS0
    movs r0, #(1 << IRQ)
    str r0, [r1]

    // ---- S > NS > S ----
    SET_CANARIES
    S_PUTS "S>"
    CALL_NS_RESET
    // Back in Secure state through FNC_RETURN.
    EXPECT r0, 0x5A, 0x101
    CHECK_CANARIES 0x100
    S_PUTS " ok\n"

    // ---- The Non-secure read of Secure RAM was a SecureFault ----
    LDC r4, sf_sfsr
    ldr r0, [r4, #12]
    EXPECT r0, 1, 0x301
    ldr r0, [r4, #0]
    EXPECT r0, SFSR_AUVIOL_SFARVALID, 0x302
    ldr r0, [r4, #4]
    EXPECT r0, s_secret, 0x303
    ldr r0, [r4, #8]
    EXPECT r0, EXC_RETURN_S_OVER_NS_THREAD, 0x304
    LDC r1, SFSR
    ldr r0, [r1]
    EXPECT r0, 0, 0x305
    S_PUTS "securefault ok\n"

    // ---- A Non-secure interrupt pended from Secure Thread mode preempts it ----
    SET_CANARIES
    LDC r0, NVIC_ISPR0
    movs r1, #(1 << IRQ)
    movs r2, #0x22
    movs r3, #0x33
    str r1, [r0]                // taken here
    dsb
    isb
    // The caller-saved registers come back from the frame, r4-r11 from the callee context.
    EXPECT r0, NVIC_ISPR0, 0x401
    EXPECT r1, 1 << IRQ, 0x402
    EXPECT r2, 0x22, 0x403
    EXPECT r3, 0x33, 0x404
    CHECK_CANARIES 0x400
    LDC r4, ns_irq_count
    ldr r0, [r4]
    EXPECT r0, 1, 0x411
    ldr r0, [r4, #4]
    EXPECT r0, EXC_RETURN_NS_OVER_S_THREAD, 0x412
    S_PUTS " ok\n"

    EXIT_PASS

// The Secure service: returns 2 * r0 + 1, to Non-secure state.
FUNC s_service
    push {r4, lr}
    mov r4, r0
    S_PUTS "S"
    lsls r0, r4, #1
    adds r0, #1
    // Do not leak Secure values to Non-secure code.
    movs r1, #0
    movs r2, #0
    movs r3, #0
    mov r12, r1
    pop {r4, lr}
    bxns lr                     // SG cleared LR[0]: return to Non-secure state

// Record SFSR, SFAR and EXC_RETURN, clear SFSR and resume the interrupted Non-secure code
// after the faulting instruction (its frame is on the Non-secure main stack).
FUNC s_securefault
    LDC r1, sf_sfsr
    LDC r2, SFSR
    ldr r3, [r2]
    str r3, [r1, #0]
    str r3, [r2]                // write one to clear
    LDC r2, SFAR
    ldr r3, [r2]
    str r3, [r1, #4]
    str lr, [r1, #8]
    ldr r3, [r1, #12]
    adds r3, #1
    str r3, [r1, #12]
    mrs r0, msp_ns
    SKIP_STACKED_INSN r0
    bx lr

FUNC s_unexpected
    UNEXPECTED_EXCEPTION

    // The only way into Secure state from Non-secure code.
    .section .s_nsc, "ax"
FUNC s_service_veneer
    sg
    b.w s_service

// ======================================================================== Non-secure

    .section .ns_vectors, "a"
ns_vectors:
    .word __ns_stack_top
    .word ns_reset
    .rept 14 + IRQ              // exceptions 2..15, IRQ 0..4
    .word ns_unexpected
    .endr
    .word ns_irq
    .rept 10
    .word ns_unexpected
    .endr

    .section .ns_bss, "aw", %nobits
    .balign 4
ns_irq_count:   .space 4        // +0
ns_irq_lr:      .space 4        // +4

    .section .ns_text, "ax"

// Called from Secure code with BLXNS (LR is FNC_RETURN).
FUNC ns_reset
    push {r4, lr}
    NS_PUTS "NS>"

    // Set up the interrupt Secure code routed here.
    LDC r1, NVIC_IPR0
    movs r0, #PRI_IRQ
    strb r0, [r1, #IRQ]
    LDC r1, NVIC_ISER0
    movs r0, #(1 << IRQ)
    str r0, [r1]

    // Call the Secure service through its veneer (BL cannot reach Secure flash).
    movs r0, #20
    LDC r3, s_service_veneer
    blx r3
    EXPECT r0, 41, 0x201

    // Secure RAM is out of reach: the load faults and is skipped, r0 is not written.
    movs r0, #0
    LDC r1, s_secret
    ldr r0, [r1]
    EXPECT r0, 0, 0x202

    movs r0, #0x5A
    pop {r4, pc}                // FNC_RETURN: back to Secure state

FUNC ns_irq
    // It interrupted Secure Thread mode: no Secure register value is visible.
    orr r12, r12, r0
    orr r12, r12, r1
    orr r12, r12, r2
    orr r12, r12, r3
    orr r12, r12, r4
    orr r12, r12, r5
    orr r12, r12, r6
    orr r12, r12, r7
    orr r12, r12, r8
    orr r12, r12, r9
    orr r12, r12, r10
    orr r12, r12, r11
    cmp r12, #0
    beq 1f
    EXIT_FAIL 0x421
1:  EXPECT lr, EXC_RETURN_NS_OVER_S_THREAD, 0x420
    LDC r1, ns_irq_count
    ldr r0, [r1]
    adds r0, #1
    str r0, [r1]
    str lr, [r1, #4]
    NS_PUTS "NS irq"
    // r4-r11 belong to the Secure context, which exception return restores from the Secure
    // stack: this handler may trash them.
    CLOBBER_CALLEE_SAVED
    bx lr

FUNC ns_unexpected
    UNEXPECTED_EXCEPTION
