/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * VM-INVALIDATION2 — the x86_64 dispatch-gap TLB witness user program (ACCEPTANCE revision).
 *
 * ONE App-class task T (kernel-built at 0x2000_0000, enqueued on CPU 0 at boot) spawns the observer
 * as a THREAD (NR 11) that shares its address space A; the production balanced placement puts the
 * observer on another CPU (CPU 1 at -smp 2). Both touch the SAME page OBS in A:
 *   observer — publishes the value it reads from OBS (pre), YIELDs to open the dispatch gap, then
 *              after the mutator reports the round done re-reads OBS (post).
 *   mutator (T) — publishes the round, replaces OBS in A (NR 13), and — this revision — CHECKS NR
 *              13's error and return before writing the sentinel or publishing success. A refused
 *              mapping reports VMI2_MUT_REFUSED and never writes the sentinel.
 *
 * Syscall ABI: rax=NR; rdi rsi rdx r10 r8 r9 args; `syscall` (clobbers rcx r11); rax result, rcx
 * error. NR 13 returns the mapped base in rax and 0 in rcx on success.
 */
    .pushsection .text.yarm_vmi2_witness, "ax", @progbits
    /* KERNEL-NOFP1: everything in this section is a USER-MODE program the kernel copies
     * into a task's address space and never executes; the final-image check accounts
     * for it as one USER_PROGRAM region (scripts/check-kernel-nofp.py). */
.Lyarm_nofp_user_vmi2_witness_begin:

    .set MBX, 0x20070000
    .set OBS, 0x20080000
    .set OBS_STK, 0x20020ff0
    .set OBS_TLS, 0x20090000

    .set NR_YIELD, 0
    .set NR_ANON, 13
    .set NR_LOG, 15
    .set NR_EXIT, 16
    .set NR_SPAWN, 11

    .set M_T1_READY,  0x00
    .set M_MUT_DONE,  0x08
    .set M_T1_POST,   0x10
    .set M_OBS_NEW,   0x18
    .set M_T1_DONE,   0x20
    .set M_T0_DONE,   0x28
    .set M_OBS_TID,   0x30
    .set M_MUT_ROUND, 0x38
    .set M_T1_PRE,    0x40

    .set ROUNDS, 8
    .set NEWVAL_BASE, 0x5644493249320000

    .macro LOG msg
    mov eax, NR_LOG
    lea rdi, [rip + \msg]
    lea rsi, [rip + \msg\()_end]
    sub rsi, rdi
    syscall
    .endm

    .macro STR name, text
    \name: .ascii "\text"
    \name\()_end:
    .endm

/* ───────────────────────── spawner + mutator (CPU 0) ───────────────────────── */
    .globl yarm_vmi2_start
yarm_vmi2_start:
    /* spawn the observer thread: rdi=TLS, rsi=stack top, rdx=entry VA */
    mov edi, OBS_TLS
    mov esi, OBS_STK
    lea rdx, [rip + vmi2_observer]
    xor r10d, r10d
    xor r8d, r8d
    xor r9d, r9d
    mov eax, NR_SPAWN
    syscall
    test rcx, rcx
    jnz spawn_fail
    mov edi, MBX
    mov qword ptr [rdi + M_OBS_TID], rax
    mov r14, 1
mut_loop:
    /* publish the round BEFORE the replace, so the kernel owner hook binds its record to it */
    mov edi, MBX
    mov qword ptr [rdi + M_MUT_ROUND], r14
    /* wait until the observer has entered the gap for this round */
    mov esi, MBX
wait_ready:
    mov rax, qword ptr [rsi + M_T1_READY]
    cmp rax, r14
    jae do_replace
    pause
    jmp wait_ready
do_replace:
    /* NR 13: fresh anon page at OBS, displacing the old frame */
    mov eax, NR_ANON
    xor edi, edi
    mov esi, OBS
    mov edx, 0x1000
    mov r10d, 3
    syscall
    /* CHECK the error lane: a refused mapping must NOT write the sentinel or claim success */
    test rcx, rcx
    jnz mut_refused
    /* CHECK the returned base is OBS (the mapping actually landed where asked) */
    mov rbx, OBS
    cmp rax, rbx
    jne mut_refused
    /* genuine replacement: write this round's sentinel into the fresh page */
    movabs rax, NEWVAL_BASE
    add rax, r14
    mov esi, OBS
    mov qword ptr [rsi], rax
    mov edi, MBX
    mov qword ptr [rdi + M_OBS_NEW], rax
    mov qword ptr [rdi + M_MUT_DONE], r14
    jmp mut_next
mut_refused:
    /* a named failure for this round; release the observer but never claim success */
    LOG vmi2_mut_refused
    mov edi, MBX
    mov qword ptr [rdi + M_MUT_DONE], r14
mut_next:
    inc r14
    cmp r14, ROUNDS
    jbe mut_loop
    mov edi, MBX
    mov qword ptr [rdi + M_T0_DONE], 1
    LOG vmi2_t0_done
    mov eax, NR_EXIT
    xor edi, edi
    syscall
0:  jmp 0b
spawn_fail:
    LOG vmi2_spawn_fail
    mov eax, NR_EXIT
    xor edi, edi
    syscall
1:  jmp 1b

/* ───────────────────────────── observer (CPU 1) ───────────────────────────── */
vmi2_observer:
    mov r14, 1
obs_loop:
    /* read OBS and publish it as `pre` BEFORE announcing readiness, so the kernel records the
     * value this CPU held going into the gap */
    mov esi, OBS
    mov rax, qword ptr [rsi]
    mov edi, MBX
    mov qword ptr [rdi + M_T1_PRE], rax
    mov qword ptr [rdi + M_T1_READY], r14
    xor eax, eax                      /* NR_YIELD: clears current; the kernel gap-hold runs here */
    syscall
    /* wait (userspace, interrupts on) until the mutator has published this round's result, so the
     * re-read observes the completed replacement rather than racing ahead of it */
    mov esi, MBX
obs_wait:
    mov rax, qword ptr [rsi + M_MUT_DONE]
    cmp rax, r14
    jae obs_read
    pause
    jmp obs_wait
obs_read:
    mov esi, OBS
    mov rax, qword ptr [rsi]
    mov edi, MBX
    mov qword ptr [rdi + M_T1_POST], rax
    LOG vmi2_t1_post
    inc r14
    cmp r14, ROUNDS
    jbe obs_loop
    mov edi, MBX
    mov qword ptr [rdi + M_T1_DONE], 1
    LOG vmi2_t1_done
    mov eax, NR_EXIT
    xor edi, edi
    syscall
2:  jmp 2b

    STR vmi2_t1_post, "VMI2_T1_POST"
    STR vmi2_t1_done, "VMI2_T1_DONE"
    STR vmi2_t0_done, "VMI2_T0_DONE"
    STR vmi2_mut_refused, "VMI2_MUT_REFUSED"
    STR vmi2_spawn_fail, "VMI2_SPAWN_FAIL"
    .globl yarm_vmi2_end
yarm_vmi2_end:

.Lyarm_nofp_user_vmi2_witness_end:
    .pushsection .yarm_nofp_map, "", @progbits
    .balign 8
    .quad 0x3150464F4E4D5259
    .quad .Lyarm_nofp_user_vmi2_witness_begin
    .quad .Lyarm_nofp_user_vmi2_witness_end
    .asciz "USER_PROGRAM vmi2_witness"
    .balign 8
    .popsection
    .popsection
