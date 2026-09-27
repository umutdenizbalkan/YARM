/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * QEMU-SMP1 §3 — the two-CPU IPI / TLB witness user programs (x86_64).
 *
 * Two kernel-built oracle tasks, copied to 0x2000_0000 in their own address spaces:
 *   server — CPU 1 (the AP), address space A;
 *   client — CPU 0 (the BSP), address space B.
 * Everything is position-independent: data is reached through absolute user VAs held in a
 * register, every branch and call is relative, and each program's strings are addressed
 * RIP-relative inside the program itself.
 *
 * User VAs (both address spaces):
 *   0x2002_0000  marker page (the reply-profile strings; unused here)
 *   0x2003_0000  recv-v2 payload        0x2004_0000  recv-v2 metadata
 *   0x2005_0000  server reply source    0x2006_0000  FXSAVE pattern image (+0x400 capture area)
 *   0x2007_0000  MBX, ONE frame shared by both address spaces
 *   0x2008_0000  W, created by each task through NR 13, replaced by the OTHER task through NR 3
 *   0x2009_0000  R, the residency probe (kernel-mapped; the kernel re-points it WITHOUT any
 *                invalidation when the requester arms a round, so a target still reading the
 *                old R after the ACK has provably not reloaded CR3 in between)
 *
 * MBX words: +0x00 client_round  +0x08 server_primed  +0x10 client_done
 *            +0x20 client_primed +0x28 server_done
 *            +0x40 server_early  +0x48 client_early   (W seen replaced before the done word)
 *            +0x60 server_brackets +0x68 client_brackets (resident-loop check count)
 *
 * Context patterns. Across a blocking syscall: rbx rbp r12 r13 r14 r15 and the FXSAVE image
 * (FCW, MXCSR, XMM0..15). While resident: additionally rdi r8 r9 r10 and RFLAGS
 * (CF PF AF ZF SF OF DF). The two tasks use distinct values for every one of them.
 */
    .intel_syntax noprefix
    .pushsection .text.yarm_smp1_witness, "ax", @progbits

    .set SMP1_BASE, 0x20000000
    .set SMP1_PAYLOAD, 0x20030000
    .set SMP1_META, 0x20040000
    .set SMP1_REPLY_SRC, 0x20050000
    .set SMP1_PAT, 0x20060000
    .set SMP1_MBX, 0x20070000
    .set SMP1_W, 0x20080000
    .set SMP1_R, 0x20090000
    .set SMP1_OLD_R, 0x5151515151515151
    .set SMP1_FLAG_MASK, 0xCD5

    /* DebugLog of one of this program's own strings (RIP-relative, so position-independent). */
    .macro SMP1_LOG start, msg, msgend
    mov eax, 15
    lea rdi, [rip + \msg]
    lea rsi, [rip + \msgend]
    sub rsi, rdi
    syscall
    .endm

    /* Recv-v2 on the cap at the patch label; blocks. */
    .macro SMP1_RECV_V2 caplabel
    mov eax, 2
    .byte 0xBF
\caplabel: .long 0x44444444
    mov esi, SMP1_PAYLOAD
    mov edx, 8
    mov r10d, SMP1_META
    mov r8d, 0x28
    xor r9d, r9d
    syscall
    .endm

    /* NR 13: a fresh anonymous page at W in the caller's own address space, then the sentinel. */
    .macro SMP1_MAKE_W old, fail
    mov eax, 13
    xor edi, edi
    mov esi, SMP1_W
    mov edx, 0x1000
    mov r10d, 3
    syscall
    test ecx, ecx
    jnz \fail
    mov esi, SMP1_W
    movabs rax, \old
    mov qword ptr [rsi], rax
    .endm

    /* NR 3: a fresh anonymous page at W in the address space the cap at the patch label names. */
    .macro SMP1_REPLACE_W caplabel
    mov eax, 3
    .byte 0xBF
