// systick: both SysTick instances fire, and their exceptions meet.
//
// Secure code starts the Secure SysTick and waits for two ticks, then calls Non-secure code
// with BLXNS. That starts the Non-secure SysTick (through its own view of SYST_CSR) and waits
// for three ticks while the Secure SysTick keeps interrupting it. Back in Secure state, Secure
// Thread code waits for three Non-secure ticks taken over it: the Non-secure handler sees none
// of the Secure registers and trashes r4-r11, which exception return restores.
//
// Then the two (equal-priority) SysTicks are made to tail-chain across the security states
// over Secure Thread mode. Secure to Non-secure: the Secure handler lingers until the
// Non-secure SysTick is pending, so the callee registers are stacked on that tail-chain.
// Non-secure to Secure: the Non-secure handler lingers past a Secure tick, which is then
// tail-chained with EXC_RETURN.DCRS = 0 (the callee registers are already stacked) and
// unstacks them on return. Expected console output:
//
//   S tick ok
//   NS tick ok
//   both banks ok
//   NS over S ok
//   tail-chains ok
//
// Failure codes: 0x1xx Secure, 0x2xx Non-secure.

#include "semihost.S"

#define S_RELOAD 499
#define NS_RELOAD 699
// Iterations of the Non-secure handler's linger loop (at least two cycles each): longer than a
// Secure SysTick period.
#define NS_LINGER_LOOPS 1000

#define EXC_RETURN_DCRS (1 << 5)

// ======================================================================== Secure

    .section .s_vectors, "a"
    .word __s_stack_top
    .word s_reset
    .rept 13                    // 2..14
    .word s_unexpected
    .endr
    .word s_systick
    .rept 16                    // IRQ 0..15
    .word s_unexpected
    .endr

    .section .s_bss, "aw", %nobits
    .balign 4
s_ticks:        .space 4
s_ticks_over_ns: .space 4
// Ticks tail-chained from a Non-secure exception over Secure code (EXC_RETURN.DCRS = 0).
s_ticks_dcrs0:  .space 4
// Set by Thread code: the next tick over Secure Thread mode lingers until the Non-secure
// SysTick is pending. s_lingered counts those.
s_linger:       .space 4
s_lingered:     .space 4

    .section .s_text, "ax"

// Start the SysTick of the current security state with reload value `reload`.
.macro SYSTICK_START reload
    LDC r1, SYST_CSR
    LDC r0, \reload
    str r0, [r1, #4]            // SYST_RVR
    str r0, [r1, #8]            // SYST_CVR: any write clears it
    movs r0, #SYST_ON
    str r0, [r1]
.endm

// Sleep until the word at `var` is at least `count`. Clobbers r0, r1.
.macro WAIT_FOR var, count
    LDC r1, \var
9996:
    wfi
    ldr r0, [r1]
    cmp r0, #\count
    blo 9996b
.endm

.macro SET_WORD var, value
    LDC r1, \var
    movs r0, #\value
    str r0, [r1]
.endm

// Add one to the word at `var`. Clobbers r0, r1.
.macro COUNT var
    LDC r1, \var
    ldr r0, [r1]
    adds r0, #1
    str r0, [r1]
.endm

FUNC s_reset
    LDC r1, SHCSR
    LDC r0, SECUREFAULTENA | USGFAULTENA | BUSFAULTENA
    str r0, [r1]
    SAU_NS_REGIONS
    SAU_ENABLE
    NS_VECTORS

    SYSTICK_START S_RELOAD
    WAIT_FOR s_ticks, 2
    S_PUTS "S tick ok\n"

    CALL_NS_RESET
    LDC r1, ns_ticks
    ldr r0, [r1]
    CHECK hs, r0, 3, 0x101
    LDC r1, s_ticks_over_ns
    ldr r0, [r1]
    CHECK hs, r0, 1, 0x102
    S_PUTS "both banks ok\n"

    SET_CANARIES
    WAIT_FOR ns_ticks_over_s, 3
    CHECK_CANARIES 0x110
    S_PUTS "NS over S ok\n"

    // Secure -> Non-secure tail-chain.
    SET_WORD s_linger, 1
    WAIT_FOR s_lingered, 1
    // Non-secure -> Secure tail-chain.
    SET_WORD ns_linger, 1
    WAIT_FOR s_ticks_dcrs0, 1
    CHECK_CANARIES 0x120
    S_PUTS "tail-chains ok\n"

    EXIT_PASS

FUNC s_systick
    COUNT s_ticks
    and r0, lr, #(EXC_RETURN_S | EXC_RETURN_DCRS)
    cmp r0, #EXC_RETURN_S
    bne 1f
    COUNT s_ticks_dcrs0
1:  tst lr, #EXC_RETURN_S
    bne 2f
    COUNT s_ticks_over_ns
    bx lr
2:  // Over Secure code. Linger once on request (not on a tail-chain from Non-secure).
    tst lr, #EXC_RETURN_DCRS
    beq 3f
    LDC r1, s_linger
    ldr r0, [r1]
    cmp r0, #0
    beq 3f
    movs r0, #0
    str r0, [r1]
    LDC r1, ICSR + SCS_NS_ALIAS // the Non-secure view: PENDSTSET is the Non-secure SysTick's
4:  ldr r0, [r1]
    tst r0, #PENDSTSET
    beq 4b
    COUNT s_lingered
3:  bx lr

FUNC s_unexpected
    UNEXPECTED_EXCEPTION

// ======================================================================== Non-secure

    .section .ns_vectors, "a"
ns_vectors:
    .word __ns_stack_top
    .word ns_reset
    .rept 13                    // 2..14
    .word ns_unexpected
    .endr
    .word ns_systick
    .rept 16                    // IRQ 0..15
    .word ns_unexpected
    .endr

    .section .ns_bss, "aw", %nobits
    .balign 4
ns_ticks:       .space 4
ns_ticks_over_s: .space 4
// Set by Secure Thread code: the next tick over Secure code lingers past a Secure tick.
ns_linger:      .space 4

    .section .ns_text, "ax"

// Called from Secure code with BLXNS (LR is FNC_RETURN).
FUNC ns_reset
    push {r4, lr}
    SYSTICK_START NS_RELOAD
    WAIT_FOR ns_ticks, 3
    NS_PUTS "NS tick ok\n"
    pop {r4, pc}

FUNC ns_systick
    tst lr, #EXC_RETURN_S
    beq 1f
    // Over Secure code: no Secure register value is visible.
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
    beq 2f
    EXIT_FAIL 0x201
2:  COUNT ns_ticks_over_s
    // r4-r11 belong to the Secure context, which exception return restores from the Secure
    // stack: this handler may trash them.
    CLOBBER_CALLEE_SAVED
    // Linger once on request.
    LDC r1, ns_linger
    ldr r0, [r1]
    cmp r0, #0
    beq 1f
    movs r0, #0
    str r0, [r1]
    LDC r0, NS_LINGER_LOOPS
3:  subs r0, #1
    bne 3b
1:  COUNT ns_ticks
    bx lr

FUNC ns_unexpected
    UNEXPECTED_EXCEPTION
