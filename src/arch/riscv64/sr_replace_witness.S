/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * SHARED-REGION-LIFETIME2 — the RISC-V shared-region replacement witness user programs.
 *
 * Two kernel-built tasks in ONE address space:
 *   R — receiver, logical CPU 0 (the boot hart): code at 0x2000_0000;
 *   O — observer, logical CPU 1 (the secondary hart): code at 0x2002_0000.
 *
 * Everything is position-independent: data is reached through absolute user VAs built with `li`,
 * strings through `lla`, capabilities through PC-relative literal words the kernel patches at
 * provisioning. Linker relaxation is off.
 *
 * User VAs:
 *   0x2007_0000  SRR_MBX: +0x0 O ready, +0x8 R done, +0x10 R's a0, +0x18 R's a1
 *   0x2008_0000  W: the window, mapped at provisioning to a page of an allocator-owned object
 *                filled with SRR_OLD_BYTE
 *
 * The run:
 *   O  logs O_READY (its last kernel entry), reads W through the OLD translation (so its hart now
 *      caches it), publishes SRR_MBX ready, and spins reading W with no kernel entry until the byte
 *      changes or its budget ends; then it logs O_NEW or O_STALE.
 *   R  waits (yielding) for SRR_MBX ready, sends a one-page shared region of the source object to its
 *      own endpoint (NR 1), and receives it with NR 2 mapped read-only AT W — replacing the page O
 *      is reading — then checks its own view of W and logs R_OK or a failure step.
 *
 * Every lifetime operation is the kernel's: the receive's install, the shootdown that must reach
 * O's hart, the displaced page's hold and its settlement. The programs only drive and observe.
 */
    .pushsection .text.yarm_srr_witness, "ax", @progbits
    .option push
    .option norelax

    .set SRR_MBX, 0x20070000
    .set SRR_W_VA, 0x20080000
    .set SRR_W_LAST, 0x20080FFF

    .set SRR_NR_YIELD, 0
    .set SRR_NR_SEND, 1
    .set SRR_NR_RECV, 2
    .set SRR_NR_LOG, 15

    .set SRR_OLD_BYTE, 0x11
    .set SRR_NEW_BYTE, 0x5A
    .set SRR_MAP_INTENT_READ, 1

    .set SRR_M_O_READY, 0x0
    .set SRR_M_R_DONE, 0x8
    .set SRR_M_R_A0, 0x10
    .set SRR_M_R_A1, 0x18
    .set SRR_M_R_GO, 0x20

    /* R's bounded wait for the observer, in yields; O's spin budget, in loads. */
    .set SRR_R_WAIT_YIELDS, 2000000
    .set SRR_O_GO_YIELDS, 20000000
    .set SRR_O_SPIN_LOADS, 0x100000000

    /* DebugLog of one of this program's strings; a2 = round, a3 = aux. */
    .macro SRRLOG msg
    lla a0, \msg
    lla a1, \msg\()_end
    sub a1, a1, a0
    li a7, SRR_NR_LOG
    ecall
    .endm

    .macro SRRSTR name, text
\name: .ascii "\text"
\name\()_end:
    .endm

/* ══════════════════════════════════ R: CPU 0 ══════════════════════════════════════════════ */
    .balign 16
    .global yarm_srr_r_start
yarm_srr_r_start:
    li a2, 0
    li a3, 0
    SRRLOG r_m_entered
    /* Tell O to arm: it primes W only once R is running, so its spin covers R's receive. */
    li t0, SRR_MBX
    li t1, 1
    sw t1, SRR_M_R_GO(t0)
    li s0, SRR_R_WAIT_YIELDS
r_wait:
    li t0, SRR_MBX
    lw t1, SRR_M_O_READY(t0)
    bnez t1, r_go
    li a7, SRR_NR_YIELD
    ecall
    addi s0, s0, -1
    bnez s0, r_wait
    li a2, 0
    li a3, 0
    SRRLOG r_m_no_observer
    j r_park
