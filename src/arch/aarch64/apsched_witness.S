/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * AP-SCHED3 — the AArch64 production SMP-scheduling witness user program, for every admitted CPU.
 * The port of `arch/riscv64/apsched_witness.S`: the same phases, mailbox, record steps and
 * grading, with AArch64's own user context checked.
 *
 * ONE App-class process P, kernel-built at 0x2000_0000. Everything after that is production
 * syscalls: P spawns its workers as threads (NR 11) and forks its churn children (NR 12), and the
 * production balanced placement — not the witness — decides which CPU each lands on and fixes it
 * as that task's home. No task is pinned, and no identity but P's is fixed.
 *
 * FIXTURE SYNCHRONIZATION (labelled, not production behaviour): balanced placement counts a CPU's
 * runnable and running tasks, so a worker that blocks at once leaves its CPU looking empty and the
 * next spawn lands there too. Each worker set therefore spins on a gate word (no kernel entry)
 * until P has spawned the whole set: every earlier worker is load when the next is placed, and
 * the set spreads one per CPU through the ordinary policy. Churn children spin a fixed count
 * before they exit, for the same reason.
 *
 * Syscall ABI: x8 = NR, x0..x5 arguments, `svc #0`; the result — or an error code, there is no
 * separate error lane on this port — comes back in x0, and x0..x5 are the only lanes the kernel
 * writes. x18 is the platform register the kernel owns (the TLS base, restored on every return to
 * EL0); this program never uses it.
 *
 * User VAs, MBX and BUF: exactly the RISC-V program's (see there). Registers in P: x19 the wait's
 * yield budget, x20..x27 its loop state, x28 the wait's clock deadline, x29 the expected
 * secondaries' count, x30 their bitmap; x9..x17 are temporaries; x0..x5 and x8 are syscall lanes.
 *
 * The spinner's checked context, against a per-thread pattern: x0..x15 and x19..x30 (x16/x17 are
 * the spin loop's own instrument and x18 is the kernel's), SP, both halves of q0..q31, FPCR, FPSR,
 * NZCV and TPIDR_EL0. NZCV is saved before the first compare. A mismatch records how many and the
 * last one's index: xN = N, SP = 31, NZCV = 32, FPCR = 33, FPSR = 34, TPIDR_EL0 = 35,
 * qN low = 64 + N, qN high = 96 + N.
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
    .set B_CHILD, 0x800
    .set B_IPC, 0x900
    .set B_PRESNAP, 0xE00

    .set D_SLOT0, 0
    .set SPIN_SLOT0, 8
    .set SPIN_SLOTS, 24
    .set SLEEP_SLOT0, 32
    .set IPC_SLOT0, 40

    .set GATE_D, 1
    .set GATE_S, 2

    .set PRE0_MIN, 3
    .set PRE_MIN, 4
    .set DEADLINE_TICKS, 40
    .set WAKE_ROUNDS, 3
    .set IPC_ROUNDS, 4
    .set FORK_ROUNDS, 4
    .set CHILD_SPIN, 3000000

    /* Bounded waits: a yield count AND a deadline on the global clock (CPU 0's timer, which no
       control breaks), whichever ends first — exactly the RISC-V program's bounds. */
    .set WAIT_YIELDS, 400000
    .set WAIT_TICKS, 6000
    /* A spawned or forked TID is never below this; an error code always is. */
    .set MIN_TID, 64
    /* The spinners' pattern base; slot N adds N << 32 (bits 32..47 hold the slot). */
    .set PAT_HI, 0xA5A5

    /* Load a 32-bit constant. */
    .macro LI32 reg, imm
    movz \reg, #((\imm) & 0xffff)
    movk \reg, #(((\imm) >> 16) & 0xffff), lsl #16
    .endm

    .macro SVC nr
    mov x8, #\nr
    svc #0
    .endm

    .macro YIELD
    SVC NR_YIELD
    .endm

    /* P's wait step: block for one tick in a timed receive on P's private nap endpoint (nothing
       ever sends to it), instead of yielding. A yield storm floods the console, and on AArch64
       the console delays CPU 0's own interrupt handling, and so the very clock (M_CLOCK) every
       bounded wait measures its deadline on: a 6000-tick wait then took most of a 600 s boot.
       x1..x5 are preserved; x0 and x8 are clobbered, as by YIELD. */
    .macro NAP
    stp x1, x2, [sp, #-48]!
    stp x3, x4, [sp, #16]
    str x5, [sp, #32]
    ldr w0, yarm_apsw_cap_ep_n
    LI32 x1, (BUF + 0x780)
    mov x2, #16
    mov x3, #1
    mov x4, #0
    mov x5, #0
    SVC NR_RECV_TO
    ldr x5, [sp, #32]
    ldp x3, x4, [sp, #16]
    ldp x1, x2, [sp], #48
    .endm

    /* DebugLog of one of this program's strings; x2 = round/aux, x3 = aux. */
    .macro LOG msg
    adr x0, \msg
    adr x1, \msg\()_end
    sub x1, x1, x0
    SVC NR_LOG
    .endm

    .macro STR name, text
\name: .ascii "\text"
\name\()_end:
    .endm

    /* *[addr] += val, atomically (no LSE on this core). Clobbers x16, x17. */
    .macro AMOADD addr, val
1:  ldxr x16, [\addr]
    add x16, x16, \val
    stxr w17, x16, [\addr]
    cbnz w17, 1b
    .endm

    /* NR 11: a thread at `entry` on worker stack slot `\slot` (a register); x0 = its TID, or an
       error code. Clobbers x9, x10. */
    .macro SPAWNR slot, entry
    lsl x10, \slot, #4
    LI32 x0, (MBX + 0x800)
    add x0, x0, x10
    lsl x10, \slot, #12
    LI32 x1, (WSTK + 0xff0)
    add x1, x1, x10
    adr x2, \entry
    mov x3, #0
    mov x4, #0
    mov x5, #0
    SVC NR_SPAWN
    .endm

    /* This worker's slot, from its own stack, into \reg. Clobbers x17. */
    .macro MYSLOT reg
    mov \reg, sp
    LI32 x17, WSTK
    sub \reg, \reg, x17
    lsr \reg, \reg, #12
    .endm

    /* Start a bounded wait: x19 = the yield budget, x28 = the clock deadline. Clobbers x9. */
    .macro WAIT_BEGIN
    LI32 x19, WAIT_YIELDS
    LI32 x9, MBX
    ldr x28, [x9, #M_CLOCK]
    LI32 x9, WAIT_TICKS
    add x28, x28, x9
    .endm

    /* One more round of a bounded wait: back to \again unless the budget is spent or the clock
       deadline has passed; otherwise fall through. Clobbers x9. */
    .macro WAIT_NEXT again
    subs x19, x19, #1
    b.eq .Lwn_out\@
    LI32 x9, MBX
    ldr x9, [x9, #M_CLOCK]
    cmp x9, x28
    b.lo \again
.Lwn_out\@:
    .endm

    /* Bounded: wait until the kernel reports EVERY expected secondary (x30) idle. On timeout the
       run ends (`p_no_idle` names the CPUs that never idled). Clobbers x9..x13. */
    .macro AWAIT_ALL_IDLE
    WAIT_BEGIN
81: LI32 x10, MBX
    mov x11, x30
    mov x12, #0
82: cbz x11, 89f
    tbz x11, #0, 83f
    add x13, x10, x12, lsl #3
    ldr x13, [x13, #M_IDLE]
    cbz x13, 84f
83: lsr x11, x11, #1
    add x12, x12, #1
    b 82b
84: NAP
    WAIT_NEXT 81b
    b p_no_idle
89:
    .endm

    /* Bounded: wait until the u64 at MBX+\off equals register \reg; on timeout, x2 = what it
       held, x3 = \reg, and control goes to \fail. Clobbers x9, x10. */
    .macro AWAIT_EQ off, reg, fail
    WAIT_BEGIN
91: LI32 x10, MBX
    ldr x10, [x10, #\off]
    cmp x10, \reg
    b.eq 92f
    NAP
    WAIT_NEXT 91b
    mov x2, x10
    mov x3, \reg
    b \fail
92:
    .endm

    /* The same, for "at least \reg". */
    .macro AWAIT_GE off, reg, fail
    WAIT_BEGIN
93: LI32 x10, MBX
    ldr x10, [x10, #\off]
    cmp x10, \reg
    b.hs 94f
    NAP
    WAIT_NEXT 93b
    mov x2, x10
    mov x3, \reg
    b \fail
94:
    .endm

    /* NR 16, and a report if the kernel declined it. */
    .macro EXIT
    SVC NR_EXIT
    mov x3, x0
    mov x2, #0
    LOG m_exit_refused
    b park
    .endm

    /* One spinner check: \reg against x16 + \n; a mismatch counts at [sp, #-8] and keeps the
       index at [sp, #-16]. Clobbers x17 and NZCV (both already recorded or unchecked). */
    .macro CHK reg, n, idx
    add x17, x16, #\n
    cmp \reg, x17
    b.eq 1f
    ldr x17, [sp, #-8]
    add x17, x17, #1
    str x17, [sp, #-8]
    mov x17, #\idx
    str x17, [sp, #-16]
1:
    .endm

    /* The same, with the expected value already in x1 and the observed in x0. */
    .macro CHK01 idx
    cmp x0, x1
    b.eq 1f
    ldr x17, [sp, #-8]
    add x17, x17, #1
    str x17, [sp, #-8]
    mov x17, #\idx
    str x17, [sp, #-16]
1:
    .endm

/* ═══════════════════════════════════════ P ═════════════════════════════════════════════════ */
    .balign 16
    .global yarm_apsw_start
yarm_apsw_start:
    mov x2, #0
    mov x3, #0
    LOG m_p_start
    /* Every released secondary admitted: AP_READY == EXPECTED != 0. */
    WAIT_BEGIN
p_ap:
    LI32 x9, MBX
    ldr x10, [x9, #M_EXPECTED]
    cbz x10, 1f
    ldr x11, [x9, #M_AP_READY]
    cmp x10, x11
    b.eq p_ready
1:  NAP
    WAIT_NEXT p_ap
    LI32 x9, MBX
    ldr x2, [x9, #M_EXPECTED]
    ldr x3, [x9, #M_AP_READY]
    LOG m_p_no_ap
    b p_end
p_ready:
    /* x30 = the expected secondaries for the whole run; x29 = how many. */
    LI32 x9, MBX
    ldr x30, [x9, #M_EXPECTED]
    mov x10, x30
    mov x29, #0
1:  cbz x10, 2f
    and x11, x10, #1
    add x29, x29, x11
    lsr x10, x10, #1
    b 1b
2:  mov x2, x30
    mov x3, x29
    LOG m_p_ap

    /* ── 0: one deadline thread per secondary, spread by the gate ── */
    mov x20, #1
1:  cmp x20, x29
    b.gt 2f
    add x12, x20, #D_SLOT0
    SPAWNR x12, w_deadline
    cmp x0, #MIN_TID
    b.lo p_spawn_fail
    add x20, x20, #1
    b 1b
2:  LI32 x9, MBX
    mov x10, #GATE_D
    str x10, [x9, #M_GATE]
    AWAIT_EQ M_D_ARMED, x29, p_a_timeout

    /* ── A: 2N + 2 spinners ── */
    lsl x21, x29, #1
    add x21, x21, #2
    mov x20, #0
1:  cmp x20, x21
    b.ge 2f
    add x12, x20, #SPIN_SLOT0
    SPAWNR x12, w_spin
    cmp x0, #MIN_TID
    b.lo p_spawn_fail
    add x20, x20, #1
    b 1b
    /* Preemptions count only once EVERY spinner runs (see the RISC-V program). ONE deadline covers
       the whole phase (both waits below): x28 is set here and not reset. */
2:  WAIT_BEGIN
p_w:
    LI32 x9, MBX
    ldr x10, [x9, #M_W_STARTED]
    cmp x10, x21
    b.eq p_w_cover
    NAP
    WAIT_NEXT p_w
    /* Not every spinner ran by the deadline: say so, then grade the preemptions from zero. */
    LI32 x9, MBX
    ldr x2, [x9, #M_W_STARTED]
    mov x3, x21
    LOG m_p_w_timeout
    LI32 x10, (BUF + B_PRESNAP)
    mov x13, #0
1:  str xzr, [x10, x13, lsl #3]
    add x13, x13, #1
    cmp x13, #8
    b.ne 1b
    b p_pre_begin
    /* Ordinary placement balances by load, so where 2N + 2 spinners land depends on what else is
       runnable — on AArch64 the supervisor's one-tick control poll is homed on CPU 0. Until a
       spinner has STARTED on CPU 0 and on every expected secondary, as the kernel observed it
       (M_W_CPUS, written at each W_START), place one more through the same placement, up to the
       spinner slots and under the same deadline. The grader counts the top-up from P_W_COVER. */
p_w_cover:
    LI32 x9, MBX
    ldr x10, [x9, #M_W_CPUS]
    orr x11, x30, #1
    bics xzr, x11, x10
    b.eq p_w_ok
    cmp x21, #SPIN_SLOTS
    b.hs p_w_ok
    add x12, x21, #SPIN_SLOT0
    SPAWNR x12, w_spin
    cmp x0, #MIN_TID
    b.lo p_spawn_fail
    add x21, x21, #1
    b p_w
p_w_ok:
    mov x2, x21
    LI32 x9, MBX
    ldr x3, [x9, #M_W_CPUS]
    LOG m_p_w_cover
    LI32 x9, (MBX + M_PRE)
    LI32 x10, (BUF + B_PRESNAP)
    mov x13, #0
1:  ldr x11, [x9, x13, lsl #3]
    str x11, [x10, x13, lsl #3]
    add x13, x13, #1
    cmp x13, #8
    b.ne 1b
p_pre_begin:
    LI32 x19, WAIT_YIELDS
p_pre:
    LI32 x9, (MBX + M_PRE)
    LI32 x15, (BUF + B_PRESNAP)
    ldr x10, [x9]
    ldr x11, [x15]
    sub x10, x10, x11
    cmp x10, #PRE0_MIN
    b.lo 3f
    mov x12, x30
    mov x13, #0
1:  cbz x12, p_stop
    tbz x12, #0, 2f
    ldr x10, [x9, x13, lsl #3]
    ldr x11, [x15, x13, lsl #3]
    sub x10, x10, x11
    cmp x10, #PRE_MIN
    b.lo 3f
2:  lsr x12, x12, #1
    add x13, x13, #1
    b 1b
3:  NAP
    WAIT_NEXT p_pre
    /* Phase A's deadline: x2 = CPUs (0 and the expected ones) below their minimum since the
       snapshot; x3 = those with no timer preemption at all. Then stop the spinners, let them
       report, and end. */
    LI32 x9, (MBX + M_PRE)
    LI32 x15, (BUF + B_PRESNAP)
    orr x12, x30, #1
    mov x13, #0
    mov x2, #0
    mov x3, #0
1:  cbz x12, 4f
    tbz x12, #0, 2f
    mov x14, #1
    lsl x14, x14, x13
    ldr x10, [x9, x13, lsl #3]
    cbnz x10, 5f
    orr x3, x3, x14
5:  ldr x11, [x15, x13, lsl #3]
    sub x10, x10, x11
    mov x11, #PRE_MIN
    cbnz x13, 6f
    mov x11, #PRE0_MIN
6:  cmp x10, x11
    b.hs 2f
    orr x2, x2, x14
2:  lsr x12, x12, #1
    add x13, x13, #1
    b 1b
4:  LOG m_p_pre_timeout
    LI32 x9, MBX
    mov x10, #1
    str x10, [x9, #M_STOP]
    ldr x21, [x9, #M_W_STARTED]
    /* At least: a spinner that never got its CPU before the stop can still start, and finish,
       after it. */
    AWAIT_GE M_SPIN_DONE, x21, p_end
    b p_end
p_stop:
    LI32 x9, MBX
    mov x10, #1
    str x10, [x9, #M_STOP]
    AWAIT_EQ M_SPIN_DONE, x21, p_a_timeout
    AWAIT_EQ M_D_DONE, x29, p_a_timeout
    LI32 x9, MBX
    ldr x2, [x9, #M_SPIN_DONE]
    ldr x3, [x9, #M_SPIN_BAD]
    LOG m_p_a_done

    /* ── B: one sleeper per secondary, woken on its idle CPU ── */
    AWAIT_ALL_IDLE
    mov x20, #1
1:  cmp x20, x29
    b.gt 2f
    add x12, x20, #SLEEP_SLOT0
    SPAWNR x12, w_sleep
    cmp x0, #MIN_TID
    b.lo p_spawn_fail
    add x20, x20, #1
    b 1b
2:  LI32 x9, MBX
    mov x10, #GATE_S
    str x10, [x9, #M_GATE]
    mov x22, #1
p_b_round:
    mov x20, #1
p_b_one:
    /* sleeper x20 announced round x22? */
    lsl x24, x20, #3
    add x24, x24, #M_S_WAITING
    WAIT_BEGIN
1:  LI32 x9, MBX
    ldr x10, [x9, x24]
    cmp x10, x22
    b.eq 2f
    NAP
    WAIT_NEXT 1b
    lsl x2, x20, #8
    orr x2, x2, x22
    mov x3, #0
    LOG m_p_b_timeout
    b p_end
2:  AWAIT_ALL_IDLE
    /* F[x20] = round; wake it. */
    LI32 x25, (MBX + M_F)
    add x25, x25, x20, lsl #2
    str w22, [x25]
    lsl x2, x20, #8
    orr x2, x2, x22
    mov x3, #0
    LOG m_p_wake
    mov x0, x25
    mov x1, #1
    SVC NR_FUTEX_WAKE
    mov x23, x0
    lsl x24, x20, #3
    add x24, x24, #M_S_ACK
    WAIT_BEGIN
3:  LI32 x9, MBX
    ldr x10, [x9, x24]
    cmp x10, x22
    b.eq 4f
    NAP
    WAIT_NEXT 3b
    lsl x2, x20, #8
    orr x2, x2, x22
    mov x3, #0
    LOG m_p_no_ack
    b p_end
4:  lsl x2, x20, #8
    orr x2, x2, x22
    mov x3, x23
    LOG m_p_acked
    add x20, x20, #1
    cmp x20, x29
    b.le p_b_one
    add x22, x22, #1
    cmp x22, #(WAKE_ROUNDS + 1)
    b.ne p_b_round

    /* ── C: ping-pong with each IPC thread in turn ── */
p_c:
    AWAIT_ALL_IDLE
    LI32 x9, MBX
    str xzr, [x9, #M_T_TURN]
    mov x20, #1
1:  cmp x20, x29
    b.gt 2f
    add x12, x20, #IPC_SLOT0
    SPAWNR x12, w_ipc
    cmp x0, #MIN_TID
    b.lo p_spawn_fail
    add x20, x20, #1
    b 1b
2:  mov x20, #1
p_c_thread:
    /* The previous thread has read its last pong before the next one starts. */
    sub x21, x20, #1
    AWAIT_EQ M_T_DONE, x21, p_c_timeout
    LI32 x9, MBX
    str x20, [x9, #M_T_TURN]
    mov x22, #1
p_c_round:
    mov x25, #0
p_c_recv:
    /* recv-v2 success: x0 == 0, or the kernel wrote the metadata status; so the status is
       poisoned first. WouldBlock (7) with the status untouched is the runtime's "nothing yet" and
       is received again (bounded). */
    add x25, x25, #1
    LI32 x9, BUF
    mov x10, #-1
    str x10, [x9, #0x40]
    ldr w0, yarm_apsw_cap_ep_p
    LI32 x1, BUF
    mov x2, #32
    LI32 x3, (BUF + 0x40)
    mov x4, #0x28
    mov x5, #0
    SVC NR_RECV
    /* The received frame: opcode +0, round +2, thread +4. */
    mov x23, x0
    LI32 x9, BUF
    ldr x26, [x9, #0x40]
    cmp x23, #7
    b.ne 5f
    cmn x26, #1
    b.ne 5f
    cmp x25, #64
    b.hs 5f
    YIELD
    b p_c_recv
5:  LI32 x9, BUF
    ldrh w10, [x9, #2]
    ldrh w11, [x9, #0]
    ldrh w13, [x9, #4]
    mov x24, #0
    cbz x23, 0f
    cmn x26, #1
    b.eq 1f
0:  mov x12, #0x5050
    cmp x11, x12
    b.ne 1f
    cmp x10, x22
    b.ne 1f
    cmp x13, x20
    b.ne 1f
    mov x24, #1
1:  lsl x2, x20, #8
    orr x2, x2, x22
    mov x3, x24
    LOG m_p_ping
    LI32 x9, (BUF + 0x200)
    mov x10, #0x4F50
    strh w10, [x9, #0]
    strh w22, [x9, #2]
    strh w20, [x9, #4]
    mov x10, #0x4B4F
    strh w10, [x9, #6]
    mov x25, #0
3:  add x25, x25, #1
    ldr w0, yarm_apsw_cap_ep_t
    LI32 x1, (BUF + 0x200)
    mov x2, #8
    mov x3, #0
    mov x4, #0
    mov x5, #-1
    SVC NR_SEND
    cbz x0, 4f
    cmp x25, #64
    b.hs 4f
    YIELD
    b 3b
4:  add x22, x22, #1
    cmp x22, #(IPC_ROUNDS + 1)
    b.ne p_c_round
    add x20, x20, #1
    cmp x20, x29
    b.le p_c_thread
    AWAIT_EQ M_T_DONE, x29, p_c_timeout

    /* ── D: per round, fork one child per secondary; reap each ── */
    mov x22, #1
p_d_round:
    AWAIT_ALL_IDLE
    mov x20, #1
p_d_fork:
    cmp x20, x29
    b.gt p_d_reap
    mov x0, #0
    mov x1, #0
    mov x2, #0
    mov x3, #0
    SVC NR_FORK
    cbz x0, c_child
    cmp x0, #MIN_TID
    b.lo p_fork_fail
    LI32 x9, (BUF + B_CHILD)
    str x0, [x9, x20, lsl #3]
    mov x2, x0
    lsl x3, x22, #8
    orr x3, x3, x20
    LOG m_p_forked
    add x20, x20, #1
    b p_d_fork
p_d_reap:
    mov x20, #1
p_d_reap_one:
    LI32 x9, (BUF + B_CHILD)
    ldr x24, [x9, x20, lsl #3]
    WAIT_BEGIN
    LI32 x19, (WAIT_YIELDS / 8)
1:  mov x2, x24
    lsl x3, x22, #8
    orr x3, x3, x20
    LOG m_p_reap
    LI32 x9, MBX
    ldr x10, [x9, #M_REAPED]
    cmp x10, x24
    b.eq 2f
    NAP
    WAIT_NEXT 1b
    mov x2, x24
    lsl x3, x22, #8
    orr x3, x3, x20
    LOG m_p_reap_timeout
    b p_end
2:  add x20, x20, #1
    cmp x20, x29
    b.le p_d_reap_one
    add x22, x22, #1
    cmp x22, #(FORK_ROUNDS + 1)
    b.ne p_d_round
    b p_end
/* A bounded wait in phase A or C timed out (x2 = what the word held, x3 = what P waited for), or
   an expected CPU never idled when P needed it (x2 = those CPUs): say so and end the run, so the
   dump is complete within the budget instead of every later wait expiring in turn. */
p_a_timeout:
    LOG m_p_a_timeout
    b p_end
p_c_timeout:
    LOG m_p_c_timeout
    b p_end
p_no_idle:
    LI32 x10, MBX
    mov x11, x30
    mov x12, #0
    mov x2, #0
1:  cbz x11, 3f
    tbz x11, #0, 2f
    add x13, x10, x12, lsl #3
    ldr x13, [x13, #M_IDLE]
    cbnz x13, 2f
    mov x14, #1
    lsl x14, x14, x12
    orr x2, x2, x14
2:  lsr x11, x11, #1
    add x12, x12, #1
    b 1b
3:  mov x3, #0
    LOG m_p_no_idle
p_end:
    mov x2, #0
    mov x3, #0
    LOG m_p_end
    b park
p_spawn_fail:
    mov x3, x0
    mov x2, #0
    LOG m_p_spawn_fail
    b p_end
p_fork_fail:
    mov x2, x0
    mov x3, #0
    LOG m_p_fork_fail
    b p_end
park:
    YIELD
    b park

/* The churn child (x20 = its index, x22 = the round, both inherited from the fork): spins so the
   next fork sees its CPU busy, takes its copy-on-write stack page, reports and exits. */
c_child:
    LI32 x9, CHILD_SPIN
1:  subs x9, x9, #1
    b.ne 1b
    str x20, [sp, #-64]
    ldr x10, [sp, #-64]
    lsl x2, x22, #8
    orr x2, x2, x20
    mov x3, x10
    LOG m_c_run
    EXIT

/* ═══════════════════════════════════ workers ══════════════════════════════════════════════ */
/* A deadline receive on an endpoint nobody sends to, after the spawn gate. */
w_deadline:
    LI32 x9, MBX
1:  ldr x10, [x9, #M_GATE]
    cmp x10, #GATE_D
    b.ne 1b
    mov x2, #0
    mov x3, #0
    LOG m_d_before
    LI32 x9, (MBX + M_D_ARMED)
    mov x10, #1
    AMOADD x9, x10
    ldr w0, yarm_apsw_cap_ep_d
    LI32 x1, (BUF + 0x300)
    mov x2, #16
    mov x3, #DEADLINE_TICKS
    mov x4, #0
    mov x5, #0
    SVC NR_RECV_TO
    mov x20, x0
    mov x2, #DEADLINE_TICKS
    mov x3, x20
    LOG m_d_after
    LI32 x9, (MBX + M_D_DONE)
    mov x10, #1
    AMOADD x9, x10
    EXIT

/* A spinner: its whole user context patterned, no kernel entry until P stops it, then the check. */
w_spin:
    mov x2, #0
    mov x3, #0
    LOG m_w_start
    LI32 x9, (MBX + M_W_STARTED)
    mov x10, #1
    AMOADD x9, x10
    /* x15 = slot; x16 = base = PAT_HI << 48 | slot << 32. */
    MYSLOT x15
    movz x16, #PAT_HI, lsl #48
    orr x16, x16, x15, lsl #32
    /* q0..q31: low half base + 0x100 + N, high half base + 0x200 + N. */
    .irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    add x14, x16, #(0x100 + \n)
    fmov d\n, x14
    add x14, x16, #(0x200 + \n)
    mov v\n\().d[1], x14
    .endr
    /* FPCR: RMode = slot & 3, FZ, DN = slot bit 2. FPSR: IOC..IXC = slot & 0x1f, IDC, QC. */
    and x14, x15, #3
    lsl x14, x14, #22
    orr x14, x14, #(1 << 24)
    ubfx x17, x15, #2, #1
    orr x14, x14, x17, lsl #25
    msr fpcr, x14
    and x14, x15, #0x1f
    orr x14, x14, #0x80
    orr x14, x14, #(1 << 27)
    msr fpsr, x14
    /* TPIDR_EL0: base + 0x300. */
    add x14, x16, #0x300
    msr tpidr_el0, x14
    /* NZCV: (slot & 0xf) << 28. Nothing below may set the flags until the check saves them. */
    and x14, x15, #0xf
    lsl x14, x14, #28
    msr nzcv, x14
    /* The GPRs last: base + N. */
    add x0, x16, #0
    add x1, x16, #1
    add x2, x16, #2
    add x3, x16, #3
    add x4, x16, #4
    add x5, x16, #5
    add x6, x16, #6
    add x7, x16, #7
    add x8, x16, #8
    add x9, x16, #9
    add x10, x16, #10
    add x11, x16, #11
    add x12, x16, #12
    add x13, x16, #13
    add x19, x16, #19
    add x20, x16, #20
    add x21, x16, #21
    add x22, x16, #22
    add x23, x16, #23
    add x24, x16, #24
    add x25, x16, #25
    add x26, x16, #26
    add x27, x16, #27
    add x28, x16, #28
    add x29, x16, #29
    add x30, x16, #30
    add x14, x16, #14
    add x15, x16, #15
    /* x16 = &STOP; spin on it (ldr / cbz set no flags). */
    movz x16, #((MBX + M_STOP) & 0xffff)
    movk x16, #(((MBX + M_STOP) >> 16) & 0xffff), lsl #16
w_spin_loop:
    ldr x17, [x16]
    cbz x17, w_spin_loop
    /* NZCV first, before any compare can change it. */
    mrs x17, nzcv
    str x17, [sp, #-32]
    str xzr, [sp, #-8]
    str xzr, [sp, #-16]
    /* x16 = base again, from sp alone (slot = (sp - WSTK) >> 12). */
    mov x16, sp
    movz x17, #(WSTK & 0xffff)
    movk x17, #((WSTK >> 16) & 0xffff), lsl #16
    sub x16, x16, x17
    lsr x16, x16, #12
    str x16, [sp, #-24]
    lsl x16, x16, #32
    movz x17, #PAT_HI, lsl #48
    orr x16, x16, x17
    CHK x0, 0, 0
    CHK x1, 1, 1
    CHK x2, 2, 2
    CHK x3, 3, 3
    CHK x4, 4, 4
    CHK x5, 5, 5
    CHK x6, 6, 6
    CHK x7, 7, 7
    CHK x8, 8, 8
    CHK x9, 9, 9
    CHK x10, 10, 10
    CHK x11, 11, 11
    CHK x12, 12, 12
    CHK x13, 13, 13
    CHK x14, 14, 14
    CHK x15, 15, 15
    CHK x19, 19, 19
    CHK x20, 20, 20
    CHK x21, 21, 21
    CHK x22, 22, 22
    CHK x23, 23, 23
    CHK x24, 24, 24
    CHK x25, 25, 25
    CHK x26, 26, 26
    CHK x27, 27, 27
    CHK x28, 28, 28
    CHK x29, 29, 29
    CHK x30, 30, 30
    /* Every GPR is checked: x0..x5 are free temporaries now. x2 = slot. */
    ldr x2, [sp, #-24]
    /* SP: exactly this slot's top. */
    mov x0, sp
    movz x1, #((WSTK + 0xff0) & 0xffff)
    movk x1, #(((WSTK + 0xff0) >> 16) & 0xffff), lsl #16
    add x1, x1, x2, lsl #12
    CHK01 31
    /* NZCV, as saved before the first compare. */
    ldr x0, [sp, #-32]
    and x1, x2, #0xf
    lsl x1, x1, #28
    CHK01 32
    /* FPCR. */
    mrs x0, fpcr
    and x1, x2, #3
    lsl x1, x1, #22
    orr x1, x1, #(1 << 24)
    ubfx x3, x2, #2, #1
    orr x1, x1, x3, lsl #25
    CHK01 33
    /* FPSR. */
    mrs x0, fpsr
    and x1, x2, #0x1f
    orr x1, x1, #0x80
    orr x1, x1, #(1 << 27)
    CHK01 34
    /* TPIDR_EL0. */
    mrs x0, tpidr_el0
    add x1, x16, #0x300
    CHK01 35
    /* q0..q31, both halves. */
    .irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    fmov x0, d\n
    add x1, x16, #(0x100 + \n)
    CHK01 (64 + \n)
    mov x0, v\n\().d[1]
    add x1, x16, #(0x200 + \n)
    CHK01 (96 + \n)
    .endr
    ldr x2, [sp, #-8]
    ldr x3, [sp, #-16]
    mov x20, x2
    mov x21, x3
    LI32 x9, (MBX + M_SPIN_BAD)
    AMOADD x9, x20
    LI32 x9, (MBX + M_SPIN_DONE)
    mov x10, #1
    AMOADD x9, x10
    mov x2, x20
    mov x3, x21
    LOG m_w_done
    EXIT

/* Sleeper k (= slot - SLEEP_SLOT0): after the spawn gate, announce round r, wait on F[k] until it
   holds r, acknowledge. */
w_sleep:
    MYSLOT x26
    sub x26, x26, #SLEEP_SLOT0
    LI32 x9, MBX
1:  ldr x10, [x9, #M_GATE]
    cmp x10, #GATE_S
    b.ne 1b
    mov x2, x26
    mov x3, #0
    LOG m_s_start
    LI32 x27, MBX
    add x27, x27, x26, lsl #3
    LI32 x28, (MBX + M_F)
    add x28, x28, x26, lsl #2
    mov x20, #1
1:  str x20, [x27, #M_S_WAITING]
2:  ldr w10, [x28]
    cmp x10, x20
    b.eq 3f
    mov x0, x28
    sub x1, x20, #1
    mov x2, x10
    SVC NR_FUTEX_WAIT
    b 2b
3:  str x20, [x27, #M_S_ACK]
    lsl x2, x26, #8
    orr x2, x2, x20
    mov x3, x10
    LOG m_s_woke
    add x20, x20, #1
    cmp x20, #(WAKE_ROUNDS + 1)
    b.ne 1b
    EXIT

/* IPC thread k (= slot - IPC_SLOT0): spins until its turn, then pings P and receives its pong. */
w_ipc:
    MYSLOT x26
    sub x26, x26, #IPC_SLOT0
    /* x28 = this thread's own buffers (send +0x00, receive +0x20, metadata +0x40). */
    LI32 x28, (BUF + B_IPC)
    add x28, x28, x26, lsl #7
    LI32 x9, MBX
1:  ldr x10, [x9, #M_T_TURN]
    cmp x10, x26
    b.ne 1b
    mov x2, x26
    mov x3, #0
    LOG m_t_start
    mov x20, #1
t_round:
    mov x10, #0x5050
    strh w10, [x28, #0]
    strh w20, [x28, #2]
    strh w26, [x28, #4]
    mov x10, #0x4749
    strh w10, [x28, #6]
    mov x25, #0
1:  add x25, x25, #1
    ldr w0, yarm_apsw_cap_ep_p
    mov x1, x28
    mov x2, #8
    mov x3, #0
    mov x4, #0
    mov x5, #-1
    SVC NR_SEND
    cbz x0, 2f
    cmp x25, #64
    b.hs 2f
    YIELD
    b 1b
2:  mov x25, #0
t_recv:
    add x25, x25, #1
    mov x10, #-1
    str x10, [x28, #0x40]
    ldr w0, yarm_apsw_cap_ep_t
    add x1, x28, #0x20
    mov x2, #32
    add x3, x28, #0x40
    mov x4, #0x28
    mov x5, #0
    SVC NR_RECV
    mov x23, x0
    ldr x24, [x28, #0x40]
    cmp x23, #7
    b.ne 5f
    cmn x24, #1
    b.ne 5f
    cmp x25, #64
    b.hs 5f
    YIELD
    b t_recv
5:  ldrh w10, [x28, #0x22]
    ldrh w11, [x28, #0x20]
    ldrh w13, [x28, #0x24]
    mov x27, #0
    cbz x23, 4f
    cmn x24, #1
    b.eq 3f
4:  mov x12, #0x4F50
    cmp x11, x12
    b.ne 3f
    cmp x10, x20
    b.ne 3f
    cmp x13, x26
    b.ne 3f
    mov x27, #1
3:  lsl x2, x26, #8
    orr x2, x2, x20
    mov x3, x27
    LOG m_t_pong
    add x20, x20, #1
    cmp x20, #(IPC_ROUNDS + 1)
    b.ne t_round
    LI32 x9, (MBX + M_T_DONE)
    mov x10, #1
    AMOADD x9, x10
    EXIT

    .balign 8
    .global yarm_apsw_cap_ep_p
yarm_apsw_cap_ep_p: .word 0x61616161
    .global yarm_apsw_cap_ep_t
yarm_apsw_cap_ep_t: .word 0x62626262
    .global yarm_apsw_cap_ep_d
yarm_apsw_cap_ep_d: .word 0x63636363
    .global yarm_apsw_cap_ep_n
yarm_apsw_cap_ep_n: .word 0x64646464

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
