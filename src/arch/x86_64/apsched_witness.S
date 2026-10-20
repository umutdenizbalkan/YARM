/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * AP-SCHED4 — the x86_64 production SMP-scheduling witness user program, for every admitted CPU.
 * The port of `arch/aarch64/apsched_witness.S`: the same phases, mailbox, record steps and grading,
 * with x86_64's own user context checked.
 *
 * ONE App-class process P, kernel-built at 0x2000_0000. Everything after that is production
 * syscalls: P spawns its workers as threads (NR 11) and forks its churn children (NR 12), and the
 * production balanced placement — not the witness — decides which CPU each lands on and fixes it
 * as that task's home. No task is pinned, and no identity but P's is fixed.
 *
 * FIXTURE SYNCHRONIZATION (labelled, not production behaviour): as in the other ports, each worker
 * set spins on a gate word (no kernel entry) until P has spawned the whole set, so the ordinary
 * load-balanced placement spreads the set one per CPU; churn children spin a fixed count before
 * they exit for the same reason.
 *
 * Syscall ABI: rax = NR, rdi rsi rdx r10 r8 r9 the arguments, `syscall`; the result comes back in
 * rax and the error lane in rcx (0 = success); r8 and rdx are the other return lanes, and `syscall`
 * itself clobbers rcx and r11. So across a syscall only rbx rbp rsi rdi r9 r10 r12..r15 survive.
 *
 * Registers in P: rbx the wait's yield budget, rbp the wait's clock deadline, r12 the expected
 * secondaries' bitmap, r13 their count, r14 and r15 loop state; everything else is a temporary.
 * P's other state lives in BUF + 0xF00 (`V_*`).
 *
 * The spinner's checked context, against a per-thread pattern (base = PAT_HI << 48 | slot << 32):
 * rax rdx rbx rbp rsi rdi r8 r9 r10 r12..r15 = base + their number (rcx and r11 are the spin loop's
 * own instrument, and `syscall` clobbers them anyway); RSP (its slot's top); the permitted RFLAGS
 * bits CF PF AF ZF SF DF OF; XMM0..15, both halves; the x87 data registers ST0..ST7 and the x87
 * control word; MXCSR; and TLS — the FS base, read through `fs:[0]`. RFLAGS is saved by `pushfq`
 * before anything can change it. A mismatch records how many and the last one's index: a GPR is
 * its number, RSP = 31, RFLAGS = 32, FCW = 33, MXCSR = 34, TLS = 35, ST(n) = 40 + n,
 * XMMn low = 64 + n, XMMn high = 96 + n.
 */
    .pushsection .text.yarm_apsw_witness, "ax", @progbits

    .set MBX, 0x20070000
    .set BUF, 0x20080000
    .set WSTK, 0x20020000

    .set NR_YIELD, 0
    .set NR_SEND, 1
    .set NR_RECV, 2
    .set NR_RECV_TO, 5
    .set NR_FUTEX_WAIT, 9
    .set NR_FUTEX_WAKE, 10
    .set NR_SPAWN, 11
    .set NR_FORK, 12
    .set NR_LOG, 15
    .set NR_EXIT, 16

    .set M_AP_READY, 0x000
    .set M_STOP, 0x008
    .set M_SPIN_DONE, 0x010
    .set M_SPIN_BAD, 0x018
    .set M_EXPECTED, 0x020
    .set M_GATE, 0x028
    .set M_D_ARMED, 0x030
    .set M_REAPED, 0x038
    .set M_D_DONE, 0x040
    .set M_T_TURN, 0x048
    .set M_T_DONE, 0x050
    .set M_W_STARTED, 0x058
    .set M_CLOCK, 0x060
    .set M_W_CPUS, 0x068
    .set M_PRE, 0x100
    .set M_IDLE, 0x140
    .set M_F, 0x180
    .set M_S_WAITING, 0x1a0
    .set M_S_ACK, 0x1e0
    .set M_TLS, 0x800
    .set B_CHILD, 0x800
    .set B_IPC, 0x900
    .set B_PRESNAP, 0xE00
    .set V_NSPIN, 0xF00
    .set V_TMP, 0xF08
    .set V_TMP2, 0xF10
    .set V_TMP3, 0xF18

    .set D_SLOT0, 0
    .set SPIN_SLOT0, 8
    .set SPIN_SLOTS, 24
    .set SLEEP_SLOT0, 32
    .set IPC_SLOT0, 40

    .set GATE_D, 1
    .set GATE_S, 2

    .set PRE0_MIN, 3
    .set PRE_MIN, 4
    /* x86_64's timer period is 0.8 s (`BOOTSTRAP_TIMER_DEADLINE_TICKS`): the deadline is five global
       ticks, and each bounded wait's clock budget 150 ticks (two minutes): far above any healthy phase
       (seconds), and short enough that a control breaking several phases still reaches the dump. */
    .set DEADLINE_TICKS, 5
    .set WAKE_ROUNDS, 3
    .set IPC_ROUNDS, 4
    .set FORK_ROUNDS, 4
    .set CHILD_SPIN, 30000000

    .set WAIT_YIELDS, 400000
    .set WAIT_TICKS, 150
    /* A spawned or forked TID is never below this. */
    .set MIN_TID, 64
    .set PAT_HI, 0xA5A5
    /* The permitted user RFLAGS bits: CF PF AF ZF SF DF OF. */
    .set FLAG_MASK, 0xCD5

    .macro SYS nr
    mov eax, \nr
    syscall
    .endm

    .macro YIELD
    SYS NR_YIELD
    .endm

    /* P's wait step: one tick in a timed receive on P's private nap endpoint (nothing ever sends to
       it), not a yield storm. Clobbers rax rcx rdx rsi rdi r8 r9 r10 r11. */
    .macro NAP
    mov edi, dword ptr [rip + yarm_apsw_cap_ep_n]
    mov esi, (BUF + 0x780)
    mov edx, 16
    mov r10d, 1
    xor r8d, r8d
    xor r9d, r9d
    SYS NR_RECV_TO
    .endm

    /* DebugLog of one of this program's strings; rdx = round/aux, r10 = aux (set by the caller).
       Clobbers rax rcx rdi rsi r8 r11 (and rdx). */
    .macro LOG msg
    lea rdi, [rip + \msg]
    lea rsi, [rip + \msg\()_end]
    sub rsi, rdi
    SYS NR_LOG
    .endm

    .macro STR name, text
\name: .ascii "\text"
\name\()_end:
    .endm

    /* NR 11: a thread at `entry` on worker stack slot \slot (a register, not rdi/rsi/rdx), its TLS
       base its slot's MBX cell; rax = its TID and rcx = 0, or rcx = the error. */
    .macro SPAWNR slot, entry
    mov rdi, \slot
    shl rdi, 4
    add rdi, (MBX + M_TLS)
    mov rsi, \slot
    shl rsi, 12
    add rsi, (WSTK + 0xff0)
    lea rdx, [rip + \entry]
    xor r10d, r10d
    xor r8d, r8d
    xor r9d, r9d
    SYS NR_SPAWN
    .endm

    /* A spawn/fork result check: success means rcx == 0 and rax >= MIN_TID. */
    .macro SPAWN_OK fail
    test rcx, rcx
    jnz \fail
    cmp rax, MIN_TID
    jb \fail
    .endm

    /* This worker's slot, from its own stack, into \reg. */
    .macro MYSLOT reg
    mov \reg, rsp
    sub \reg, WSTK
    shr \reg, 12
    .endm

    /* Start a bounded wait: rbx = the yield budget, rbp = the clock deadline. */
    .macro WAIT_BEGIN
    mov ebx, WAIT_YIELDS
    mov rbp, qword ptr [MBX + M_CLOCK]
    add rbp, WAIT_TICKS
    .endm

    /* One more round of a bounded wait: back to \again unless the budget is spent or the clock
       deadline has passed; otherwise fall through. Clobbers rax. */
    .macro WAIT_NEXT again
    dec rbx
    jz .Lwn_out\@
    mov rax, qword ptr [MBX + M_CLOCK]
    cmp rax, rbp
    jb \again
.Lwn_out\@:
    .endm

    /* Bounded: wait until the kernel reports EVERY expected secondary (r12) idle. */
    .macro AWAIT_ALL_IDLE
    WAIT_BEGIN
.Laai_top\@:
    mov rax, r12
    xor ecx, ecx
.Laai_scan\@:
    test rax, rax
    jz .Laai_done\@
    bt rax, 0
    jnc .Laai_next\@
    cmp qword ptr [MBX + M_IDLE + rcx * 8], 0
    je .Laai_wait\@
.Laai_next\@:
    shr rax, 1
    inc rcx
    jmp .Laai_scan\@
.Laai_wait\@:
    NAP
    WAIT_NEXT .Laai_top\@
    jmp p_no_idle
.Laai_done\@:
    .endm

    /* Bounded: wait until the u64 at MBX+\off equals \reg (a register that survives a NAP); on
       timeout rdx = what it held, r10 = \reg, and control goes to \fail. */
    .macro AWAIT_EQ off, reg, fail
    WAIT_BEGIN
.Lae_top\@:
    mov rax, qword ptr [MBX + \off]
    cmp rax, \reg
    je .Lae_done\@
    NAP
    WAIT_NEXT .Lae_top\@
    mov rdx, qword ptr [MBX + \off]
    mov r10, \reg
    jmp \fail
.Lae_done\@:
    .endm

    /* The same, for "at least \reg". */
    .macro AWAIT_GE off, reg, fail
    WAIT_BEGIN
.Lag_top\@:
    mov rax, qword ptr [MBX + \off]
    cmp rax, \reg
    jae .Lag_done\@
    NAP
    WAIT_NEXT .Lag_top\@
    mov rdx, qword ptr [MBX + \off]
    mov r10, \reg
    jmp \fail
.Lag_done\@:
    .endm

    /* NR 16, and a report if the kernel declined it. */
    .macro EXIT
    xor edi, edi
    SYS NR_EXIT
    mov r10, rcx
    xor edx, edx
    LOG m_exit_refused
    jmp park
    .endm

    /* One spinner check: \reg against r11 + \n; a mismatch counts at [rsp-8] and keeps the index
       at [rsp-16]. Clobbers rcx and the flags (already saved). */
    .macro CHK reg, n, idx
    lea rcx, [r11 + \n]
    cmp \reg, rcx
    je .Lchk\@
    inc qword ptr [rsp - 8]
    mov qword ptr [rsp - 16], \idx
.Lchk\@:
    .endm

    /* The same, with the expected value in rcx and the observed in rax. */
    .macro CHKAC idx
    cmp rax, rcx
    je .Lchkac\@
    inc qword ptr [rsp - 8]
    mov qword ptr [rsp - 16], \idx
.Lchkac\@:
    .endm

/* ═══════════════════════════════════════ P ═════════════════════════════════════════════════ */
    .balign 16
    .global yarm_apsw_start
yarm_apsw_start:
    xor edx, edx
    xor r10d, r10d
    LOG m_p_start
    /* Every released secondary admitted: AP_READY == EXPECTED != 0. */
    WAIT_BEGIN
p_ap:
    mov rax, qword ptr [MBX + M_EXPECTED]
    test rax, rax
    jz 1f
    cmp rax, qword ptr [MBX + M_AP_READY]
    je p_ready
1:  NAP
    WAIT_NEXT p_ap
    mov rdx, qword ptr [MBX + M_EXPECTED]
    mov r10, qword ptr [MBX + M_AP_READY]
    LOG m_p_no_ap
    jmp p_end
p_ready:
    mov r12, qword ptr [MBX + M_EXPECTED]
    /* r13 = the number of bits in r12 (no POPCNT: the qemu64 model lacks it). */
    xor r13d, r13d
    mov rax, r12
.Lp_count:
    test rax, rax
    jz .Lp_counted
    lea rcx, [rax - 1]
    and rax, rcx
    inc r13
    jmp .Lp_count
.Lp_counted:
    mov rdx, r12
    mov r10, r13
    LOG m_p_ap

    /* ── 0: one deadline thread per secondary, spread by the gate ── */
    mov r14d, 1
1:  cmp r14, r13
    ja 2f
    lea r15, [r14 + D_SLOT0]
    SPAWNR r15, w_deadline
    SPAWN_OK p_spawn_fail
    inc r14
    jmp 1b
2:  mov qword ptr [MBX + M_GATE], GATE_D
    AWAIT_EQ M_D_ARMED, r13, p_a_timeout

    /* ── A: 2N + 2 spinners ── */
    lea r15, [r13 * 2 + 2]
    mov qword ptr [BUF + V_NSPIN], r15
    xor r14d, r14d
1:  cmp r14, r15
    jae 2f
    lea rax, [r14 + SPIN_SLOT0]
    mov qword ptr [BUF + V_TMP], rax
    mov r9, rax
    SPAWNR r9, w_spin
    SPAWN_OK p_spawn_fail
    inc r14
    jmp 1b
    /* ONE deadline covers the whole phase. */
2:  WAIT_BEGIN
p_w:
    mov rax, qword ptr [MBX + M_W_STARTED]
    cmp rax, qword ptr [BUF + V_NSPIN]
    je p_w_cover
    NAP
    WAIT_NEXT p_w
    mov rdx, qword ptr [MBX + M_W_STARTED]
    mov r10, qword ptr [BUF + V_NSPIN]
    LOG m_p_w_timeout
    xor eax, eax
1:  mov qword ptr [BUF + B_PRESNAP + rax * 8], 0
    inc rax
    cmp rax, 8
    jne 1b
    jmp p_pre_begin
    /* Until a spinner has STARTED on CPU 0 and on every expected secondary (M_W_CPUS), place one
       more through the same placement, up to the spinner slots and under the same deadline. */
p_w_cover:
    mov rax, r12
    or rax, 1
    mov rcx, qword ptr [MBX + M_W_CPUS]
    not rcx
    test rax, rcx
    jz p_w_ok
    mov r15, qword ptr [BUF + V_NSPIN]
    cmp r15, SPIN_SLOTS
    jae p_w_ok
    lea r9, [r15 + SPIN_SLOT0]
    SPAWNR r9, w_spin
    SPAWN_OK p_spawn_fail
    inc qword ptr [BUF + V_NSPIN]
    jmp p_w
p_w_ok:
    mov rdx, qword ptr [BUF + V_NSPIN]
    mov r10, qword ptr [MBX + M_W_CPUS]
    LOG m_p_w_cover
    xor eax, eax
1:  mov rcx, qword ptr [MBX + M_PRE + rax * 8]
    mov qword ptr [BUF + B_PRESNAP + rax * 8], rcx
    inc rax
    cmp rax, 8
    jne 1b
p_pre_begin:
    mov ebx, WAIT_YIELDS
p_pre:
    mov rax, qword ptr [MBX + M_PRE]
    sub rax, qword ptr [BUF + B_PRESNAP]
    cmp rax, PRE0_MIN
    jb 3f
    mov rsi, r12
    xor edi, edi
1:  test rsi, rsi
    jz p_stop
    bt rsi, 0
    jnc 2f
    mov rax, qword ptr [MBX + M_PRE + rdi * 8]
    sub rax, qword ptr [BUF + B_PRESNAP + rdi * 8]
    cmp rax, PRE_MIN
    jb 3f
2:  shr rsi, 1
    inc rdi
    jmp 1b
3:  NAP
    WAIT_NEXT p_pre
    /* Phase A's deadline: rdx = CPUs below their minimum since the snapshot; r10 = those with no
       timer preemption at all. Then stop the spinners, let them report, and end. */
    mov rsi, r12
    or rsi, 1
    xor edi, edi
    xor edx, edx
    xor r10d, r10d
1:  test rsi, rsi
    jz 4f
    bt rsi, 0
    jnc 2f
    mov rax, qword ptr [MBX + M_PRE + rdi * 8]
    test rax, rax
    jnz 5f
    bts r10, rdi
5:  sub rax, qword ptr [BUF + B_PRESNAP + rdi * 8]
    mov ecx, PRE_MIN
    test rdi, rdi
    jnz 6f
    mov ecx, PRE0_MIN
6:  cmp rax, rcx
    jae 2f
    bts rdx, rdi
2:  shr rsi, 1
    inc rdi
    jmp 1b
4:  LOG m_p_pre_timeout
    mov qword ptr [MBX + M_STOP], 1
    mov r15, qword ptr [MBX + M_W_STARTED]
    AWAIT_GE M_SPIN_DONE, r15, p_end
    jmp p_end
p_stop:
    mov qword ptr [MBX + M_STOP], 1
    mov r15, qword ptr [BUF + V_NSPIN]
    AWAIT_EQ M_SPIN_DONE, r15, p_a_timeout
    AWAIT_EQ M_D_DONE, r13, p_a_timeout
    mov rdx, qword ptr [MBX + M_SPIN_DONE]
    mov r10, qword ptr [MBX + M_SPIN_BAD]
    LOG m_p_a_done

    /* ── B: one sleeper per secondary, woken on its idle CPU ── */
    AWAIT_ALL_IDLE
    mov r14d, 1
1:  cmp r14, r13
    ja 2f
    lea r9, [r14 + SLEEP_SLOT0]
    SPAWNR r9, w_sleep
    SPAWN_OK p_spawn_fail
    inc r14
    jmp 1b
2:  mov qword ptr [MBX + M_GATE], GATE_S
    mov r15d, 1
p_b_round:
    mov r14d, 1
p_b_one:
    /* sleeper r14 announced round r15? */
    WAIT_BEGIN
1:  cmp qword ptr [MBX + M_S_WAITING + r14 * 8], r15
    je 2f
    NAP
    WAIT_NEXT 1b
    mov rdx, r14
    shl rdx, 8
    or rdx, r15
    xor r10d, r10d
    LOG m_p_b_timeout
    jmp p_end
2:  AWAIT_ALL_IDLE
    /* F[r14] = round; wake it. */
    mov dword ptr [MBX + M_F + r14 * 4], r15d
    mov rdx, r14
    shl rdx, 8
    or rdx, r15
    xor r10d, r10d
    LOG m_p_wake
    lea rdi, [MBX + M_F + r14 * 4]
    mov esi, 1
    SYS NR_FUTEX_WAKE
    mov qword ptr [BUF + V_TMP], rax
    WAIT_BEGIN
3:  cmp qword ptr [MBX + M_S_ACK + r14 * 8], r15
    je 4f
    NAP
    WAIT_NEXT 3b
    mov rdx, r14
    shl rdx, 8
    or rdx, r15
    xor r10d, r10d
    LOG m_p_no_ack
    jmp p_end
4:  mov rdx, r14
    shl rdx, 8
    or rdx, r15
    mov r10, qword ptr [BUF + V_TMP]
    LOG m_p_acked
    inc r14
    cmp r14, r13
    jbe p_b_one
    inc r15
    cmp r15, (WAKE_ROUNDS + 1)
    jne p_b_round

    /* ── C: ping-pong with each IPC thread in turn ── */
p_c:
    AWAIT_ALL_IDLE
    mov qword ptr [MBX + M_T_TURN], 0
    mov r14d, 1
1:  cmp r14, r13
    ja 2f
    lea r9, [r14 + IPC_SLOT0]
    SPAWNR r9, w_ipc
    SPAWN_OK p_spawn_fail
    inc r14
    jmp 1b
2:  mov r14d, 1
p_c_thread:
    /* The previous thread has read its last pong before the next one starts. */
    /* (r15, free until the rounds start, holds the count: a NAP clobbers r9.) */
    lea r15, [r14 - 1]
    AWAIT_EQ M_T_DONE, r15, p_c_timeout
    mov qword ptr [MBX + M_T_TURN], r14
    mov r15d, 1
p_c_round:
    xor r9d, r9d
p_c_recv:
    /* recv-v2: success is rcx == 0 (or the kernel wrote the metadata status), so the status is
       poisoned first; WouldBlock (7) with it untouched is "nothing yet" and is received again,
       bounded. */
    inc r9
    mov qword ptr [BUF + 0x40], -1
    mov qword ptr [BUF + V_TMP3], r9
    mov edi, dword ptr [rip + yarm_apsw_cap_ep_p]
    mov esi, BUF
    mov edx, 32
    mov r10d, (BUF + 0x40)
    mov r8d, 0x28
    xor r9d, r9d
    SYS NR_RECV
    mov r9, qword ptr [BUF + V_TMP3]
    mov qword ptr [BUF + V_TMP], rcx
    cmp rcx, 7
    jne 5f
    cmp qword ptr [BUF + 0x40], -1
    jne 5f
    cmp r9, 64
    jae 5f
    YIELD
    jmp p_c_recv
    /* The received frame: opcode +0, round +2, thread +4. */
5:  xor r10d, r10d
    cmp qword ptr [BUF + V_TMP], 0
    je 0f
    cmp qword ptr [BUF + 0x40], -1
    je 1f
0:  cmp word ptr [BUF + 0], 0x5050
    jne 1f
    cmp word ptr [BUF + 2], r15w
    jne 1f
    cmp word ptr [BUF + 4], r14w
    jne 1f
    mov r10d, 1
1:  mov rdx, r14
    shl rdx, 8
    or rdx, r15
    LOG m_p_ping
    mov word ptr [BUF + 0x200], 0x4F50
    mov word ptr [BUF + 0x202], r15w
    mov word ptr [BUF + 0x204], r14w
    mov word ptr [BUF + 0x206], 0x4B4F
    xor r9d, r9d
3:  inc r9
    mov qword ptr [BUF + V_TMP3], r9
    mov edi, dword ptr [rip + yarm_apsw_cap_ep_t]
    mov esi, (BUF + 0x200)
    mov edx, 8
    xor r10d, r10d
    xor r8d, r8d
    mov r9, -1
    SYS NR_SEND
    mov r9, qword ptr [BUF + V_TMP3]
    test rcx, rcx
    jz 4f
    cmp r9, 64
    jae 4f
    YIELD
    jmp 3b
4:  inc r15
    cmp r15, (IPC_ROUNDS + 1)
    jne p_c_round
    inc r14
    cmp r14, r13
    jbe p_c_thread
    AWAIT_EQ M_T_DONE, r13, p_c_timeout

    /* ── D: per round, fork one child per secondary; reap each ── */
    mov r15d, 1
p_d_round:
    AWAIT_ALL_IDLE
    mov r14d, 1
p_d_fork:
    cmp r14, r13
    ja p_d_reap
    xor edi, edi
    xor esi, esi
    xor edx, edx
    xor r10d, r10d
    SYS NR_FORK
    test rcx, rcx
    jnz p_fork_fail
    test rax, rax
    jz c_child
    cmp rax, MIN_TID
    jb p_fork_fail
    mov qword ptr [BUF + B_CHILD + r14 * 8], rax
    mov rdx, rax
    mov r10, r15
    shl r10, 8
    or r10, r14
    LOG m_p_forked
    inc r14
    jmp p_d_fork
p_d_reap:
    mov r14d, 1
p_d_reap_one:
    WAIT_BEGIN
    mov ebx, (WAIT_YIELDS / 8)
1:  mov rdx, qword ptr [BUF + B_CHILD + r14 * 8]
    mov r10, r15
    shl r10, 8
    or r10, r14
    LOG m_p_reap
    mov rax, qword ptr [MBX + M_REAPED]
    cmp rax, qword ptr [BUF + B_CHILD + r14 * 8]
    je 2f
    NAP
    WAIT_NEXT 1b
    mov rdx, qword ptr [BUF + B_CHILD + r14 * 8]
    mov r10, r15
    shl r10, 8
    or r10, r14
    LOG m_p_reap_timeout
    jmp p_end
2:  inc r14
    cmp r14, r13
    jbe p_d_reap_one
    inc r15
    cmp r15, (FORK_ROUNDS + 1)
    jne p_d_round
    jmp p_end
p_a_timeout:
    LOG m_p_a_timeout
    jmp p_end
p_c_timeout:
    LOG m_p_c_timeout
    jmp p_end
p_no_idle:
    mov rsi, r12
    xor edi, edi
    xor edx, edx
1:  test rsi, rsi
    jz 3f
    bt rsi, 0
    jnc 2f
    cmp qword ptr [MBX + M_IDLE + rdi * 8], 0
    jne 2f
    bts rdx, rdi
2:  shr rsi, 1
    inc rdi
    jmp 1b
3:  xor r10d, r10d
    LOG m_p_no_idle
p_end:
    xor edx, edx
    xor r10d, r10d
    LOG m_p_end
    jmp park
p_spawn_fail:
    mov r10, rcx
    mov rdx, rax
    LOG m_p_spawn_fail
    jmp p_end
p_fork_fail:
    mov rdx, rax
    mov r10, rcx
    LOG m_p_fork_fail
    jmp p_end
park:
    YIELD
    jmp park

/* The churn child (r14 = its index, r15 = the round, both inherited from the fork): spins so the
   next fork sees its CPU busy, takes its copy-on-write stack page, reports and exits. */
c_child:
    mov eax, CHILD_SPIN
1:  dec rax
    jnz 1b
    mov qword ptr [rsp - 64], r14
    mov r10, qword ptr [rsp - 64]
    mov rdx, r15
    shl rdx, 8
    or rdx, r14
    LOG m_c_run
    EXIT

/* ═══════════════════════════════════ workers ══════════════════════════════════════════════ */
/* A deadline receive on an endpoint nobody sends to, after the spawn gate. */
w_deadline:
1:  cmp qword ptr [MBX + M_GATE], GATE_D
    jne 1b
    xor edx, edx
    xor r10d, r10d
    LOG m_d_before
    lock add qword ptr [MBX + M_D_ARMED], 1
    mov edi, dword ptr [rip + yarm_apsw_cap_ep_d]
    mov esi, (BUF + 0x300)
    mov edx, 16
    mov r10d, DEADLINE_TICKS
    xor r8d, r8d
    xor r9d, r9d
    SYS NR_RECV_TO
    /* The outcome: the error lane (TimedOut = 9), else the result. */
    mov r10, rcx
    test rcx, rcx
    jnz 1f
    mov r10, rax
1:  mov edx, DEADLINE_TICKS
    LOG m_d_after
    lock add qword ptr [MBX + M_D_DONE], 1
    EXIT

/* A spinner: its whole user context patterned, no kernel entry until P stops it, then the check. */
w_spin:
    xor edx, edx
    xor r10d, r10d
    LOG m_w_start
    lock add qword ptr [MBX + M_W_STARTED], 1
    /* r15 = slot; r11 = base = PAT_HI << 48 | slot << 32. */
    MYSLOT r15
    mov r11, r15
    shl r11, 32
    mov rax, PAT_HI
    shl rax, 48
    or r11, rax
    /* TLS: this thread's FS base is its slot's MBX cell; write base + 0x300 through it. */
    lea rax, [r11 + 0x300]
    mov qword ptr fs:[0], rax
    /* XMM0..15: low half base + 0x100 + n, high half base + 0x200 + n (built at [rsp - 96]). */
    .irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15
    lea rax, [r11 + 0x100 + \n]
    mov qword ptr [rsp - 96], rax
    lea rax, [r11 + 0x200 + \n]
    mov qword ptr [rsp - 88], rax
    movdqu xmm\n, xmmword ptr [rsp - 96]
    .endr
    /* x87: RC = slot & 3, PC = 11 (extended) or 10 (double) by slot bit 2, every exception masked;
       then ST7..ST0 = base + 0x400 + 0..7 (ST0 holds n = 7). */
    fninit
    mov rax, r15
    and eax, 3
    shl eax, 10
    or eax, 0x037F
    bt r15, 2
    jnc 1f
    and eax, 0xFEFF
1:  mov word ptr [rsp - 104], ax
    fldcw word ptr [rsp - 104]
    .irp n, 0,1,2,3,4,5,6,7
    lea rax, [r11 + 0x400 + \n]
    mov qword ptr [rsp - 112], rax
    fild qword ptr [rsp - 112]
    .endr
    /* MXCSR: every exception masked, RC = (slot >> 3) & 3, FZ = slot bit 1. */
    mov rax, r15
    shr eax, 3
    and eax, 3
    shl eax, 13
    or eax, 0x1F80
    bt r15, 1
    jnc 1f
    or eax, 0x8000
1:  mov dword ptr [rsp - 116], eax
    ldmxcsr dword ptr [rsp - 116]
    /* RFLAGS: (slot * 0x4B) & FLAG_MASK, with IF and the reserved bit 1. Nothing below may change
       the flags until the check saves them: only `mov`, `lea` and `jrcxz` follow. */
    imul eax, r15d, 0x4B
    and eax, FLAG_MASK
    or eax, 0x202
    push rax
    popfq
    /* The GPRs last: base + their number. */
    lea rax, [r11 + 0]
    lea rdx, [r11 + 2]
    lea rbx, [r11 + 3]
    lea rbp, [r11 + 5]
    lea rsi, [r11 + 6]
    lea rdi, [r11 + 7]
    lea r8, [r11 + 8]
    lea r9, [r11 + 9]
    lea r10, [r11 + 10]
    lea r12, [r11 + 12]
    lea r13, [r11 + 13]
    lea r14, [r11 + 14]
    lea r15, [r11 + 15]
    /* r11 = &STOP; spin on it (mov / jrcxz change no flags). */
    mov r11d, (MBX + M_STOP)
w_spin_loop:
    mov rcx, qword ptr [r11]
    jrcxz w_spin_loop
    /* RFLAGS first, before anything can change it. */
    pushfq
    mov rcx, qword ptr [rsp]
    mov qword ptr [rsp - 24], rcx
    pop rcx
    mov qword ptr [rsp - 32], rcx
    mov qword ptr [rsp - 8], 0
    mov qword ptr [rsp - 16], 0
    /* r11 = base again, from rsp alone. */
    mov r11, rsp
    sub r11, WSTK
    shr r11, 12
    mov qword ptr [rsp - 40], r11
    shl r11, 32
    mov rcx, PAT_HI
    shl rcx, 48
    or r11, rcx
    CHK rax, 0, 0
    CHK rdx, 2, 2
    CHK rbx, 3, 3
    CHK rbp, 5, 5
    CHK rsi, 6, 6
    CHK rdi, 7, 7
    CHK r8, 8, 8
    CHK r9, 9, 9
    CHK r10, 10, 10
    CHK r12, 12, 12
    CHK r13, 13, 13
    CHK r14, 14, 14
    CHK r15, 15, 15
    /* Every GPR is checked: rax rdx rbx are free now. rbx = slot. */
    mov rbx, qword ptr [rsp - 40]
    /* RSP: exactly this slot's top. */
    mov rax, rsp
    mov rcx, rbx
    shl rcx, 12
    add rcx, (WSTK + 0xff0)
    CHKAC 31
    /* RFLAGS, as saved before the first compare. */
    mov rax, qword ptr [rsp - 32]
    and eax, FLAG_MASK
    imul ecx, ebx, 0x4B
    and ecx, FLAG_MASK
    CHKAC 32
    /* FCW. */
    fnstcw word ptr [rsp - 104]
    movzx eax, word ptr [rsp - 104]
    mov rcx, rbx
    and ecx, 3
    shl ecx, 10
    or ecx, 0x037F
    bt rbx, 2
    jnc 1f
    and ecx, 0xFEFF
1:  CHKAC 33
    /* MXCSR. */
    stmxcsr dword ptr [rsp - 116]
    mov eax, dword ptr [rsp - 116]
    mov rcx, rbx
    shr ecx, 3
    and ecx, 3
    shl ecx, 13
    or ecx, 0x1F80
    bt rbx, 1
    jnc 1f
    or ecx, 0x8000
1:  CHKAC 34
    /* TLS: FS still names this slot's cell. */
    mov rax, qword ptr fs:[0]
    lea rcx, [r11 + 0x300]
    CHKAC 35
    /* ST0..ST7: ST0 holds n = 7, ST7 holds n = 0. */
    .irp k, 0,1,2,3,4,5,6,7
    fistp qword ptr [rsp - 112]
    mov rax, qword ptr [rsp - 112]
    lea rcx, [r11 + 0x400 + 7 - \k]
    CHKAC (40 + \k)
    .endr
    /* XMM0..15, both halves. */
    .irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15
    movdqu xmmword ptr [rsp - 96], xmm\n
    mov rax, qword ptr [rsp - 96]
    lea rcx, [r11 + 0x100 + \n]
    CHKAC (64 + \n)
    mov rax, qword ptr [rsp - 88]
    lea rcx, [r11 + 0x200 + \n]
    CHKAC (96 + \n)
    .endr
    mov r14, qword ptr [rsp - 8]
    mov r15, qword ptr [rsp - 16]
    lock add qword ptr [MBX + M_SPIN_BAD], r14
    lock add qword ptr [MBX + M_SPIN_DONE], 1
    mov rdx, r14
    mov r10, r15
    LOG m_w_done
    EXIT

/* Sleeper k (= slot - SLEEP_SLOT0): after the spawn gate, announce round r, wait on F[k] until it
   holds r, acknowledge. */
w_sleep:
    MYSLOT r12
    sub r12, SLEEP_SLOT0
1:  cmp qword ptr [MBX + M_GATE], GATE_S
    jne 1b
    mov rdx, r12
    xor r10d, r10d
    LOG m_s_start
    mov r15d, 1
1:  mov qword ptr [MBX + M_S_WAITING + r12 * 8], r15
2:  mov eax, dword ptr [MBX + M_F + r12 * 4]
    cmp rax, r15
    je 3f
    lea rdi, [MBX + M_F + r12 * 4]
    lea rsi, [r15 - 1]
    mov rdx, rax
    SYS NR_FUTEX_WAIT
    jmp 2b
3:  mov qword ptr [MBX + M_S_ACK + r12 * 8], r15
    mov rdx, r12
    shl rdx, 8
    or rdx, r15
    mov r10d, dword ptr [MBX + M_F + r12 * 4]
    LOG m_s_woke
    inc r15
    cmp r15, (WAKE_ROUNDS + 1)
    jne 1b
    EXIT

/* IPC thread k (= slot - IPC_SLOT0): spins until its turn, then pings P and receives its pong. */
w_ipc:
    MYSLOT r12
    sub r12, IPC_SLOT0
    /* r13 = this thread's own buffers (send +0x00, receive +0x20, metadata +0x40). */
    mov r13, r12
    shl r13, 7
    add r13, (BUF + B_IPC)
1:  cmp qword ptr [MBX + M_T_TURN], r12
    jne 1b
    mov rdx, r12
    xor r10d, r10d
    LOG m_t_start
    mov r15d, 1
t_round:
    mov word ptr [r13 + 0], 0x5050
    mov word ptr [r13 + 2], r15w
    mov word ptr [r13 + 4], r12w
    mov word ptr [r13 + 6], 0x4749
    xor r14d, r14d
1:  inc r14
    mov edi, dword ptr [rip + yarm_apsw_cap_ep_p]
    mov rsi, r13
    mov edx, 8
    xor r10d, r10d
    xor r8d, r8d
    mov r9, -1
    SYS NR_SEND
    test rcx, rcx
    jz 2f
    cmp r14, 64
    jae 2f
    YIELD
    jmp 1b
2:  xor r14d, r14d
t_recv:
    inc r14
    mov qword ptr [r13 + 0x40], -1
    mov edi, dword ptr [rip + yarm_apsw_cap_ep_t]
    lea rsi, [r13 + 0x20]
    mov edx, 32
    lea r10, [r13 + 0x40]
    mov r8d, 0x28
    xor r9d, r9d
    SYS NR_RECV
    mov rbx, rcx
    cmp rcx, 7
    jne 5f
    cmp qword ptr [r13 + 0x40], -1
    jne 5f
    cmp r14, 64
    jae 5f
    YIELD
    jmp t_recv
5:  xor r10d, r10d
    test rbx, rbx
    jz 4f
    cmp qword ptr [r13 + 0x40], -1
    je 3f
4:  cmp word ptr [r13 + 0x20], 0x4F50
    jne 3f
    cmp word ptr [r13 + 0x22], r15w
    jne 3f
    cmp word ptr [r13 + 0x24], r12w
    jne 3f
    mov r10d, 1
    jmp 6f
    /* A failed round records what the receive returned: bit 0 clear, bits 8.. the error code,
       bit 7 set when the metadata was written. */
3:  mov r10, rbx
    shl r10, 8
    cmp qword ptr [r13 + 0x40], -1
    je 6f
    or r10, 0x80
6:  mov rdx, r12
    shl rdx, 8
    or rdx, r15
    LOG m_t_pong
    inc r15
    cmp r15, (IPC_ROUNDS + 1)
    jne t_round
    lock add qword ptr [MBX + M_T_DONE], 1
    EXIT

    .balign 8
    .global yarm_apsw_cap_ep_p
yarm_apsw_cap_ep_p: .long 0x61616161
    .global yarm_apsw_cap_ep_t
yarm_apsw_cap_ep_t: .long 0x62626262
    .global yarm_apsw_cap_ep_d
yarm_apsw_cap_ep_d: .long 0x63636363
    .global yarm_apsw_cap_ep_n
yarm_apsw_cap_ep_n: .long 0x64646464

    STR m_p_start, "APS P_START"
    STR m_p_no_ap, "APS P_NO_AP"
    STR m_p_ap, "APS P_AP_READY"
    STR m_p_pre_timeout, "APS P_PRE_TIMEOUT"
    STR m_p_w_timeout, "APS P_W_TIMEOUT"
    STR m_p_w_cover, "APS P_W_COVER"
    STR m_p_a_done, "APS P_A_DONE"
    STR m_p_b_timeout, "APS P_B_TIMEOUT"
    STR m_p_a_timeout, "APS P_A_TIMEOUT"
    STR m_p_c_timeout, "APS P_C_TIMEOUT"
    STR m_p_no_idle, "APS P_NO_IDLE"
    STR m_p_wake, "APS P_WAKE"
    STR m_p_no_ack, "APS P_NO_ACK"
    STR m_p_acked, "APS P_ACKED"
    STR m_p_ping, "APS P_PING"
    STR m_p_forked, "APS P_FORKED"
    STR m_p_reap, "APS P_REAP"
    STR m_p_reap_timeout, "APS P_REAP_TIMEOUT"
    STR m_p_end, "APS P_END"
    STR m_p_spawn_fail, "APS P_SPAWN_FAIL"
    STR m_p_fork_fail, "APS P_FORK_FAIL"
    STR m_c_run, "APS C_RUN"
    STR m_d_before, "APS D_BEFORE"
    STR m_d_after, "APS D_AFTER"
    STR m_w_start, "APS W_START"
    STR m_w_done, "APS W_DONE"
    STR m_s_start, "APS S_START"
    STR m_s_woke, "APS S_WOKE"
    STR m_t_start, "APS T_START"
    STR m_t_pong, "APS T_PONG"
    STR m_exit_refused, "APS EXIT_REFUSED"
    .balign 16
    .global yarm_apsw_end
yarm_apsw_end:

    .popsection
