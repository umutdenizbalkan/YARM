/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * QEMU-SMP1 §3/§5 — the two-CPU IPI / TLB witness user programs (x86_64).
 *
 * Two kernel-built oracle tasks, copied to 0x2000_0000 in their own address spaces:
 *   server — CPU 1 (the AP), address space A;
 *   client — CPU 0 (the BSP), address space B.
 * Everything is position-independent: data is reached through absolute user VAs held in a
 * register, every branch and call is relative, and each program's strings are addressed
 * RIP-relative inside the program itself.
 *
 * User VAs (both address spaces):
 *   0x2003_0000  recv-v2 payload        0x2004_0000  recv-v2 metadata
 *   0x2005_0000  server reply source
 *   0x2006_0000  PAT: the task's FXSAVE pattern image; +0x300 per-round scratch; +0x400 capture
 *   0x2007_0000  MBX, ONE frame shared by both address spaces
 *   0x2008_0000  W, created by each task through NR 13, replaced by the OTHER task through NR 3
 *   0x2009_0000  R, the residency probe. On every arm the kernel re-points it at a frame it has
 *                never mapped there before, WITHOUT invalidating the target, so a target that
 *                still reads the value it primed has provably not reloaded CR3 in between.
 *
 * Phases:
 *   1. wakes — the IPC round trip (client NR6 -> blocked server, server NR7 -> blocked client);
 *      each side checks rbx rbp r12..r15 and its FXSAVE pattern after it resumes.
 *   2. serial rounds r = 1..SMP1_SERIAL_ROUNDS — odd r: the client replaces the server's W while
 *      the server is RESIDENT in ring 3; even r: the mirror. The resident target checks ten
 *      GPRs, RFLAGS (CF PF AF ZF SF OF DF) and its FXSAVE image around a flag-neutral dwell.
 *   3. mutual rounds m = 1..SMP1_MUTUAL_ROUNDS — both tasks cross a barrier and replace each
 *      other's W at the same time, so each CPU waits for the other's ACK while the other is in
 *      the kernel doing the same.
 *
 * MBX words: serial  +0x00 started  +0x08 primed  +0x10 done  +0x18 observed   (round numbers)
 *            mutual  +0x20/+0x28 ready (server/client)  +0x30/+0x38 nr3 returned
 *                    +0x40/+0x48 own W checked
 */
    .intel_syntax noprefix
    .pushsection .text.yarm_smp1_witness, "ax", @progbits

    .set SMP1_PAYLOAD, 0x20030000
    .set SMP1_META, 0x20040000
    .set SMP1_REPLY_SRC, 0x20050000
    .set SMP1_PAT, 0x20060000
    .set SMP1_MBX, 0x20070000
    .set SMP1_W, 0x20080000
    .set SMP1_R, 0x20090000
    .set SMP1_FLAG_MASK, 0xCD5
    .set SMP1_SERIAL_ROUNDS, 8
    .set SMP1_MUTUAL_ROUNDS, 4

    /* DebugLog of one of this program's own strings (RIP-relative). */
    .macro SMP1_LOG msg
    mov eax, 15
    lea rdi, [rip + \msg]
    lea rsi, [rip + \msg\()_end]
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

    /* Spin in ring 3 until MBX[off] >= rbx. */
    .macro SMP1_WAIT_AT_LEAST off
    mov esi, SMP1_MBX
1:  mov rax, qword ptr [rsi + \off]
    cmp rax, rbx
    jae 2f
    pause
    jmp 1b
2:
    .endm

    /*
     * The round driver. rbx carries the round number between syscalls (the kernel preserves it).
     *   pfx     label prefix          p/flags/old   this task's patterns and W sentinel
     *   parity  1 = target in odd serial rounds, 0 = in even ones
     *   capA/capB  NR 3 patch labels (serial / mutual) for the OTHER task's address space
     *   rdy/ordy, nr3/onr3, chk/ochk   this task's / the other task's mutual MBX words
     */
    .macro SMP1_ROUNDS pfx, p, flags, old, parity, fpcheck, capA, capB, rdy, ordy, nr3, onr3, chk, ochk