\caplabel: .long 0x55555555
    mov esi, SMP1_W
    mov edx, 0x1000
    mov r10d, 3
    syscall
    .endm

    .macro SMP1_SET_CALLEE p
    movabs rbx, \p + 1
    movabs rbp, \p + 2
    movabs r12, \p + 3
    movabs r13, \p + 4
    movabs r14, \p + 5
    movabs r15, \p + 6
    .endm

    .macro SMP1_CHECK_CALLEE p, fail
    movabs rax, \p + 1
    cmp rbx, rax
    jne \fail
    movabs rax, \p + 2
    cmp rbp, rax
    jne \fail
    movabs rax, \p + 3
    cmp r12, rax
    jne \fail
    movabs rax, \p + 4
    cmp r13, rax
    jne \fail
    movabs rax, \p + 5
    cmp r14, rax
    jne \fail
    movabs rax, \p + 6
    cmp r15, rax
    jne \fail
    .endm

    .macro SMP1_SET_RESIDENT p, flags
    mov eax, SMP1_PAT
    fxrstor64 [rax]
    SMP1_SET_CALLEE \p
    movabs rdi, \p + 7
    movabs r8, \p + 8
    movabs r9, \p + 9
    movabs r10, \p + 10
    push \flags
    popfq
    .endm

    /* Inside a bracket: rdx holds the RFLAGS captured on entry; rax rcx rsi r11 are scratch. */
    .macro SMP1_CHECK_RESIDENT p, flags, fpcheck, fail
    mov rax, rdx
    and eax, SMP1_FLAG_MASK
    cmp eax, (\flags) & SMP1_FLAG_MASK
    jne \fail
    SMP1_CHECK_CALLEE \p, \fail
    movabs rax, \p + 7
    cmp rdi, rax
    jne \fail
    movabs rax, \p + 8
    cmp r8, rax
    jne \fail
    movabs rax, \p + 9
    cmp r9, rax
    jne \fail
    movabs rax, \p + 10
    cmp r10, rax
    jne \fail
    call \fpcheck
    test eax, eax
    jnz \fail
    .endm

    /* A flag-neutral dwell: mov, lea, jrcxz, jmp only. IPIs and ticks land here. */
    .macro SMP1_DWELL
    mov ecx, 20000
1:  lea rcx, [rcx - 1]
    jrcxz 2f
    jmp 1b
2:
    .endm

    /* fxsave64 into the capture area and compare FCW, MXCSR and XMM0..15 with the pattern.
     * Uses rax rcx rsi r11; eax = 0 when equal. */
    .macro SMP1_FPCHECK_BODY
    mov esi, SMP1_PAT + 0x400
    fxsave64 [rsi]
    mov ax, word ptr [rsi]
    cmp ax, word ptr [rsi - 0x400]
    jne 9f
    mov eax, dword ptr [rsi + 24]
    cmp eax, dword ptr [rsi - 0x400 + 24]
    jne 9f
    mov ecx, 32
    lea rax, [rsi + 160]
1:  mov r11, qword ptr [rax]
    cmp r11, qword ptr [rax - 0x400]
    jne 9f
    add rax, 8
    dec ecx
    jnz 1b
    xor eax, eax
    ret
9:  mov eax, 1
    ret
    .endm

/* ══════════════════════════════ server: CPU 1, address space A ══════════════════════════ */
    .set SRV_P, 0xA1A1000000000000
    .set SRV_FLAGS, 0xA87
    .set SRV_OLD_W, 0xA0A0A0A0A0A0A0A0

    .balign 16
    .global yarm_smp1_server_start
yarm_smp1_server_start:
    SMP1_MAKE_W SRV_OLD_W, smp1s_setup_fail
    SMP1_LOG yarm_smp1_server_start, smp1s_m_entered, smp1s_m_entered_end
    /* The context that must survive the block and the AP saved-frame resume. */
    mov eax, SMP1_PAT
    fxrstor64 [rax]
    SMP1_SET_CALLEE SRV_P
    SMP1_RECV_V2 yarm_smp1_server_recv_cap_1
    /* Resumed by the AP saved-frame path after the client's NR6 woke CPU 1. */
    SMP1_CHECK_CALLEE SRV_P, smp1s_resume_ctx_fail
    call smp1s_fpcheck
    test eax, eax
    jnz smp1s_resume_ctx_fail
    SMP1_LOG yarm_smp1_server_start, smp1s_m_resume_ok, smp1s_m_resume_ok_end
    /* Reply with the receiver-local Reply cap the metadata carries (CPU 1 -> CPU 0 wake). */
    mov r13d, SMP1_META + 0x10
    mov r13d, dword ptr [r13]
    xor ebx, ebx
