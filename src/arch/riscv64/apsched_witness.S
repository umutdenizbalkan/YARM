/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * AP-SCHED2 — the RISC-V production SMP-scheduling witness user program, for every admitted CPU.
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
 * User VAs:
 *   0x2001_0000  P's stack (top 0x2001_0ff0)
 *   0x2002_0000  worker stacks, one page per slot (slot N top = 0x2002_0ff0 + N * 0x1000)
 *   0x2007_0000  MBX: shared words (below); the kernel publishes only its observation words
 *   0x2008_0000  BUF: IPC buffers and receive metadata
 *
 * MBX (u64 unless noted):
 *   +0x000 AP_READY    kernel: bitmap of CPUs that took their production admission
 *   +0x008 STOP        P: spinners stop
 *   +0x010 SPIN_DONE   spinners: amoadd 1 each
 *   +0x018 SPIN_BAD    spinners: amoadd their mismatch count
 *   +0x020 EXPECTED    kernel: bitmap of the secondaries the boot hart released
 *   +0x028 GATE        P: the phase whose workers may leave their spawn gate
 *   +0x030 D_ARMED     deadline threads: amoadd 1 each, just before their receive
 *   +0x038 REAPED      kernel: the last churn child the reaper reaped
 *   +0x040 D_DONE      deadline threads: amoadd 1 each
 *   +0x048 T_TURN      P: the IPC thread whose rounds run now
 *   +0x050 T_DONE      IPC threads: amoadd 1 each
 *   +0x058 W_STARTED   spinners: amoadd 1 each, once running
 *   +0x060 CLOCK       kernel: CPU 0's timer interrupts (the global tick's source), at each one
 *   +0x100 PRE[c]      kernel: timer preemptions of this process's workers on CPU c (8 CPUs)
 *   +0x140 IDLE[c]     kernel: 1 from CPU c's idle arrival until it next leaves idle
 *   +0x180 F[k] (u32)  sleeper k's futex word
 *   +0x1a0 S_WAITING[k]  +0x1e0 S_ACK[k]
 *
 * P never stores to MBX after its first fork: the store would copy-on-write split P's mailbox
 * onto a new frame while the kernel keeps publishing into the old one (now the child's). The
 * churn child table therefore lives in BUF: BUF+0x800 CHILD[k]. IPC thread k's own buffers are at
 * BUF+0x900+k*0x80. P's snapshot of PRE[c] once every spinner runs: BUF+0xE00.
 *
 * Phases (P drives; the kernel records every step with a causal stamp):
 *   0  one deadline thread per secondary (NR 5 on an endpoint nobody sends to).
 *   A  2N non-yielding spinners (N = every admitted CPU): each loads a per-thread pattern into
 *      x1, x3..x29, spins with no kernel entry until P stops it, then checks every patterned
 *      register. P stops them once the kernel has seen enough TIMER preemptions on EVERY CPU.
 *   B  one sleeper per secondary; each round, once a sleeper has announced the round AND every
 *      secondary is idle, P stores its word and wakes it (NR 10): the wake must reach that idle
 *      CPU by IPI.
 *   C  one IPC thread per secondary; in turn, each pings P over E_P (NR 1) and receives P's pong
 *      on E_T (NR 2): every round is a remote wake each way.
 *   D  FORK_ROUNDS rounds of one forked child per secondary, each running, touching its
 *      copy-on-write stack and exiting wherever placement put it; the witness reaps each through
 *      the production NR 31 transaction.
 *
 * Linker relaxation is off; data is reached through absolute VAs built with `li`, strings and
 * entry points through `lla`, capabilities through literal words the kernel patches.
 */
    .pushsection .text.yarm_apsw_witness, "ax", @progbits
    /* KERNEL-NOFP1: everything in this section is a USER-MODE program the kernel copies
     * into a task's address space and never executes; the final-image check accounts
     * for it as one USER_PROGRAM region (scripts/check-kernel-nofp.py). */
.Lyarm_nofp_user_apsched_witness_begin:
    .option push
    .option norelax

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
    .set M_PRE, 0x100
    .set M_IDLE, 0x140
    .set M_F, 0x180
    .set M_S_WAITING, 0x1a0
    .set M_S_ACK, 0x1e0
    .set B_CHILD, 0x800
    .set B_IPC, 0x900
    .set B_PRESNAP, 0xE00

    /* Worker-stack slot bases: deadline 1..7, spinners 8..23, sleepers 25..31, IPC 33..39. */
    .set D_SLOT0, 0
    .set SPIN_SLOT0, 8
    .set SLEEP_SLOT0, 24
    .set IPC_SLOT0, 32

    .set GATE_D, 1
    .set GATE_S, 2

    .set PRE0_MIN, 3
    .set PRE_MIN, 4
    .set DEADLINE_TICKS, 40
    .set WAKE_ROUNDS, 3
    .set IPC_ROUNDS, 4
    .set FORK_ROUNDS, 4
    .set CHILD_SPIN, 3000000

    /* Bounded waits: a yield count AND a deadline on the global clock, whichever ends first.
       The yield count alone could outlast any boot budget: on a contended CPU 0 one yield can
       cost a whole quantum. The deadline is read from CLOCK, which CPU 0's timer drives, so it
       still expires when a SECONDARY's timer is the thing that is broken. 6000 ticks (60 s of
       guest time at 100 Hz) is about five times the longest phase of a healthy 8-CPU run
       (phase A, ~1200 ticks). */
    .set WAIT_YIELDS, 400000
    .set WAIT_TICKS, 6000
    /* The spinners' pattern base; slot N adds N << 32. */
    .set PAT_BASE, 0xA5A5000000000000

    .macro YIELD
    li a7, NR_YIELD
    ecall
    .endm

    /* DebugLog of one of this program's strings; a2 = round/aux, a3 = aux. */
    .macro LOG msg
    lla a0, \msg
    lla a1, \msg\()_end
    sub a1, a1, a0
    li a7, NR_LOG
    ecall
    .endm

    /* Start a bounded wait: s0 = the yield budget, s9 = the clock deadline. Uses t0. */
    .macro WAIT_BEGIN
    li s0, WAIT_YIELDS
    li t0, MBX
    ld s9, M_CLOCK(t0)
    li t0, WAIT_TICKS
    add s9, s9, t0
    .endm

    /* One more round of a bounded wait: back to \again unless the yield budget is spent or the
       clock deadline (s9) has passed; otherwise fall through. Uses s0, t0. */
    .macro WAIT_NEXT again
    addi s0, s0, -1
    beqz s0, .Lwn_out\@
    li t0, MBX
    ld t0, M_CLOCK(t0)
    bltu t0, s9, \again
.Lwn_out\@:
    .endm

    .macro STR name, text
\name: .ascii "\text"
\name\()_end:
    .endm

    /* NR 11: a thread at `entry` on worker stack slot `\slot` (a register); a0 = its TID, 0 on
       refusal. Clobbers t0, t1. */
    .macro SPAWNR slot, entry
    slli t1, \slot, 4
    li a0, (MBX + 0x800)
    add a0, a0, t1
    slli t1, \slot, 12
    li a1, (WSTK + 0xff0)
    add a1, a1, t1
    lla a2, \entry
    li a3, 0
    li a4, 0
    li a5, 0
    li a7, NR_SPAWN
    ecall
    .endm

    /* This worker's slot, from its own stack, into \reg. */
    .macro MYSLOT reg
    li \reg, WSTK
    sub \reg, sp, \reg
    srli \reg, \reg, 12
    .endm

    /* Bounded: wait until the kernel reports EVERY expected secondary (s11) idle, so the next
       placement sees them empty and a wake has an idle target. On timeout carry on — the grader
       then sees what happened. Uses s0, t0..t6. */
    .macro AWAIT_ALL_IDLE
    WAIT_BEGIN
81: li t0, MBX
    mv t1, s11
    li t4, 0
82: beqz t1, 89f
    andi t2, t1, 1
    beqz t2, 83f
    slli t5, t4, 3
    add t5, t5, t0
    ld t6, M_IDLE(t5)
    beqz t6, 84f
83: srli t1, t1, 1
    addi t4, t4, 1
    j 82b
84: YIELD
    WAIT_NEXT 81b
89:
    .endm

    /* Bounded: wait until the u64 at MBX+\off equals register \reg. Uses s0, t0, t1. */
    .macro AWAIT_EQ off, reg
    WAIT_BEGIN
91: li t0, MBX
    ld t1, \off(t0)
    beq t1, \reg, 92f
    YIELD
    WAIT_NEXT 91b
92:
    .endm

    /* NR 16, and a report if the kernel declined it. */
    .macro EXIT
    li a7, NR_EXIT
    ecall
    mv a3, a0
    li a2, 0
    LOG m_exit_refused
    j park
    .endm

    /* One spinner register check: xN against base (t5) + N; mismatches counted at -8(sp), the
       last bad N kept at -16(sp). Uses only t6 beyond the checked register. */
    .macro CHK reg, n
    addi t6, t5, \n
    beq \reg, t6, 1f
    ld t6, -8(sp)
    addi t6, t6, 1
    sd t6, -8(sp)
    li t6, \n
    sd t6, -16(sp)
1:
    .endm

/* ═══════════════════════════════════════ P ═════════════════════════════════════════════════ */
    .balign 16
    .global yarm_apsw_start
yarm_apsw_start:
    li a2, 0
    li a3, 0
    LOG m_p_start
    /* Every released secondary admitted: AP_READY == EXPECTED != 0. */
    WAIT_BEGIN
p_ap:
    li t0, MBX
    ld t1, M_EXPECTED(t0)
    beqz t1, 1f
    ld t2, M_AP_READY(t0)
    beq t1, t2, p_ready
1:  YIELD
    WAIT_NEXT p_ap
    li t0, MBX
    ld a2, M_EXPECTED(t0)
    ld a3, M_AP_READY(t0)
    LOG m_p_no_ap
    j p_end
p_ready:
    /* s11 = the expected secondaries for the whole run; s10 = how many. */
    li t0, MBX
    ld s11, M_EXPECTED(t0)
    mv t1, s11
    li s10, 0
1:  beqz t1, 2f
    andi t2, t1, 1
    add s10, s10, t2
    srli t1, t1, 1
    j 1b
2:  mv a2, s11
    mv a3, s10
    LOG m_p_ap

    /* ── 0: one deadline thread per secondary, spread by the gate ── */
    li s1, 1
1:  bgt s1, s10, 2f
    addi t3, s1, D_SLOT0
    SPAWNR t3, w_deadline
    beqz a0, p_spawn_fail
    addi s1, s1, 1
    j 1b
2:  li t0, MBX
    li t1, GATE_D
    sd t1, M_GATE(t0)
    AWAIT_EQ M_D_ARMED, s10

    /* ── A: 2N spinners ── */
    slli s2, s10, 1
    addi s2, s2, 2
    li s1, 0
1:  bge s1, s2, 2f
    addi t3, s1, SPIN_SLOT0
    SPAWNR t3, w_spin
    beqz a0, p_spawn_fail
    addi s1, s1, 1
    j 1b
    /* Preemptions count only once EVERY spinner runs: a spinner spawned late onto a CPU whose
       total was already met would otherwise start after the stop and never be preempted. From
       here, round-robin gives each of a CPU's spinners a preemption within that CPU's next
       PRE_MIN (at most PRE_MIN spinners share a CPU under balanced placement).
       ONE deadline covers the whole phase (both waits below): s9 is set here and not reset. */
2:  WAIT_BEGIN
p_w:
    li t0, MBX
    ld t1, M_W_STARTED(t0)
    beq t1, s2, p_w_ok
    YIELD
    WAIT_NEXT p_w
    /* Not every spinner ran by the deadline: say so, then grade the preemptions from zero (the
       snapshot below would be meaningless), against the deadline that has already passed. */
    li t0, MBX
    ld a2, M_W_STARTED(t0)
    mv a3, s2
    LOG m_p_w_timeout
    li t1, (BUF + B_PRESNAP)
    li t4, 0
1:  slli t6, t4, 3
    add t5, t6, t1
    sd zero, 0(t5)
    addi t4, t4, 1
    li t2, 8
    bne t4, t2, 1b
    j p_pre_begin
p_w_ok:
    li t0, MBX
    li t1, (BUF + B_PRESNAP)
    li t4, 0
1:  slli t6, t4, 3
    add t5, t6, t0
    ld t2, M_PRE(t5)
    add t5, t6, t1
    sd t2, 0(t5)
    addi t4, t4, 1
    li t2, 8
    bne t4, t2, 1b
p_pre_begin:
    li s0, WAIT_YIELDS
p_pre:
    li t0, MBX
    li a6, (BUF + B_PRESNAP)
    ld t1, M_PRE(t0)
    ld t2, 0(a6)
    sub t1, t1, t2
    li t2, PRE0_MIN
    bltu t1, t2, 3f
    mv t3, s11
    li t4, 0
1:  beqz t3, p_stop
    andi t5, t3, 1
    beqz t5, 2f
    slli t6, t4, 3
    add t5, t6, a6
    ld t2, 0(t5)
    add t6, t6, t0
    ld t1, M_PRE(t6)
    sub t1, t1, t2
    li t2, PRE_MIN
    bltu t1, t2, 3f
2:  srli t3, t3, 1
    addi t4, t4, 1
    j 1b
3:  YIELD
    WAIT_NEXT p_pre
    /* Phase A's deadline: name the CPUs that fell short. a2 = CPUs (0 and the expected ones)
       below their minimum since the snapshot; a3 = those with no timer preemption at all. Then
       stop the spinners, let them report, and end: nothing after this phase is meaningful. */
    li t0, MBX
    li a6, (BUF + B_PRESNAP)
    ori t3, s11, 1
    li t4, 0
    li a2, 0
    li a3, 0
1:  beqz t3, 4f
    andi t5, t3, 1
    beqz t5, 2f
    slli t6, t4, 3
    add t5, t6, t0
    ld t1, M_PRE(t5)
    li t5, 1
    sll t5, t5, t4
    bnez t1, 5f
    or a3, a3, t5
5:  add t6, t6, a6
    ld t2, 0(t6)
    sub t1, t1, t2
    li t2, PRE_MIN
    bnez t4, 6f
    li t2, PRE0_MIN
6:  bgeu t1, t2, 2f
    or a2, a2, t5
2:  srli t3, t3, 1
    addi t4, t4, 1
    j 1b
4:  LOG m_p_pre_timeout
    li t0, MBX
    li t1, 1
    sd t1, M_STOP(t0)
    li t0, MBX
    ld s2, M_W_STARTED(t0)
    AWAIT_EQ M_SPIN_DONE, s2
    j p_end
p_stop:
    li t0, MBX
    li t1, 1
    sd t1, M_STOP(t0)
    AWAIT_EQ M_SPIN_DONE, s2
    AWAIT_EQ M_D_DONE, s10
    li t0, MBX
    ld a2, M_SPIN_DONE(t0)
    ld a3, M_SPIN_BAD(t0)
    LOG m_p_a_done

    /* ── B: one sleeper per secondary, woken on its idle CPU ── */
    AWAIT_ALL_IDLE
    li s1, 1
1:  bgt s1, s10, 2f
    addi t3, s1, SLEEP_SLOT0
    SPAWNR t3, w_sleep
    beqz a0, p_spawn_fail
    addi s1, s1, 1
    j 1b
2:  li t0, MBX
    li t1, GATE_S
    sd t1, M_GATE(t0)
    li s3, 1
p_b_round:
    li s1, 1
p_b_one:
    /* sleeper s1 announced round s3? */
    slli s5, s1, 3
    addi s5, s5, M_S_WAITING
    WAIT_BEGIN
1:  li t0, MBX
    add t1, t0, s5
    ld t1, 0(t1)
    beq t1, s3, 2f
    YIELD
    WAIT_NEXT 1b
    slli a2, s1, 8
    or a2, a2, s3
    li a3, 0
    LOG m_p_b_timeout
    j p_c
2:  AWAIT_ALL_IDLE
    /* F[s1] = round; wake it. */
    slli s6, s1, 2
    li t0, (MBX + M_F)
    add s6, s6, t0
    sw s3, 0(s6)
    slli a2, s1, 8
    or a2, a2, s3
    li a3, 0
    LOG m_p_wake
    mv a0, s6
    li a1, 1
    li a7, NR_FUTEX_WAKE
    ecall
    mv s4, a0
    slli s5, s1, 3
    addi s5, s5, M_S_ACK
    WAIT_BEGIN
3:  li t0, MBX
    add t1, t0, s5
    ld t1, 0(t1)
    beq t1, s3, 4f
    YIELD
    WAIT_NEXT 3b
    slli a2, s1, 8
    or a2, a2, s3
    li a3, 0
    LOG m_p_no_ack
    j p_c
4:  slli a2, s1, 8
    or a2, a2, s3
    mv a3, s4
    LOG m_p_acked
    addi s1, s1, 1
    ble s1, s10, p_b_one
    addi s3, s3, 1
    li t2, (WAKE_ROUNDS + 1)
    bne s3, t2, p_b_round

    /* ── C: ping-pong with each IPC thread in turn ── */
p_c:
    AWAIT_ALL_IDLE
    li t0, MBX
    sd zero, M_T_TURN(t0)
    li s1, 1
1:  bgt s1, s10, 2f
    addi t3, s1, IPC_SLOT0
    SPAWNR t3, w_ipc
    beqz a0, p_spawn_fail
    addi s1, s1, 1
    j 1b
2:  li s1, 1
p_c_thread:
    /* The previous thread has read its last pong before the next one starts. */
    addi s2, s1, -1
    AWAIT_EQ M_T_DONE, s2
    li t0, MBX
    sd s1, M_T_TURN(t0)
    li s3, 1
p_c_round:
    li s6, 0
p_c_recv:
    /* recv-v2 success: a0 == 0, or the kernel wrote the metadata status (the user runtime's own
       decoding); so the status is poisoned first. WouldBlock with the status untouched is the
       runtime's "nothing yet" and is received again (bounded). */
    addi s6, s6, 1
    li t0, BUF
    li t1, -1
    sd t1, 0x40(t0)
    lw a0, yarm_apsw_cap_ep_p
    li a1, BUF
    li a2, 32
    li a3, (BUF + 0x40)
    li a4, 0x28
    li a5, 0
    li a7, NR_RECV
    ecall
    /* The received frame: opcode +0, round +2, thread +4. */
    mv s4, a0
    li t0, BUF
    ld s7, 0x40(t0)
    li t3, 7
    bne s4, t3, 5f
    li t3, -1
    bne s7, t3, 5f
    li t3, 64
    bgeu s6, t3, 5f
    YIELD
    j p_c_recv
5:  li t0, BUF
    lhu t1, 2(t0)
    lhu t2, 0(t0)
    lhu t4, 4(t0)
    li s5, 0
    beqz s4, 0f
    li t3, -1
    beq s7, t3, 1f
0:  li t3, 0x5050
    bne t2, t3, 1f
    bne t1, s3, 1f
    bne t4, s1, 1f
    li s5, 1
1:  slli a2, s1, 8
    or a2, a2, s3
    mv a3, s5
    LOG m_p_ping
    li t0, (BUF + 0x200)
    li t1, 0x4F50
    sh t1, 0(t0)
    sh s3, 2(t0)
    sh s1, 4(t0)
    li t1, 0x4B4F
    sh t1, 6(t0)
    li s6, 0
3:  addi s6, s6, 1
    lw a0, yarm_apsw_cap_ep_t
    li a1, (BUF + 0x200)
    li a2, 8
    li a3, 0
    li a4, 0
    li a5, -1
    li a7, NR_SEND
    ecall
    beqz a0, 4f
    li t2, 64
    bgeu s6, t2, 4f
    YIELD
    j 3b
4:  addi s3, s3, 1
    li t2, (IPC_ROUNDS + 1)
    bne s3, t2, p_c_round
    addi s1, s1, 1
    ble s1, s10, p_c_thread
    AWAIT_EQ M_T_DONE, s10

    /* ── D: per round, fork one child per secondary; reap each ── */
    li s3, 1
p_d_round:
    AWAIT_ALL_IDLE
    li s1, 1
p_d_fork:
    bgt s1, s10, p_d_reap
    li a0, 0
    li a1, 0
    li a2, 0
    li a3, 0
    li a7, NR_FORK
    ecall
    bnez a3, p_fork_fail
    beqz a0, c_child
    slli t1, s1, 3
    li t0, (BUF + B_CHILD)
    add t1, t1, t0
    sd a0, 0(t1)
    mv a2, a0
    slli a3, s3, 8
    or a3, a3, s1
    LOG m_p_forked
    addi s1, s1, 1
    j p_d_fork
p_d_reap:
    li s1, 1
p_d_reap_one:
    slli t1, s1, 3
    li t0, (BUF + B_CHILD)
    add t1, t1, t0
    ld s5, 0(t1)
    WAIT_BEGIN
    li s0, (WAIT_YIELDS / 8)
1:  mv a2, s5
    slli a3, s3, 8
    or a3, a3, s1
    LOG m_p_reap
    li t0, MBX
    ld t1, M_REAPED(t0)
    beq t1, s5, 2f
    YIELD
    YIELD
    YIELD
    YIELD
    WAIT_NEXT 1b
    mv a2, s5
    slli a3, s3, 8
    or a3, a3, s1
    LOG m_p_reap_timeout
    j p_end
2:  addi s1, s1, 1
    ble s1, s10, p_d_reap_one
    addi s3, s3, 1
    li t2, (FORK_ROUNDS + 1)
    bne s3, t2, p_d_round
p_end:
    li a2, 0
    li a3, 0
    LOG m_p_end
    j park
p_spawn_fail:
    mv a3, a0
    li a2, 0
    LOG m_p_spawn_fail
    j p_end
p_fork_fail:
    mv a2, a0
    LOG m_p_fork_fail
    j p_end
park:
    YIELD
    j park

/* The churn child (s1 = its index, s3 = the round, both inherited from the fork): spins so the
   next fork sees its CPU busy, takes its copy-on-write stack page, reports and exits. */
c_child:
    li t0, CHILD_SPIN
1:  addi t0, t0, -1
    bnez t0, 1b
    sd s1, -64(sp)
    ld t1, -64(sp)
    slli a2, s3, 8
    or a2, a2, s1
    mv a3, t1
    LOG m_c_run
    EXIT

/* ═══════════════════════════════════ workers ══════════════════════════════════════════════ */
/* A deadline receive on an endpoint nobody sends to, after the spawn gate. */
w_deadline:
    li t0, MBX
1:  ld t1, M_GATE(t0)
    li t2, GATE_D
    bne t1, t2, 1b
    li a2, 0
    li a3, 0
    LOG m_d_before
    li t0, MBX
    li t1, 1
    addi t2, t0, M_D_ARMED
    amoadd.d zero, t1, (t2)
    lw a0, yarm_apsw_cap_ep_d
    li a1, (BUF + 0x300)
    li a2, 16
    li a3, DEADLINE_TICKS
    li a4, 0
    li a5, 0
    li a7, NR_RECV_TO
    ecall
    mv s1, a0
    li a2, DEADLINE_TICKS
    mv a3, s1
    LOG m_d_after
    li t0, MBX
    li t1, 1
    addi t2, t0, M_D_DONE
    amoadd.d zero, t1, (t2)
    EXIT

/* A spinner: patterned registers, no kernel entry until P stops it, then the check. */
w_spin:
    li a2, 0
    li a3, 0
    LOG m_w_start
    li t0, MBX
    li t1, 1
    addi t2, t0, M_W_STARTED
    amoadd.d zero, t1, (t2)
    /* base = PAT_BASE + (slot << 32), slot from this thread's own stack. */
    li t6, WSTK
    sub t6, sp, t6
    srli t6, t6, 12
    slli t6, t6, 32
    li t5, PAT_BASE
    add t5, t5, t6
    addi x1, t5, 1
    addi x3, t5, 3
    addi x4, t5, 4
    addi x5, t5, 5
    addi x6, t5, 6
    addi x7, t5, 7
    addi x8, t5, 8
    addi x9, t5, 9
    addi x10, t5, 10
    addi x11, t5, 11
    addi x12, t5, 12
    addi x13, t5, 13
    addi x14, t5, 14
    addi x15, t5, 15
    addi x16, t5, 16
    addi x17, t5, 17
    addi x18, t5, 18
    addi x19, t5, 19
    addi x20, t5, 20
    addi x21, t5, 21
    addi x22, t5, 22
    addi x23, t5, 23
    addi x24, t5, 24
    addi x25, t5, 25
    addi x26, t5, 26
    addi x27, t5, 27
    addi x28, t5, 28
    addi x29, t5, 29
    li t6, MBX
w_spin_loop:
    ld t5, M_STOP(t6)
    beqz t5, w_spin_loop
    /* Recompute the base from sp alone; then every patterned register. */
    li t6, WSTK
    sub t6, sp, t6
    srli t6, t6, 12
    slli t6, t6, 32
    li t5, PAT_BASE
    add t5, t5, t6
    sd zero, -8(sp)
    sd zero, -16(sp)
    CHK x1, 1
    CHK x3, 3
    CHK x4, 4
    CHK x5, 5
    CHK x6, 6
    CHK x7, 7
    CHK x8, 8
    CHK x9, 9
    CHK x10, 10
    CHK x11, 11
    CHK x12, 12
    CHK x13, 13
    CHK x14, 14
    CHK x15, 15
    CHK x16, 16
    CHK x17, 17
    CHK x18, 18
    CHK x19, 19
    CHK x20, 20
    CHK x21, 21
    CHK x22, 22
    CHK x23, 23
    CHK x24, 24
    CHK x25, 25
    CHK x26, 26
    CHK x27, 27
    CHK x28, 28
    CHK x29, 29
    ld a2, -8(sp)
    ld a3, -16(sp)
    li t0, MBX
    addi t1, t0, M_SPIN_BAD
    amoadd.d zero, a2, (t1)
    li t2, 1
    addi t1, t0, M_SPIN_DONE
    amoadd.d zero, t2, (t1)
    LOG m_w_done
    EXIT

/* Sleeper k (= slot - SLEEP_SLOT0): after the spawn gate, announce round r, wait on F[k] until it
   holds r, acknowledge. */
w_sleep:
    MYSLOT s7
    addi s7, s7, -SLEEP_SLOT0
    li t0, MBX
1:  ld t1, M_GATE(t0)
    li t2, GATE_S
    bne t1, t2, 1b
    mv a2, s7
    li a3, 0
    LOG m_s_start
    slli s8, s7, 3
    li t0, MBX
    add s8, s8, t0
    slli s9, s7, 2
    li t0, (MBX + M_F)
    add s9, s9, t0
    li s1, 1
1:  sd s1, M_S_WAITING(s8)
2:  lwu t1, 0(s9)
    beq t1, s1, 3f
    mv a0, s9
    addi a1, s1, -1
    mv a2, t1
    li a7, NR_FUTEX_WAIT
    ecall
    j 2b
3:  sd s1, M_S_ACK(s8)
    slli a2, s7, 8
    or a2, a2, s1
    mv a3, t1
    LOG m_s_woke
    addi s1, s1, 1
    li t2, (WAKE_ROUNDS + 1)
    bne s1, t2, 1b
    EXIT

/* IPC thread k (= slot - IPC_SLOT0): spins until its turn, then pings P and receives its pong. */
w_ipc:
    MYSLOT s7
    addi s7, s7, -IPC_SLOT0
    /* s9 = this thread's own buffers (send +0x00, receive +0x20, metadata +0x40): the threads
       share P's space, so a shared buffer would let the next thread's receive set-up overwrite
       this one's unread pong. */
    slli s9, s7, 7
    li t0, (BUF + B_IPC)
    add s9, s9, t0
    li t0, MBX
1:  ld t1, M_T_TURN(t0)
    bne t1, s7, 1b
    mv a2, s7
    li a3, 0
    LOG m_t_start
    li s1, 1
t_round:
    li t1, 0x5050
    sh t1, 0(s9)
    sh s1, 2(s9)
    sh s7, 4(s9)
    li t1, 0x4749
    sh t1, 6(s9)
    li s6, 0
1:  addi s6, s6, 1
    lw a0, yarm_apsw_cap_ep_p
    mv a1, s9
    li a2, 8
    li a3, 0
    li a4, 0
    li a5, -1
    li a7, NR_SEND
    ecall
    beqz a0, 2f
    li t2, 64
    bgeu s6, t2, 2f
    YIELD
    j 1b
2:  li s6, 0
t_recv:
    addi s6, s6, 1
    li t1, -1
    sd t1, 0x40(s9)
    lw a0, yarm_apsw_cap_ep_t
    addi a1, s9, 0x20
    li a2, 32
    addi a3, s9, 0x40
    li a4, 0x28
    li a5, 0
    li a7, NR_RECV
    ecall
    mv s4, a0
    ld s5, 0x40(s9)
    li t3, 7
    bne s4, t3, 5f
    li t3, -1
    bne s5, t3, 5f
    li t3, 64
    bgeu s6, t3, 5f
    YIELD
    j t_recv
5:  lhu t1, 0x22(s9)
    lhu t2, 0x20(s9)
    lhu t4, 0x24(s9)
    li s8, 0
    beqz s4, 4f
    li t3, -1
    beq s5, t3, 3f
4:  li t3, 0x4F50
    bne t2, t3, 3f
    bne t1, s1, 3f
    bne t4, s7, 3f
    li s8, 1
3:  slli a2, s7, 8
    or a2, a2, s1
    mv a3, s8
    LOG m_t_pong
    addi s1, s1, 1
    li t2, (IPC_ROUNDS + 1)
    bne s1, t2, t_round
    li t0, MBX
    li t1, 1
    addi t2, t0, M_T_DONE
    amoadd.d zero, t1, (t2)
    EXIT

    .balign 8
    .global yarm_apsw_cap_ep_p
yarm_apsw_cap_ep_p: .word 0x61616161
    .global yarm_apsw_cap_ep_t
yarm_apsw_cap_ep_t: .word 0x62626262
    .global yarm_apsw_cap_ep_d
yarm_apsw_cap_ep_d: .word 0x63636363

    STR m_p_start, "APS P_START"
    STR m_p_no_ap, "APS P_NO_AP"
    STR m_p_ap, "APS P_AP_READY"
    STR m_p_pre_timeout, "APS P_PRE_TIMEOUT"
    STR m_p_w_timeout, "APS P_W_TIMEOUT"
    STR m_p_a_done, "APS P_A_DONE"
    STR m_p_b_timeout, "APS P_B_TIMEOUT"
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

    .option pop
.Lyarm_nofp_user_apsched_witness_end:
    .pushsection .yarm_nofp_map, "", @progbits
    .balign 8
    .quad 0x3150464F4E4D5259
    .quad .Lyarm_nofp_user_apsched_witness_begin
    .quad .Lyarm_nofp_user_apsched_witness_end
    .asciz "USER_PROGRAM apsched_witness"
    .balign 8
    .popsection
    .popsection