\pfx\()_round:
    mov esi, SMP1_MBX
    mov rbx, qword ptr [rsi + 0x18]
    inc rbx
    cmp rbx, SMP1_SERIAL_ROUNDS
    ja \pfx\()_mutual
    mov eax, ebx
    and eax, 1
    cmp eax, \parity
    je \pfx\()_target

    /* ── requester of round rbx ── */
    SMP1_WAIT_AT_LEAST 0x08
    mov qword ptr [rsi + 0x00], rbx
    SMP1_LOG \pfx\()_m_arm
    SMP1_REPLACE_W \capA
    test ecx, ecx
    jnz \pfx\()_vmmap_fail
    mov esi, SMP1_MBX
    mov qword ptr [rsi + 0x10], rbx
    SMP1_LOG \pfx\()_m_req_done
    SMP1_WAIT_AT_LEAST 0x18
    jmp \pfx\()_round

    /* ── resident target of round rbx ── */
\pfx\()_target:
    /* This round's sentinel, written and read through the current translation (priming W). */
    mov esi, SMP1_W
    movabs rax, \old
    xor rax, rbx
    mov qword ptr [rsi], rax
    mov rcx, qword ptr [rsi]
    mov edi, SMP1_PAT
    mov qword ptr [rdi + 0x300], rcx
    mov qword ptr [rdi + 0x308], rbx
    /* Prime R and remember the value this CPU's translation yields. */
    mov esi, SMP1_R
    mov rax, qword ptr [rsi]
    mov qword ptr [rdi + 0x310], rax
    mov esi, SMP1_MBX
    mov qword ptr [rsi + 0x08], rbx
    SMP1_SET_RESIDENT \p, \flags
\pfx\()_tloop:
    SMP1_DWELL
    pushfq
    pop rdx
    SMP1_CHECK_RESIDENT \p, \flags, \fpcheck, \pfx\()_resident_fail
    mov r11d, SMP1_PAT
    mov rcx, qword ptr [r11 + 0x308]
    mov esi, SMP1_MBX
    mov rax, qword ptr [rsi + 0x10]
    cmp rax, rcx
    je \pfx\()_tobserve
    /* Keep using W through whatever translation this CPU holds. */
    mov esi, SMP1_W
    mov rax, qword ptr [rsi]
    cmp rax, qword ptr [r11 + 0x300]
    je \pfx\()_tnext
    test rax, rax
    jnz \pfx\()_unexpected
\pfx\()_tnext:
    push rdx
    popfq
    jmp \pfx\()_tloop
\pfx\()_tobserve:
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
    SMP1_CHECK_RESIDENT \p, \flags, \fpcheck, \pfx\()_resident_fail
    pop r11
    pop rsi
    cld
    mov eax, SMP1_PAT
    cmp rsi, qword ptr [rax + 0x310]
    jne \pfx\()_residency_lost
    test r11, r11
    jz \pfx\()_tok
    cmp r11, qword ptr [rax + 0x300]
    je \pfx\()_stale
    jmp \pfx\()_unexpected
\pfx\()_tok:
    SMP1_LOG \pfx\()_m_target_ok
    mov eax, SMP1_PAT
    mov rbx, qword ptr [rax + 0x308]
    mov esi, SMP1_MBX
    mov qword ptr [rsi + 0x18], rbx
    jmp \pfx\()_round

    /* ── mutual rounds ── */
\pfx\()_mutual:
    xor ebx, ebx
\pfx\()_mloop:
    inc ebx
    cmp ebx, SMP1_MUTUAL_ROUNDS
    ja \pfx\()_fin
    /* Prime this CPU's translation of W with this round's sentinel. */
    mov esi, SMP1_W
    movabs rax, \old
    mov rcx, rbx
    shl rcx, 8
    xor rax, rcx
    mov qword ptr [rsi], rax
    mov rcx, qword ptr [rsi]
    mov edi, SMP1_PAT
    mov qword ptr [rdi + 0x300], rcx
    /* Barrier, then both replace each other's W at once. */
    mov esi, SMP1_MBX
    mov qword ptr [rsi + \rdy], rbx
    SMP1_WAIT_AT_LEAST \ordy
    SMP1_REPLACE_W \capB
    test ecx, ecx
    jnz \pfx\()_vmmap_fail
    mov esi, SMP1_MBX
    mov qword ptr [rsi + \nr3], rbx
    /* The other NR 3 returned only after THIS CPU acknowledged its request. */
    SMP1_WAIT_AT_LEAST \onr3
    mov esi, SMP1_W
    mov rax, qword ptr [rsi]
    test rax, rax
    jz \pfx\()_mok
    mov edi, SMP1_PAT
    cmp rax, qword ptr [rdi + 0x300]
    je \pfx\()_mstale
    jmp \pfx\()_unexpected
\pfx\()_mok:
    SMP1_LOG \pfx\()_m_mutual_ok
    mov esi, SMP1_MBX
    mov qword ptr [rsi + \chk], rbx
    SMP1_WAIT_AT_LEAST \ochk
    jmp \pfx\()_mloop
