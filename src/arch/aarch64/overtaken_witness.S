/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * QEMU-SMP3-ACCEPTANCE §2 — the AArch64 overtaken-deferral witness user programs.
 *
 * Two kernel-built tasks, each copied to 0x2000_0000 in its own address space; the mailbox page
 * 0x2007_0000 is ONE frame mapped in both.
 *   W — CPU 0. Each round: load x19..x28 with a round-specific pattern, FutexWait on WAIT, and on
 *       return require a non-error result lane (x0 <= 1), the pattern in x19..x28 and the same SP.
 *       Next round on success; park on FAIL on any mismatch; park on DONE after the last round.
 *   K — CPU 1. Each round: FutexWake WAIT until one call wakes exactly one waiter (yielding
 *       between attempts), then log it. Park on KPARK after the last round.
 * W's verdict is where it parks: the kernel witness reads it and emits the sealed summary.
 */
    .pushsection .text.yarm_ovt_witness, "ax", %progbits

    .set WAIT_VA, 0x20070000
    .set FAIL_VA, 0x20070040
    .set DONE_VA, 0x20070080
    .set KPARK_VA, 0x200700C0
    .set ROUNDS, 4

    .set NR_YIELD, 0
    .set NR_FUTEX_WAIT, 9
    .set NR_FUTEX_WAKE, 10
    .set NR_LOG, 15

    .macro OLI32 reg, val
    movz \reg, #(((\val) >> 16) & 0xffff), lsl #16
    movk \reg, #((\val) & 0xffff)
    .endm

    /* FutexWait(addr, expected = observed = x20): the kernel parks unconditionally. */
    .macro OWAIT addr
    OLI32 x0, \addr
    mov x1, x20
    mov x2, x20
    mov x8, #NR_FUTEX_WAIT
    svc #0
    .endm

    .balign 4
    .global yarm_ovt_w_start
yarm_ovt_w_start:
    mov x20, #0
    mov x29, sp
1:
    add x20, x20, #1
    /* x9 = 0x5757_0000_0000_0000 | round << 8; x19, x21..x28 = x9 + i. */
    movz x9, #0x5757, lsl #48
    orr x9, x9, x20, lsl #8
    add x19, x9, #19
    add x21, x9, #21
    add x22, x9, #22
    add x23, x9, #23
    add x24, x9, #24
    add x25, x9, #25
    add x26, x9, #26
    add x27, x9, #27
    add x28, x9, #28
    OWAIT WAIT_VA
    cmp x0, #1
    b.hi 8f
    mov x10, sp
    cmp x10, x29
    b.ne 8f
    movz x9, #0x5757, lsl #48
    orr x9, x9, x20, lsl #8
    add x10, x9, #19
    cmp x19, x10
    b.ne 8f
    add x10, x9, #21
    cmp x21, x10
    b.ne 8f
    add x10, x9, #22
    cmp x22, x10
    b.ne 8f
    add x10, x9, #23
    cmp x23, x10
    b.ne 8f
    add x10, x9, #24
    cmp x24, x10
    b.ne 8f
    add x10, x9, #25
    cmp x25, x10
    b.ne 8f
    add x10, x9, #26
    cmp x26, x10
    b.ne 8f
    add x10, x9, #27
    cmp x27, x10
    b.ne 8f
    add x10, x9, #28
    cmp x28, x10
    b.ne 8f
    adr x0, ovt_w_ok
    adr x1, ovt_w_ok_end
    sub x1, x1, x0
    mov x8, #NR_LOG
    svc #0
    cmp x20, #ROUNDS
    b.lo 1b
9:
    OWAIT DONE_VA
    b 9b
8:
    OWAIT FAIL_VA
    b 8b
ovt_w_ok: .ascii "OVT_W_ROUND_OK"
ovt_w_ok_end:
    .balign 4
    .global yarm_ovt_w_end
yarm_ovt_w_end:

    .balign 4
    .global yarm_ovt_k_start
yarm_ovt_k_start:
    mov x20, #0
1:
    add x20, x20, #1
2:
    OLI32 x0, WAIT_VA
    mov x1, #1
    mov x8, #NR_FUTEX_WAKE
    svc #0
    cmp x0, #1
    b.eq 3f
    mov x8, #NR_YIELD
    svc #0
    b 2b
3:
    adr x0, ovt_k_woke
    adr x1, ovt_k_woke_end
    sub x1, x1, x0
    mov x8, #NR_LOG
    svc #0
    cmp x20, #ROUNDS
    b.lo 1b
4:
    OWAIT KPARK_VA
    b 4b
ovt_k_woke: .ascii "OVT_K_WOKE_ONE"
ovt_k_woke_end:
    .balign 4
    .global yarm_ovt_k_end
yarm_ovt_k_end:

    .popsection