smp1s_reply_retry:
    inc ebx
    push 7
    pop rax
    mov edi, r13d
    mov esi, SMP1_REPLY_SRC
    push 8
    pop rdx
    or r9, -1
    syscall
    test ecx, ecx
    je smp1s_replied
    cmp ecx, 7
    jne smp1s_reply_fail
    cmp ebx, 0x40
    jae smp1s_reply_fail
    mov ecx, 0x40000
1:  pause
    dec ecx
    jne 1b
    jmp smp1s_reply_retry
smp1s_replied:
    SMP1_LOG yarm_smp1_server_start, smp1s_m_replied, smp1s_m_replied_end

    /* ── T01: resident TARGET while the client (CPU 0) replaces W in address space A. ── */
    SMP1_SET_RESIDENT SRV_P, SRV_FLAGS
smp1s_t01_loop:
    SMP1_DWELL
    pushfq
    pop rdx
    SMP1_CHECK_RESIDENT SRV_P, SRV_FLAGS, smp1s_fpcheck, smp1s_resident_fail
    mov esi, SMP1_MBX
    add qword ptr [rsi + 0x60], 1
    mov rax, qword ptr [rsi + 0x00]
    cmp rax, 1
    jne smp1s_t01_next
    mov rax, qword ptr [rsi + 0x08]
    cmp rax, 1
    je smp1s_t01_primed
    /* Prime: use the OLD translations of W and R. */
    mov esi, SMP1_W
    mov rax, qword ptr [rsi]
    movabs rcx, SRV_OLD_W
    cmp rax, rcx
    jne smp1s_prime_fail
    mov esi, SMP1_R
    mov rax, qword ptr [rsi]
    movabs rcx, SMP1_OLD_R
    cmp rax, rcx
    jne smp1s_prime_fail
    mov esi, SMP1_MBX
    mov qword ptr [rsi + 0x08], 1
    jmp smp1s_t01_next
smp1s_t01_primed:
    mov rax, qword ptr [rsi + 0x10]
    cmp rax, 1
    je smp1s_t01_observe
    /* Keep reading W through whatever translation this CPU holds. */
    mov esi, SMP1_W
    mov rax, qword ptr [rsi]
    movabs rcx, SRV_OLD_W
    cmp rax, rcx
    je smp1s_t01_next
    test rax, rax
    jnz smp1s_unexpected
    mov esi, SMP1_MBX
    mov qword ptr [rsi + 0x40], 1
smp1s_t01_next:
    push rdx
    popfq
    jmp smp1s_t01_loop
smp1s_t01_observe:
    mov esi, SMP1_W
    mov r11, qword ptr [rsi]
    mov esi, SMP1_R
    mov rsi, qword ptr [rsi]
    push rdx
    popfq
    /* One more complete check, then leave resident mode. */
    pushfq
    pop rdx
    push rsi
    push r11
    SMP1_CHECK_RESIDENT SRV_P, SRV_FLAGS, smp1s_fpcheck, smp1s_resident_fail
    pop r11
    pop rsi
    movabs rax, SMP1_OLD_R
    cmp rsi, rax
    jne smp1s_t01_residency_lost
    test r11, r11
    jz smp1s_t01_ok
    movabs rax, SRV_OLD_W
    cmp r11, rax
    je smp1s_t01_stale
    jmp smp1s_unexpected
smp1s_t01_ok:
    SMP1_LOG yarm_smp1_server_start, smp1s_m_t01_ok, smp1s_m_t01_ok_end
    jmp smp1s_t10
smp1s_t01_residency_lost:
    SMP1_LOG yarm_smp1_server_start, smp1s_m_t01_lost, smp1s_m_t01_lost_end
    jmp smp1s_t10