\pfx\()_mstale:
    SMP1_LOG \pfx\()_m_mutual_stale
    jmp \pfx\()_park
\pfx\()_residency_lost:
    SMP1_LOG \pfx\()_m_lost
    jmp \pfx\()_park
\pfx\()_stale:
    SMP1_LOG \pfx\()_m_stale
    jmp \pfx\()_park
\pfx\()_resident_fail:
    cld
    SMP1_LOG \pfx\()_m_resident_fail
    jmp \pfx\()_park
\pfx\()_unexpected:
    cld
    SMP1_LOG \pfx\()_m_unexpected
    jmp \pfx\()_park
\pfx\()_vmmap_fail:
    SMP1_LOG \pfx\()_m_vmmap_fail
\pfx\()_park:
    pause
    jmp \pfx\()_park
    .endm

    .macro SMP1_STR name, text
\name: .ascii "\text\n"
\name\()_end:
    .endm

/* ══════════════════════════════ server: CPU 1, address space A ══════════════════════════ */
    .set SRV_P, 0xA1A1000000000000
    .set SRV_FLAGS, 0xA87
    .set SRV_OLD_W, 0xA0A0A0A0A0A0A0A0

    .balign 16
    .global yarm_smp1_server_start
yarm_smp1_server_start:
    SMP1_MAKE_W SRV_OLD_W, smp1s_setup_fail
    SMP1_LOG smp1s_m_entered
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
    SMP1_LOG smp1s_m_resume_ok
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
    SMP1_LOG smp1s_m_replied
    SMP1_ROUNDS smp1s, SRV_P, SRV_FLAGS, SRV_OLD_W, 1, smp1s_fpcheck, yarm_smp1_server_client_as_cap, yarm_smp1_server_client_as_cap_m, 0x20, 0x28, 0x30, 0x38, 0x40, 0x48
smp1s_fin:
    SMP1_LOG smp1s_m_rounds_done
smp1s_block:
    SMP1_RECV_V2 yarm_smp1_server_recv_cap_2
    SMP1_LOG smp1s_m_again
    jmp smp1s_block
smp1s_setup_fail:
    SMP1_LOG smp1s_m_setup_fail
    jmp smp1s_park
smp1s_resume_ctx_fail:
    SMP1_LOG smp1s_m_resume_fail
    jmp smp1s_park
smp1s_reply_fail:
    SMP1_LOG smp1s_m_reply_fail
    jmp smp1s_park
smp1s_fpcheck:
    SMP1_FPCHECK_BODY

    SMP1_STR smp1s_m_entered, "SMP1_SERVER_ENTERED cpu=1 result=ok"
    SMP1_STR smp1s_m_resume_ok, "SMP1_SERVER_RESUME_CONTEXT_OK cpu=1 path=ap_saved_frame gprs=6 fxsave=1 result=ok"
    SMP1_STR smp1s_m_resume_fail, "SMP1_SERVER_RESUME_CONTEXT_FAIL cpu=1 result=fail"
    SMP1_STR smp1s_m_replied, "SMP1_SERVER_REPLIED cpu=1 result=ok"
    SMP1_STR smp1s_m_arm, "SMP1_ARM_R target=client"
    SMP1_STR smp1s_m_req_done, "SMP1_REQUESTER_DONE cpu=1 vm_map=ok result=ok"
    SMP1_STR smp1s_m_target_ok, "SMP1_TARGET_OBSERVED cpu=1 w=replaced r=stale context=ok result=ok"
    SMP1_STR smp1s_m_lost, "SMP1_TARGET_RESIDENCY_LOST cpu=1 r=reloaded result=fail"
    SMP1_STR smp1s_m_stale, "SMP1_TARGET_STALE_AFTER_ACK cpu=1 w=old r=stale result=fail"
    SMP1_STR smp1s_m_mutual_ok, "SMP1_MUTUAL_OBSERVED cpu=1 w=replaced result=ok"
    SMP1_STR smp1s_m_mutual_stale, "SMP1_MUTUAL_STALE_AFTER_ACK cpu=1 w=old result=fail"
    SMP1_STR smp1s_m_rounds_done, "SMP1_ROUNDS_DONE cpu=1 result=ok"
    SMP1_STR smp1s_m_again, "SMP1_SERVER_RECV_AGAIN_RETURNED cpu=1 result=fail"
    SMP1_STR smp1s_m_setup_fail, "SMP1_SERVER_SETUP_FAIL cpu=1 result=fail"
    SMP1_STR smp1s_m_reply_fail, "SMP1_SERVER_REPLY_FAIL cpu=1 result=fail"
    SMP1_STR smp1s_m_resident_fail, "SMP1_SERVER_RESIDENT_CONTEXT_FAIL cpu=1 result=fail"
    SMP1_STR smp1s_m_unexpected, "SMP1_SERVER_UNEXPECTED_W cpu=1 result=fail"
    SMP1_STR smp1s_m_vmmap_fail, "SMP1_SERVER_VM_MAP_FAIL cpu=1 result=fail"
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
    SMP1_LOG smp1c_m_entered
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
    SMP1_LOG smp1c_m_resume_ok
    SMP1_ROUNDS smp1c, CLI_P, CLI_FLAGS, CLI_OLD_W, 0, smp1c_fpcheck, yarm_smp1_client_server_as_cap, yarm_smp1_client_server_as_cap_m, 0x28, 0x20, 0x38, 0x30, 0x48, 0x40
