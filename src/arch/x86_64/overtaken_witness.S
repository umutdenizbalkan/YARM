/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * QEMU-SMP3-ACCEPTANCE §2 — the x86_64 overtaken-deferral witness user programs.
 *
 * The SMP oracle profile's two tasks run these instead of its own programs, each copied to
 * 0x2000_0000 in its own address space; the mailbox page 0x2007_0000 is ONE frame mapped in both.
 *   W — the client, CPU 0. Each round: load rbx rbp r12 r13 with a round-specific pattern,
 *       FutexWait on WAIT, and on return require a clear error lane (ecx == 0), the pattern and the
 *       same rsp. Next round on success; park on FAIL on any mismatch; park on DONE after the last.
 *   K — the server, CPU 1. Each round: wait (bounded, no kernel entry) until the kernel's hold
 *       publishes GO for this round's wait generation — W's wait on WAIT has COMMITTED and its
 *       drain is held — then FutexWake WAIT until one call wakes exactly one waiter (pausing
 *       between attempts), then log it. Park on KPARK after the last round; log OVT_K_GO_TIMEOUT
 *       and park on KFAIL if GO never arrives (AP-SCHED1).
 * Everything is position-independent; strings are RIP-relative.
 */
    .pushsection .text.yarm_ovt_witness, "ax", @progbits
    /* KERNEL-NOFP1: everything in this section is a USER-MODE program the kernel copies
     * into a task's address space and never executes; the final-image check accounts
     * for it as one USER_PROGRAM region (scripts/check-kernel-nofp.py). */
.Lyarm_nofp_user_overtaken_witness_begin:

    .set OVT_WAIT, 0x20070000
    .set OVT_FAIL, 0x20070040
    .set OVT_DONE, 0x20070080
    .set OVT_KPARK, 0x200700C0
    .set OVT_GO, 0x20070100
    .set OVT_KFAIL, 0x20070140
    .set OVT_GO_MAGIC, 0x4F565447
    .set OVT_GO_BOUND, 0x40000000
    .set OVT_ROUNDS, 4

    /*
     * FutexWait(addr, expected = observed = r15). BL6e: since BL3b the kernel parks only while the
     * 32-bit word still holds `expected`, so the program stores it first. Nothing else writes these
     * words; only K's FutexWake (on WAIT) ends a park.
     */
    .macro OVT_WAIT_ON addr
    mov eax, 9
    mov edi, \addr
    mov dword ptr [rdi], r15d
    mov rsi, r15
    mov rdx, r15
    syscall
    .endm

    .macro OVT_LOG msg
    mov eax, 15
    lea rdi, [rip + \msg]
    lea rsi, [rip + \msg\()_end]
    sub rsi, rdi
    syscall
    .endm

    .global yarm_ovt_x86_w_start
yarm_ovt_x86_w_start:
    xor r15d, r15d
    mov r14, rsp
1:
    inc r15
    movabs rax, 0x5757000000000000
    mov rbx, r15
    shl rbx, 8
    or rbx, rax
    lea rbp, [rbx + 1]
    lea r12, [rbx + 2]
    lea r13, [rbx + 3]
    OVT_WAIT_ON OVT_WAIT
    test ecx, ecx
    jnz 8f
    cmp rsp, r14
    jne 8f
    movabs rax, 0x5757000000000000
    mov rcx, r15
    shl rcx, 8
    or rax, rcx
    cmp rbx, rax
    jne 8f
    lea rcx, [rax + 1]
    cmp rbp, rcx
    jne 8f
    lea rcx, [rax + 2]
    cmp r12, rcx
    jne 8f
    lea rcx, [rax + 3]
    cmp r13, rcx
    jne 8f
    OVT_LOG ovt_x86_w_ok
    cmp r15, OVT_ROUNDS
    jb 1b
9:
    OVT_WAIT_ON OVT_DONE
    jmp 9b
8:
    OVT_WAIT_ON OVT_FAIL
    jmp 8b
ovt_x86_w_ok: .ascii "OVT_W_ROUND_OK"
ovt_x86_w_ok_end:
    .global yarm_ovt_x86_w_end
yarm_ovt_x86_w_end:

    .global yarm_ovt_x86_k_start
yarm_ovt_x86_k_start:
    xor r15d, r15d
1:
    inc r15
    /* GO: high 32 bits the magic, low 32 bits this round's wait generation (= r15). */
    mov r14d, OVT_GO_BOUND
6:
    mov edi, OVT_GO
    mov rax, qword ptr [rdi]
    cmp eax, r15d
    jne 7f
    shr rax, 32
    cmp eax, OVT_GO_MAGIC
    je 2f
7:
    pause
    dec r14d
    jnz 6b
    OVT_LOG ovt_x86_k_go_timeout
    xor r15d, r15d
    jmp 8f
2:
    mov eax, 10
    mov edi, OVT_WAIT
    mov esi, 1
    syscall
    cmp rax, 1
    je 3f
    mov ecx, 4096
5:
    pause
    dec ecx
    jnz 5b
    jmp 2b
3:
    OVT_LOG ovt_x86_k_woke
    cmp r15, OVT_ROUNDS
    jb 1b
4:
    OVT_WAIT_ON OVT_KPARK
    jmp 4b
8:
    OVT_WAIT_ON OVT_KFAIL
    jmp 8b
ovt_x86_k_woke: .ascii "OVT_K_WOKE_ONE"
ovt_x86_k_woke_end:
ovt_x86_k_go_timeout: .ascii "OVT_K_GO_TIMEOUT"
ovt_x86_k_go_timeout_end:
    .global yarm_ovt_x86_k_end
yarm_ovt_x86_k_end:

.Lyarm_nofp_user_overtaken_witness_end:
    .pushsection .yarm_nofp_map, "", @progbits
    .balign 8
    .quad 0x3150464F4E4D5259
    .quad .Lyarm_nofp_user_overtaken_witness_begin
    .quad .Lyarm_nofp_user_overtaken_witness_end
    .asciz "USER_PROGRAM overtaken_witness"
    .balign 8
    .popsection
    .popsection
