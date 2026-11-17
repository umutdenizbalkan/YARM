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
    mov r12, 0x30020000
.Lrtw_round:
    mov rbx, qword ptr [r12 + 0x28]
.Lrtw_spin:
    dec rbx
    jnz .Lrtw_spin
    inc qword ptr [r12]
    mov rax, qword ptr [r12 + 0x20]
    test rax, rax
    jnz .Lrtw_yield
    mov rax, qword ptr [r12 + 0x10]
    cmp rax, qword ptr [r12 + 0x18]
    je .Lrtw_round
    mov qword ptr [r12 + 0x18], rax
.Lrtw_yield:
    inc qword ptr [r12 + 0x08]
    xor eax, eax
    syscall
    jmp .Lrtw_round
    .balign 16
    .global yarm_rtw_user_end
yarm_rtw_user_end:
    .popsection