r_go:
    /* NR 1: the shared-region send of [0, PAGE) of the source object to R's own endpoint. */
    lw a0, yarm_srr_r_cap_ep
    li a1, 0
    li a2, 4096
    li a3, 0
    li a4, 0
    lw a5, yarm_srr_r_cap_src
    li a7, SRR_NR_SEND
    ecall
    bnez a0, r_send_fail
    /* NR 2: receive it mapped read-only at W, replacing the page O is reading. */
    lw a0, yarm_srr_r_cap_ep
    li a1, SRR_W_VA
    li a2, 4096
    li a3, 0
    li a4, SRR_MAP_INTENT_READ
    li a5, 0
    li a7, SRR_NR_RECV
    ecall
    mv s1, a0
    mv s2, a1
    li t0, SRR_MBX
    sd s1, SRR_M_R_A0(t0)
    sd s2, SRR_M_R_A1(t0)
    li t1, 1
    sw t1, SRR_M_R_DONE(t0)
    bnez s1, r_recv_fail
    li t2, 4096
    bne s2, t2, r_recv_fail
    /* R's own view of W: the granted page's bytes, first and last. */
    li t0, SRR_W_VA
    lbu t1, 0(t0)
    li t2, SRR_NEW_BYTE
    bne t1, t2, r_bytes_fail
    li t0, SRR_W_LAST
    lbu t1, 0(t0)
    bne t1, t2, r_bytes_fail
    mv a2, s1
    mv a3, s2
    SRRLOG r_m_ok
    j r_park
r_send_fail:
    mv a3, a0
    li a2, 1
    SRRLOG r_m_send_fail
    j r_park
r_recv_fail:
    mv a2, s1
    mv a3, s2
    SRRLOG r_m_recv_fail
    j r_park
r_bytes_fail:
    mv a3, t1
    li a2, 0
    SRRLOG r_m_bytes_fail
r_park:
    li a7, SRR_NR_YIELD
    ecall
    j r_park
    .balign 8
    .global yarm_srr_r_cap_ep
yarm_srr_r_cap_ep: .word 0x61616161
    .global yarm_srr_r_cap_src
yarm_srr_r_cap_src: .word 0x62626262
    SRRSTR r_m_entered, "SRR R_ENTERED"
    SRRSTR r_m_no_observer, "SRR R_NO_OBSERVER"
    SRRSTR r_m_send_fail, "SRR R_SEND_FAIL"
    SRRSTR r_m_recv_fail, "SRR R_RECV_FAIL"
    SRRSTR r_m_bytes_fail, "SRR R_BYTES_FAIL"
    SRRSTR r_m_ok, "SRR R_OK"
    .balign 16
    .global yarm_srr_r_end
yarm_srr_r_end:

/* ══════════════════════════════════ O: CPU 1 ══════════════════════════════════════════════ */
    .balign 16
    .global yarm_srr_o_start
yarm_srr_o_start:
    /* Wait (yielding) until R runs; nothing is primed yet, so kernel entries here are harmless. */
    li s0, SRR_O_GO_YIELDS
o_wait_go:
    li t3, SRR_MBX
    lw t4, SRR_M_R_GO(t3)
    bnez t4, o_arm
    li a7, SRR_NR_YIELD
    ecall
    addi s0, s0, -1
    bnez s0, o_wait_go
o_arm:
    li a2, 0
    li a3, 0
    /* The last kernel entry before the observation: the kernel records this hart's count. */
    SRRLOG o_m_ready
    li t0, SRR_W_VA
    lbu t1, 0(t0)
    li t2, SRR_OLD_BYTE
    bne t1, t2, o_prime_fail
    li t3, SRR_MBX
    li t4, 1
    sw t4, SRR_M_O_READY(t3)
    li s0, SRR_O_SPIN_LOADS
    li t2, SRR_NEW_BYTE
o_spin:
    lbu t1, 0(t0)
    beq t1, t2, o_new
    /* R finished its receive and W still reads OLD here: one last load, then it is stale. */
    lw t4, SRR_M_R_DONE(t3)
    bnez t4, o_last
    addi s0, s0, -1
    bnez s0, o_spin
    j o_stale
o_last:
    lbu t1, 0(t0)
    beq t1, t2, o_new
o_stale:
    mv a3, t1
    li a2, 0
    SRRLOG o_m_stale
    j o_park
o_new:
    li t5, SRR_W_LAST
    lbu a3, 0(t5)
    li a2, 0
    SRRLOG o_m_new
    j o_park
o_prime_fail:
    mv a3, t1
    li a2, 0
    SRRLOG o_m_prime_fail
o_park:
    li a7, SRR_NR_YIELD
    ecall
    j o_park
    SRRSTR o_m_ready, "SRR O_READY"
    SRRSTR o_m_prime_fail, "SRR O_PRIME_FAIL"
    SRRSTR o_m_new, "SRR O_NEW"
    SRRSTR o_m_stale, "SRR O_STALE"
    .balign 16
    .global yarm_srr_o_end
yarm_srr_o_end:

    .option pop
    .popsection