smp1s_t01_stale:
    SMP1_LOG yarm_smp1_server_start, smp1s_m_t01_stale, smp1s_m_t01_stale_end
    jmp smp1s_park

    /* ── T10: REQUESTER — replace W in address space B while the client is resident on CPU 0. ── */
smp1s_t10:
    mov esi, SMP1_MBX
1:  mov rax, qword ptr [rsi + 0x20]
    cmp rax, 1
    je 2f
    pause
    jmp 1b
2:
    SMP1_LOG yarm_smp1_server_start, smp1s_m_arm, smp1s_m_arm_end
    SMP1_REPLACE_W yarm_smp1_server_client_as_cap
    test ecx, ecx
    jnz smp1s_vmmap_fail
    mov esi, SMP1_MBX
    mov qword ptr [rsi + 0x28], 1
    SMP1_LOG yarm_smp1_server_start, smp1s_m_t10_done, smp1s_m_t10_done_end
smp1s_fin:
    SMP1_RECV_V2 yarm_smp1_server_recv_cap_2
    SMP1_LOG yarm_smp1_server_start, smp1s_m_again, smp1s_m_again_end
    jmp smp1s_fin

smp1s_setup_fail:
    SMP1_LOG yarm_smp1_server_start, smp1s_m_setup_fail, smp1s_m_setup_fail_end
    jmp smp1s_park
smp1s_resume_ctx_fail:
    SMP1_LOG yarm_smp1_server_start, smp1s_m_resume_fail, smp1s_m_resume_fail_end
    jmp smp1s_park
smp1s_reply_fail:
    SMP1_LOG yarm_smp1_server_start, smp1s_m_reply_fail, smp1s_m_reply_fail_end
    jmp smp1s_park
smp1s_resident_fail:
    cld
    SMP1_LOG yarm_smp1_server_start, smp1s_m_resident_fail, smp1s_m_resident_fail_end
    jmp smp1s_park
smp1s_prime_fail:
    cld
    SMP1_LOG yarm_smp1_server_start, smp1s_m_prime_fail, smp1s_m_prime_fail_end
    jmp smp1s_park
smp1s_unexpected:
    cld
    SMP1_LOG yarm_smp1_server_start, smp1s_m_unexpected, smp1s_m_unexpected_end
    jmp smp1s_park
smp1s_vmmap_fail:
    SMP1_LOG yarm_smp1_server_start, smp1s_m_vmmap_fail, smp1s_m_vmmap_fail_end
smp1s_park:
    pause
    jmp smp1s_park

smp1s_fpcheck:
    SMP1_FPCHECK_BODY

smp1s_m_entered: .ascii "SMP1_SERVER_ENTERED cpu=1 result=ok\n"
smp1s_m_entered_end:
smp1s_m_resume_ok: .ascii "SMP1_SERVER_RESUME_CONTEXT_OK cpu=1 path=ap_saved_frame gprs=6 fxsave=1 result=ok\n"
smp1s_m_resume_ok_end:
smp1s_m_resume_fail: .ascii "SMP1_SERVER_RESUME_CONTEXT_FAIL cpu=1 result=fail\n"
smp1s_m_resume_fail_end:
smp1s_m_replied: .ascii "SMP1_SERVER_REPLIED cpu=1 result=ok\n"
smp1s_m_replied_end:
smp1s_m_t01_ok: .ascii "SMP1_T01_TARGET_OBSERVED cpu=1 w=replaced r=stale context=ok result=ok\n"
smp1s_m_t01_ok_end:
smp1s_m_t01_lost: .ascii "SMP1_T01_TARGET_RESIDENCY_LOST cpu=1 w=any r=reloaded result=inconclusive\n"
smp1s_m_t01_lost_end:
smp1s_m_t01_stale: .ascii "SMP1_T01_TARGET_STALE_AFTER_ACK cpu=1 w=old r=stale result=fail\n"
smp1s_m_t01_stale_end:
smp1s_m_arm: .ascii "SMP1_ARM_R target=client\n"
smp1s_m_arm_end:
smp1s_m_t10_done: .ascii "SMP1_T10_REQUESTER_DONE cpu=1 vm_map=ok result=ok\n"
smp1s_m_t10_done_end:
smp1s_m_again: .ascii "SMP1_SERVER_RECV_AGAIN_RETURNED cpu=1 result=unexpected\n"
smp1s_m_again_end:
smp1s_m_setup_fail: .ascii "SMP1_SERVER_SETUP_FAIL cpu=1 result=fail\n"
smp1s_m_setup_fail_end:
smp1s_m_reply_fail: .ascii "SMP1_SERVER_REPLY_FAIL cpu=1 result=fail\n"
smp1s_m_reply_fail_end:
smp1s_m_resident_fail: .ascii "SMP1_SERVER_RESIDENT_CONTEXT_FAIL cpu=1 result=fail\n"
smp1s_m_resident_fail_end:
smp1s_m_prime_fail: .ascii "SMP1_SERVER_PRIME_FAIL cpu=1 result=fail\n"
smp1s_m_prime_fail_end:
smp1s_m_unexpected: .ascii "SMP1_SERVER_UNEXPECTED_W cpu=1 result=fail\n"
smp1s_m_unexpected_end:
smp1s_m_vmmap_fail: .ascii "SMP1_SERVER_VM_MAP_FAIL cpu=1 result=fail\n"
smp1s_m_vmmap_fail_end:
    .balign 16
    .global yarm_smp1_server_end
