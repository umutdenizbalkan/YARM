/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * VM-INVALIDATION2 — the x86_64 dispatch-gap TLB witness user program.
 *
 * ONE App-class task T (kernel-built at 0x2000_0000, enqueued on CPU 0 at boot) spawns the
 * observer as a THREAD (NR 11) that shares its address space A; the production balanced placement
 * puts the observer on another CPU (CPU 1 at -smp 2). Both touch the SAME page OBS in A:
 *   observer — reads OBS, then YIELDs, which clears its `current` slot while A's root stays loaded
 *              (the dispatch gap); after it is re-selected it re-reads OBS.
 *   mutator (T) — while the observer is in the gap, replaces OBS in A with a fresh frame (NR 13),
 *              whose displacement drives the real shootdown whose target set must name the
 *              root-resident observer CPU.
 *
 * Nothing here performs the work it witnesses: the replace is an ordinary syscall; the shootdown,
 * its target set and the gap snapshot are the kernel's. Syscall ABI: rax=NR; rdi rsi rdx r10 r8 r9
 * args; `syscall` (clobbers rcx r11); rax result, rcx error.
 */
    .pushsection .text.yarm_vmi2_witness, "ax", @progbits

    .set MBX, 0x20070000
    .set OBS, 0x20080000
    .set OBS_STK, 0x20020ff0          /* observer thread stack top (page 0x20020000) */
    .set OBS_TLS, 0x20090000          /* observer thread TLS base */

    .set NR_YIELD, 0
    .set NR_ANON, 13
    .set NR_LOG, 15
    .set NR_EXIT, 16
    .set NR_SPAWN, 11

    .set M_T1_READY, 0x00
    .set M_MUT_DONE, 0x08
    .set M_T1_POST,  0x10
    .set M_OBS_NEW,  0x18
    .set M_T1_DONE,  0x20
    .set M_T0_DONE,  0x28
    .set M_OBS_TID,  0x30

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
    mov esi, MBX
wait_ready:
    mov rax, qword ptr [rsi + M_T1_READY]
    cmp rax, r14
    jae do_replace
    pause
    jmp wait_ready
do_replace:
    mov eax, NR_ANON                  /* NR 13: fresh anon page at OBS, displacing the old frame */
    xor edi, edi
    mov esi, OBS
    mov edx, 0x1000
    mov r10d, 3
    syscall
    movabs rax, NEWVAL_BASE
    add rax, r14
    mov esi, OBS
    mov qword ptr [rsi], rax
    mov edi, MBX
    mov qword ptr [rdi + M_OBS_NEW], rax
    mov qword ptr [rdi + M_MUT_DONE], r14
    inc r14
    cmp r14, ROUNDS
    jbe mut_loop
    mov edi, MBX
    mov qword ptr [rdi + M_T0_DONE], 1
    LOG vmi2_t0_done
mut_wait_done:
    mov esi, MBX
    mov rax, qword ptr [rsi + M_T1_DONE]
    test rax, rax
    jnz mut_exit
    pause
    jmp mut_wait_done
mut_exit:
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
    mov esi, OBS
    mov rax, qword ptr [rsi]          /* cache a translation for OBS on this CPU */
    mov edi, MBX
    mov qword ptr [rdi + M_T1_READY], r14
    xor eax, eax                      /* NR_YIELD: clears current; the kernel gap-hold runs here */
    syscall
    mov esi, OBS
    mov rax, qword ptr [rsi]          /* re-read after resume (same root, suppression) */
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
    STR vmi2_spawn_fail, "VMI2_SPAWN_FAIL"
    .globl yarm_vmi2_end
yarm_vmi2_end:

    .popsection
