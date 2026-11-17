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
    li s1, 0x30020000
.Lrtw_round:
    ld s3, 0x28(s1)
.Lrtw_spin:
    addi s3, s3, -1
    bnez s3, .Lrtw_spin
    ld t0, 0(s1)
    addi t0, t0, 1
    sd t0, 0(s1)
    ld t1, 0x20(s1)
    bnez t1, .Lrtw_yield
    ld t1, 0x10(s1)
    ld t2, 0x18(s1)
    beq t1, t2, .Lrtw_round
    sd t1, 0x18(s1)
.Lrtw_yield:
    ld t0, 8(s1)
    addi t0, t0, 1
    sd t0, 8(s1)
    li a7, 0
    ecall
    j .Lrtw_round
    .balign 16
    .global yarm_rtw_user_end
yarm_rtw_user_end:
    .popsection
