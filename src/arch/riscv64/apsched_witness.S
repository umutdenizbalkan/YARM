/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * AP-SCHED1 — the RISC-V production AP-scheduling witness user program.
 *
 * ONE App-class process P, kernel-built at 0x2000_0000. Everything after that is production
 * syscalls: P spawns its workers as threads (NR 11) and forks its churn children (NR 12), and the
 * production balanced placement — not the witness — decides which CPU each lands on and fixes it
 * as that task's home. No task is pinned, and no identity but P's is fixed.
 *
 * User VAs:
 *   0x2001_0000  P's stack (top 0x2001_0ff0)
 *   0x2002_0000  worker stacks, one page per slot (slot N top = 0x2002_0ff0 + N * 0x1000)
 *   0x2007_0000  MBX: shared words (below); the kernel publishes only its observation words
 *   0x2008_0000  BUF: IPC buffers and receive metadata
 *
 * MBX (u64 unless noted):
 *   +0x000 AP_READY   kernel: CPU 1 took its production admission
 *   +0x008 STOP       P: spinners stop
 *   +0x010 SPIN_DONE  spinners: amoadd 1 each
 *   +0x018 SPIN_BAD   spinners: amoadd their mismatch count
 *   +0x020 PRE0 / +0x028 PRE1   kernel: timer preemptions of this process's tasks per CPU
 *   +0x030 IDLE1      kernel: CPU 1 idle arrivals
 *   +0x038 REAPED     kernel: the last churn child the reaper reaped
 *   +0x040 F (u32)    the sleeper's futex word
 *   +0x048 S_WAITING  +0x050 S_ACK   +0x058 D_DONE   +0x060 T_DONE   +0x068 D_ARMED
 *   +0x070 CPU1_IDLE  kernel: 1 from CPU 1's idle arrival until it next leaves idle
 *
 * Phases (P drives; the kernel records every step with a causal stamp):
 *   A  a deadline thread D (NR 5, nobody sends) and four non-yielding spinners. Each spinner loads
 *      a per-thread pattern into x1, x3..x29, spins on one MBX word with no kernel entry until P
 *      stops it, and then checks every patterned register. P stops them only once the kernel has
 *      seen enough TIMER preemptions of this process on BOTH CPUs.
 *   B  a sleeper S announces round r and futex-waits; once S has announced r AND the kernel
 *      reports CPU 1 idle (it was not while S ran), P stores F and wakes S (NR 10) — the wake must
 *      reach idle CPU 1 by IPI. WAKE_ROUNDS rounds.
 *   C  a thread T pings P over E_P (NR 1) and receives P's pong on E_T (NR 2): every round is a
 *      remote wake each way. IPC_ROUNDS rounds.
 *   D  FORK_ROUNDS forked children, each running, touching its (copy-on-write) stack and exiting
 *      wherever placement put it; the witness reaps each through the production NR 31 transaction.
 *
 * Linker relaxation is off; data is reached through absolute VAs built with `li`, strings and
 * entry points through `lla`, capabilities through literal words the kernel patches.
 */
    .pushsection .text.yarm_apsw_witness, "ax", @progbits
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
    .set M_PRE0, 0x020
    .set M_PRE1, 0x028
    .set M_IDLE1, 0x030
    .set M_REAPED, 0x038
    .set M_F, 0x040
    .set M_S_WAITING, 0x048
    .set M_S_ACK, 0x050
    .set M_D_DONE, 0x058
    .set M_T_DONE, 0x060
    .set M_D_ARMED, 0x068
    .set M_CPU1_IDLE, 0x070

    .set SPINNERS, 4
    .set PRE0_MIN, 3
    .set PRE1_MIN, 6
    .set DEADLINE_TICKS, 40
    .set WAKE_ROUNDS, 5
    .set IPC_ROUNDS, 6
    .set FORK_ROUNDS, 6

    /* Bounded waits, in yields. */
    .set WAIT_YIELDS, 400000
    /* The spinners' pattern base; slot N adds N << 32. */
    .set PAT_BASE, 0xA5A5000000000000

    .macro YIELD
    li a7, NR_YIELD
    ecall
    .endm

    /* DebugLog of one of this program's strings; a2 = round, a3 = aux. */
    .macro LOG msg
    lla a0, \msg
    lla a1, \msg\()_end
    sub a1, a1, a0
    li a7, NR_LOG
    ecall
    .endm

    .macro STR name, text