yarm_smp1_server_end:

/* ══════════════════════════════ client: CPU 0, address space B ══════════════════════════ */
    .set CLI_P, 0xB2B2000000000000
    .set CLI_FLAGS, 0x652
    .set CLI_OLD_W, 0xB0B0B0B0B0B0B0B0

    .balign 16
    .global yarm_smp1_client_start
yarm_smp1_client_start:
    SMP1_MAKE_W CLI_OLD_W, smp1c_setup_fail
    SMP1_LOG yarm_smp1_client_start, smp1c_m_entered, smp1c_m_entered_end
    /* NR6 to the server (CPU 0 -> CPU 1 wake, CPU 1 idle), bounded WouldBlock retry. */
    xor ebx, ebx
smp1c_nr6_retry:
    inc ebx
    mov eax, 6
    .byte 0xBF
    .global yarm_smp1_client_send_cap
yarm_smp1_client_send_cap: .long 0x11111111
    mov esi, SMP1_PAYLOAD
    mov edx, 0x0A
    .byte 0x41, 0xB9
    .global yarm_smp1_client_reply_cap_r9
yarm_smp1_client_reply_cap_r9: .long 0x22222222
    syscall
    test ecx, ecx
    je smp1c_sent
    cmp ecx, 7
    jne smp1c_nr6_fail
    cmp ebx, 0x40
    jae smp1c_nr6_fail
    xor eax, eax
    syscall
    jmp smp1c_nr6_retry
smp1c_sent:
    /* The context that must survive the block and the BSP's production selection. */
    mov eax, SMP1_PAT
    fxrstor64 [rax]
    SMP1_SET_CALLEE CLI_P
    SMP1_RECV_V2 yarm_smp1_client_reply_cap_1
    SMP1_CHECK_CALLEE CLI_P, smp1c_resume_ctx_fail
    call smp1c_fpcheck
    test eax, eax
    jnz smp1c_resume_ctx_fail
    mov edi, SMP1_PAYLOAD
    mov rax, qword ptr [rdi]
    movabs rcx, 0x214b4f2d594c5052
    cmp rax, rcx
    jne smp1c_resume_ctx_fail
    SMP1_LOG yarm_smp1_client_start, smp1c_m_resume_ok, smp1c_m_resume_ok_end

    /* ── T01: REQUESTER — replace W in address space A while the server is resident on CPU 1. ── */
    mov esi, SMP1_MBX
    mov qword ptr [rsi + 0x00], 1
1:  mov rax, qword ptr [rsi + 0x08]
    cmp rax, 1
    je 2f
    pause
    jmp 1b
