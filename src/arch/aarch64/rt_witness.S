/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Umut Deniz Balkan
 *
 * RT-SCHED1A — the RT witness user program (`kernel::rt_witness`), copied by the kernel into
 * each witness task's own address space. It runs a loop: spin MBX+0x28 iterations, count a
 * heartbeat (MBX+0x00), then yield (NR 0, counted at MBX+0x08 before the trap) either every round
 * (MBX+0x20 != 0: a yield storm) or only when the kernel has raised its yield request
 * (MBX+0x10 != MBX+0x18, acknowledged by copying the request into MBX+0x18). Nothing else.
 */
    .pushsection .text.yarm_rtw_witness, "ax", @progbits
    /* KERNEL-NOFP1: a user-mode program the kernel copies and never executes. It uses no FP or
     * SIMD instruction, so the final-image check reads it as ordinary code (no exclusion). */
    .balign 16
    .global yarm_rtw_user_start
yarm_rtw_user_start:
    movz x19, #0x3002, lsl #16
.Lrtw_round:
    ldr x20, [x19, #0x28]
.Lrtw_spin:
    subs x20, x20, #1
    b.ne .Lrtw_spin
    ldr x21, [x19]
    add x21, x21, #1
    str x21, [x19]
    ldr x21, [x19, #0x20]
    cbnz x21, .Lrtw_yield
    ldr x21, [x19, #0x10]
    ldr x22, [x19, #0x18]
    cmp x21, x22
    b.eq .Lrtw_round
    str x21, [x19, #0x18]
.Lrtw_yield:
    ldr x21, [x19, #0x08]
    add x21, x21, #1
    str x21, [x19, #0x08]
    mov x8, #0
    svc #0
    b .Lrtw_round
    .balign 16
    .global yarm_rtw_user_end
yarm_rtw_user_end:
    .popsection