\name: .ascii "\text"
\name\()_end:
    .endm

    /* NR 11: a thread at `entry` on worker stack `slot`; a0 = its TID, 0 on refusal. */
    .macro SPAWN slot, entry
    li a0, (MBX + 0x800 + \slot * 0x10)
    li a1, (WSTK + \slot * 0x1000 + 0xff0)
    lla a2, \entry
    li a3, 0
    li a4, 0
    li a5, 0
    li a7, NR_SPAWN
    ecall
    .endm

    /* Bounded: wait until the kernel reports CPU 1 idle, so the balanced placement of the next
       task sees CPU 1 empty (a previous phase's threads may still be exiting there). On timeout
       carry on — the grader then sees where placement put the task. */
    .macro AWAIT_CPU1_IDLE
    li s0, WAIT_YIELDS
8:  li t0, MBX
    ld t1, M_CPU1_IDLE(t0)
    bnez t1, 9f
    YIELD
    addi s0, s0, -1
    bnez s0, 8b
9:
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
    li s0, WAIT_YIELDS
p_ap:
    li t0, MBX
    ld t1, M_AP_READY(t0)
    bnez t1, p_a
    YIELD
    addi s0, s0, -1
    bnez s0, p_ap
    LOG m_p_no_ap
    j p_end

    /* ── A: the deadline thread, then four spinners ── */
p_a:
    li a2, 0
    li a3, 0
    LOG m_p_ap
    SPAWN 0, w_deadline
    beqz a0, p_spawn_fail
    li s0, WAIT_YIELDS
1:  li t0, MBX
    ld t1, M_D_ARMED(t0)
    bnez t1, 2f
    YIELD
    addi s0, s0, -1
    bnez s0, 1b
2:  SPAWN 1, w_spin
    beqz a0, p_spawn_fail
    SPAWN 2, w_spin
    beqz a0, p_spawn_fail
    SPAWN 3, w_spin
    beqz a0, p_spawn_fail
    SPAWN 4, w_spin
    beqz a0, p_spawn_fail
    li s0, WAIT_YIELDS
p_pre:
    li t0, MBX
    ld t1, M_PRE1(t0)
    li t2, PRE1_MIN
    bltu t1, t2, 1f
    ld t1, M_PRE0(t0)
    li t2, PRE0_MIN
    bgeu t1, t2, p_stop
1:  YIELD
    addi s0, s0, -1
    bnez s0, p_pre
    li t0, MBX
    ld a2, M_PRE0(t0)
    ld a3, M_PRE1(t0)
    LOG m_p_pre_timeout
p_stop:
    li t0, MBX
    li t1, 1
    sd t1, M_STOP(t0)
    li s0, WAIT_YIELDS
1:  li t0, MBX
    ld t1, M_SPIN_DONE(t0)
    li t2, SPINNERS
    bne t1, t2, 2f
    ld t1, M_D_DONE(t0)
    bnez t1, 3f
2:  YIELD
    addi s0, s0, -1
    bnez s0, 1b
    LOG m_p_a_timeout
3:  li t0, MBX
    ld a2, M_SPIN_DONE(t0)
    ld a3, M_SPIN_BAD(t0)
    LOG m_p_a_done

    /* ── B: wake the sleeper on an idle CPU ── */
    AWAIT_CPU1_IDLE
    SPAWN 5, w_sleep
    beqz a0, p_spawn_fail
    li s1, 1
p_b_round:
    li s0, WAIT_YIELDS