2:
    SMP1_LOG yarm_smp1_client_start, smp1c_m_arm, smp1c_m_arm_end
    SMP1_REPLACE_W yarm_smp1_client_server_as_cap
    test ecx, ecx
    jnz smp1c_vmmap_fail
    mov esi, SMP1_MBX
    mov qword ptr [rsi + 0x10], 1
    SMP1_LOG yarm_smp1_client_start, smp1c_m_t01_done, smp1c_m_t01_done_end

    /* ── T10: resident TARGET while the server (CPU 1) replaces W in address space B. ── */
    SMP1_SET_RESIDENT CLI_P, CLI_FLAGS
smp1c_t10_loop:
    SMP1_DWELL
    pushfq
    pop rdx
    SMP1_CHECK_RESIDENT CLI_P, CLI_FLAGS, smp1c_fpcheck, smp1c_resident_fail
    mov esi, SMP1_MBX
    add qword ptr [rsi + 0x68], 1
    mov rax, qword ptr [rsi + 0x20]
    cmp rax, 1
    je smp1c_t10_primed
    mov esi, SMP1_W
    mov rax, qword ptr [rsi]
    movabs rcx, CLI_OLD_W
    cmp rax, rcx
    jne smp1c_prime_fail
    mov esi, SMP1_R
    mov rax, qword ptr [rsi]
    movabs rcx, SMP1_OLD_R
    cmp rax, rcx
    jne smp1c_prime_fail
    mov esi, SMP1_MBX
    mov qword ptr [rsi + 0x20], 1
    jmp smp1c_t10_next
smp1c_t10_primed:
    mov rax, qword ptr [rsi + 0x28]
    cmp rax, 1
    je smp1c_t10_observe
    mov esi, SMP1_W
    mov rax, qword ptr [rsi]
    movabs rcx, CLI_OLD_W
    cmp rax, rcx
    je smp1c_t10_next
    test rax, rax
    jnz smp1c_unexpected
    mov esi, SMP1_MBX
    mov qword ptr [rsi + 0x48], 1
smp1c_t10_next:
    push rdx
    popfq
    jmp smp1c_t10_loop
smp1c_t10_observe:
    mov esi, SMP1_W
    mov r11, qword ptr [rsi]
    mov esi, SMP1_R
    mov rsi, qword ptr [rsi]
    push rdx
    popfq
    pushfq
    pop rdx
    push rsi
    push r11
    SMP1_CHECK_RESIDENT CLI_P, CLI_FLAGS, smp1c_fpcheck, smp1c_resident_fail
    pop r11
    pop rsi
    cld
    movabs rax, SMP1_OLD_R
    cmp rsi, rax
    jne smp1c_t10_residency_lost
    test r11, r11
    jz smp1c_t10_ok
    movabs rax, CLI_OLD_W
    cmp r11, rax
    je smp1c_t10_stale
    jmp smp1c_unexpected
smp1c_t10_ok:
    SMP1_LOG yarm_smp1_client_start, smp1c_m_t10_ok, smp1c_m_t10_ok_end
    jmp smp1c_fin
smp1c_t10_residency_lost:
    SMP1_LOG yarm_smp1_client_start, smp1c_m_t10_lost, smp1c_m_t10_lost_end
    jmp smp1c_fin
smp1c_t10_stale:
    SMP1_LOG yarm_smp1_client_start, smp1c_m_t10_stale, smp1c_m_t10_stale_end
    jmp smp1c_park
smp1c_fin:
    SMP1_RECV_V2 yarm_smp1_client_reply_cap_2
    SMP1_LOG yarm_smp1_client_start, smp1c_m_again, smp1c_m_again_end
    jmp smp1c_fin

smp1c_setup_fail:
    SMP1_LOG yarm_smp1_client_start, smp1c_m_setup_fail, smp1c_m_setup_fail_end
    jmp smp1c_park
smp1c_nr6_fail:
    SMP1_LOG yarm_smp1_client_start, smp1c_m_nr6_fail, smp1c_m_nr6_fail_end
    jmp smp1c_park
smp1c_resume_ctx_fail:
    SMP1_LOG yarm_smp1_client_start, smp1c_m_resume_fail, smp1c_m_resume_fail_end
    jmp smp1c_park