smp1c_fin:
    SMP1_LOG smp1c_m_rounds_done
smp1c_block:
    SMP1_RECV_V2 yarm_smp1_client_reply_cap_2
    SMP1_LOG smp1c_m_again
    jmp smp1c_block
smp1c_setup_fail:
    SMP1_LOG smp1c_m_setup_fail
    jmp smp1c_park
smp1c_nr6_fail:
    SMP1_LOG smp1c_m_nr6_fail
    jmp smp1c_park
smp1c_resume_ctx_fail:
    SMP1_LOG smp1c_m_resume_fail
    jmp smp1c_park
smp1c_fpcheck:
    SMP1_FPCHECK_BODY

    SMP1_STR smp1c_m_entered, "SMP1_CLIENT_ENTERED cpu=0 result=ok"
    SMP1_STR smp1c_m_resume_ok, "SMP1_CLIENT_RESUME_CONTEXT_OK cpu=0 path=production_selection gprs=6 fxsave=1 reply=ok result=ok"
    SMP1_STR smp1c_m_resume_fail, "SMP1_CLIENT_RESUME_CONTEXT_FAIL cpu=0 result=fail"
    SMP1_STR smp1c_m_arm, "SMP1_ARM_R target=server"
    SMP1_STR smp1c_m_req_done, "SMP1_REQUESTER_DONE cpu=0 vm_map=ok result=ok"
    SMP1_STR smp1c_m_target_ok, "SMP1_TARGET_OBSERVED cpu=0 w=replaced r=stale context=ok result=ok"
    SMP1_STR smp1c_m_lost, "SMP1_TARGET_RESIDENCY_LOST cpu=0 r=reloaded result=fail"
    SMP1_STR smp1c_m_stale, "SMP1_TARGET_STALE_AFTER_ACK cpu=0 w=old r=stale result=fail"
    SMP1_STR smp1c_m_mutual_ok, "SMP1_MUTUAL_OBSERVED cpu=0 w=replaced result=ok"
    SMP1_STR smp1c_m_mutual_stale, "SMP1_MUTUAL_STALE_AFTER_ACK cpu=0 w=old result=fail"
    SMP1_STR smp1c_m_rounds_done, "SMP1_ROUNDS_DONE cpu=0 result=ok"
    SMP1_STR smp1c_m_again, "SMP1_CLIENT_RECV_AGAIN_RETURNED cpu=0 result=fail"
    SMP1_STR smp1c_m_setup_fail, "SMP1_CLIENT_SETUP_FAIL cpu=0 result=fail"
    SMP1_STR smp1c_m_nr6_fail, "SMP1_CLIENT_NR6_FAIL cpu=0 result=fail"
    SMP1_STR smp1c_m_resident_fail, "SMP1_CLIENT_RESIDENT_CONTEXT_FAIL cpu=0 result=fail"
    SMP1_STR smp1c_m_unexpected, "SMP1_CLIENT_UNEXPECTED_W cpu=0 result=fail"
    SMP1_STR smp1c_m_vmmap_fail, "SMP1_CLIENT_VM_MAP_FAIL cpu=0 result=fail"
    .balign 16
    .global yarm_smp1_client_end
yarm_smp1_client_end:

    .global yarm_smp1_server_recv_cap_1
    .global yarm_smp1_server_recv_cap_2
    .global yarm_smp1_server_client_as_cap
    .global yarm_smp1_server_client_as_cap_m
    .global yarm_smp1_client_reply_cap_1
    .global yarm_smp1_client_reply_cap_2
    .global yarm_smp1_client_server_as_cap
    .global yarm_smp1_client_server_as_cap_m
    .popsection