1:  li t0, MBX
    ld t1, M_S_WAITING(t0)
    beq t1, s1, 2f
    YIELD
    addi s0, s0, -1
    bnez s0, 1b
    mv a2, s1
    li a3, 0
    LOG m_p_b_timeout
    j p_c
2:  li s0, WAIT_YIELDS
3:  li t0, MBX
    ld t1, M_CPU1_IDLE(t0)
    bnez t1, 4f
    YIELD
    addi s0, s0, -1
    bnez s0, 3b
    mv a2, s1
    li a3, 0
    LOG m_p_no_idle
4:  li t0, MBX
    sw s1, M_F(t0)
    mv a2, s1
    li a3, 0
    LOG m_p_wake
    li a0, (MBX + M_F)
    li a1, 1
    li a7, NR_FUTEX_WAKE
    ecall
    mv s4, a0
    li s0, WAIT_YIELDS
5:  li t0, MBX
    ld t1, M_S_ACK(t0)
    beq t1, s1, 6f
    YIELD
    addi s0, s0, -1
    bnez s0, 5b
    mv a2, s1
    li a3, 0
    LOG m_p_no_ack
    j p_c
6:  mv a2, s1
    mv a3, s4
    LOG m_p_acked
    addi s1, s1, 1
    li t2, (WAKE_ROUNDS + 1)
    bne s1, t2, p_b_round

    /* ── C: ping-pong with T ── */
p_c:
    AWAIT_CPU1_IDLE
    SPAWN 6, w_ipc
    beqz a0, p_spawn_fail
    li s1, 1
p_c_round:
    li s6, 0
p_c_recv:
    /* recv-v2 success: a0 == 0, or the kernel wrote the metadata status (the user runtime's own
       decoding); so the status is poisoned first. WouldBlock with the status untouched is the
       runtime's "nothing yet" — a receive that met a concurrent send while publishing itself is
       restored with the message still queued — and is received again (bounded). */
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
    /* The received frame: the sender's opcode at +0, its round at +2. */
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
    li s5, 0
    beqz s4, 0f
    li t3, -1
    beq s7, t3, 1f
0:  li t3, 0x5050
    bne t2, t3, 1f
    bne t1, s1, 1f
    li s5, 1
1:  mv a2, s1
    mv a3, s5
    LOG m_p_ping
    bnez s5, 2f
    mv a2, s4
    mv a3, s7
    LOG m_p_ping_raw
2:  li t0, (BUF + 0x200)
    li t1, 0x4F50
    sh t1, 0(t0)
    sh s1, 2(t0)
    li t1, 0x4B4F
    sh t1, 4(t0)
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
4:  addi s1, s1, 1
    li t2, (IPC_ROUNDS + 1)
    bne s1, t2, p_c_round
    li s0, WAIT_YIELDS
5:  li t0, MBX
    ld t1, M_T_DONE(t0)
    bnez t1, p_d
    YIELD
    addi s0, s0, -1
    bnez s0, 5b
    LOG m_p_c_timeout

    /* ── D: fork, let the child run and exit, reap it ── */
p_d:
    li s1, 1
p_d_round:
    AWAIT_CPU1_IDLE
    li a0, 0
    li a1, 0
    li a2, 0
    li a3, 0
    li a7, NR_FORK
    ecall
    bnez a3, p_fork_fail
    beqz a0, c_child
    mv s5, a0
    mv a2, s5
    mv a3, s1
    LOG m_p_forked
    li s0, (WAIT_YIELDS / 8)
1:  mv a2, s5
    mv a3, s1
    LOG m_p_reap
    li t0, MBX
    ld t1, M_REAPED(t0)
    beq t1, s5, 2f
    YIELD
    YIELD
    YIELD
    YIELD
    addi s0, s0, -1
    bnez s0, 1b
    mv a2, s5
    mv a3, s1
    LOG m_p_reap_timeout
    j p_end
2:  addi s1, s1, 1
    li t2, (FORK_ROUNDS + 1)
    bne s1, t2, p_d_round
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