smp1c_resident_fail:
    cld
    SMP1_LOG yarm_smp1_client_start, smp1c_m_resident_fail, smp1c_m_resident_fail_end
    jmp smp1c_park
smp1c_prime_fail:
    cld
    SMP1_LOG yarm_smp1_client_start, smp1c_m_prime_fail, smp1c_m_prime_fail_end
    jmp smp1c_park
smp1c_unexpected:
    cld
    SMP1_LOG yarm_smp1_client_start, smp1c_m_unexpected, smp1c_m_unexpected_end
    jmp smp1c_park
smp1c_vmmap_fail:
    SMP1_LOG yarm_smp1_client_start, smp1c_m_vmmap_fail, smp1c_m_vmmap_fail_end
smp1c_park:
    pause
    jmp smp1c_park

smp1c_fpcheck:
    SMP1_FPCHECK_BODY

smp1c_m_entered: .ascii "SMP1_CLIENT_ENTERED cpu=0 result=ok\n"
smp1c_m_entered_end:
smp1c_m_resume_ok: .ascii "SMP1_CLIENT_RESUME_CONTEXT_OK cpu=0 path=production_selection gprs=6 fxsave=1 reply=ok result=ok\n"
smp1c_m_resume_ok_end:
smp1c_m_resume_fail: .ascii "SMP1_CLIENT_RESUME_CONTEXT_FAIL cpu=0 result=fail\n"
smp1c_m_resume_fail_end:
smp1c_m_arm: .ascii "SMP1_ARM_R target=server\n"
smp1c_m_arm_end:
smp1c_m_t01_done: .ascii "SMP1_T01_REQUESTER_DONE cpu=0 vm_map=ok result=ok\n"
smp1c_m_t01_done_end:
smp1c_m_t10_ok: .ascii "SMP1_T10_TARGET_OBSERVED cpu=0 w=replaced r=stale context=ok result=ok\n"
smp1c_m_t10_ok_end:
smp1c_m_t10_lost: .ascii "SMP1_T10_TARGET_RESIDENCY_LOST cpu=0 w=any r=reloaded result=inconclusive\n"
smp1c_m_t10_lost_end:
smp1c_m_t10_stale: .ascii "SMP1_T10_TARGET_STALE_AFTER_ACK cpu=0 w=old r=stale result=fail\n"
smp1c_m_t10_stale_end:
smp1c_m_again: .ascii "SMP1_CLIENT_RECV_AGAIN_RETURNED cpu=0 result=unexpected\n"
smp1c_m_again_end:
smp1c_m_setup_fail: .ascii "SMP1_CLIENT_SETUP_FAIL cpu=0 result=fail\n"
smp1c_m_setup_fail_end:
smp1c_m_nr6_fail: .ascii "SMP1_CLIENT_NR6_FAIL cpu=0 result=fail\n"
smp1c_m_nr6_fail_end:
smp1c_m_resident_fail: .ascii "SMP1_CLIENT_RESIDENT_CONTEXT_FAIL cpu=0 result=fail\n"
smp1c_m_resident_fail_end:
smp1c_m_prime_fail: .ascii "SMP1_CLIENT_PRIME_FAIL cpu=0 result=fail\n"
smp1c_m_prime_fail_end:
smp1c_m_unexpected: .ascii "SMP1_CLIENT_UNEXPECTED_W cpu=0 result=fail\n"
smp1c_m_unexpected_end:
smp1c_m_vmmap_fail: .ascii "SMP1_CLIENT_VM_MAP_FAIL cpu=0 result=fail\n"
smp1c_m_vmmap_fail_end:
    .balign 16
    .global yarm_smp1_client_end
yarm_smp1_client_end:

    .global yarm_smp1_server_recv_cap_1
    .global yarm_smp1_server_recv_cap_2
    .global yarm_smp1_server_client_as_cap
    .global yarm_smp1_client_reply_cap_1
    .global yarm_smp1_client_reply_cap_2
    .global yarm_smp1_client_server_as_cap
    .popsection