/* The churn child: runs wherever it was placed, takes its copy-on-write stack page, reports and
   exits. */
c_child:
    sd s1, -64(sp)
    ld t1, -64(sp)
    mv a2, s1
    mv a3, t1
    LOG m_c_run
    EXIT

/* ═══════════════════════════════════ workers ══════════════════════════════════════════════ */
/* D: a deadline receive on an endpoint nobody sends to. */
w_deadline:
    li a2, 0
    li a3, 0
    LOG m_d_before
    li t0, MBX
    li t1, 1
    sd t1, M_D_ARMED(t0)
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
    sd t1, M_D_DONE(t0)
    EXIT

/* A spinner: patterned registers, no kernel entry until P stops it, then the check. */
w_spin:
    li a2, 0
    li a3, 0
    LOG m_w_start
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

/* S: announce round r, wait on F until it holds r, acknowledge. */
w_sleep:
    li a2, 0
    li a3, 0
    LOG m_s_start
    li s1, 1
1:  li t0, MBX
    sd s1, M_S_WAITING(t0)
2:  li t0, MBX
    lwu t1, M_F(t0)
    beq t1, s1, 3f
    li a0, (MBX + M_F)
    addi a1, s1, -1
    mv a2, t1
    li a7, NR_FUTEX_WAIT
    ecall
    j 2b
3:  li t0, MBX
    sd s1, M_S_ACK(t0)
    mv a2, s1
    mv a3, t1
    LOG m_s_woke
    addi s1, s1, 1
    li t2, (WAKE_ROUNDS + 1)
    bne s1, t2, 1b
    EXIT

/* T: ping P, receive its pong. */
w_ipc:
    li a2, 0
    li a3, 0
    LOG m_t_start
    li s1, 1
t_round:
    li t0, (BUF + 0x280)
    li t1, 0x5050
    sh t1, 0(t0)
    sh s1, 2(t0)
    li t1, 0x4749
    sh t1, 4(t0)
    sh t1, 6(t0)
    li s6, 0
1:  addi s6, s6, 1
    lw a0, yarm_apsw_cap_ep_p
    li a1, (BUF + 0x280)
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
    li t0, BUF
    li t1, -1
    sd t1, 0x140(t0)
    lw a0, yarm_apsw_cap_ep_t
    li a1, (BUF + 0x100)
    li a2, 32
    li a3, (BUF + 0x140)
    li a4, 0x28
    li a5, 0
    li a7, NR_RECV
    ecall
    mv s4, a0
    li t0, BUF
    ld s7, 0x140(t0)
    li t3, 7
    bne s4, t3, 5f
    li t3, -1
    bne s7, t3, 5f
    li t3, 64
    bgeu s6, t3, 5f
    YIELD
    j t_recv
5:  li t0, BUF
    lhu t1, 0x102(t0)
    lhu t2, 0x100(t0)
    li s5, 0
    beqz s4, 4f
    li t3, -1
    beq s7, t3, 3f
4:  li t3, 0x4F50
    bne t2, t3, 3f
    bne t1, s1, 3f
    li s5, 1
3:  mv a2, s1
    mv a3, s5
    LOG m_t_pong
    addi s1, s1, 1
    li t2, (IPC_ROUNDS + 1)
    bne s1, t2, t_round
    li t0, MBX
    li t1, 1
    sd t1, M_T_DONE(t0)
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
    STR m_p_a_timeout, "APS P_A_TIMEOUT"
    STR m_p_a_done, "APS P_A_DONE"
    STR m_p_b_timeout, "APS P_B_TIMEOUT"
    STR m_p_no_idle, "APS P_NO_IDLE"
    STR m_p_wake, "APS P_WAKE"
    STR m_p_no_ack, "APS P_NO_ACK"
    STR m_p_acked, "APS P_ACKED"
    STR m_p_ping, "APS P_PING"
    STR m_p_ping_raw, "APS P_PING_RAW"
    STR m_p_c_timeout, "APS P_C_TIMEOUT"
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
    .popsection
