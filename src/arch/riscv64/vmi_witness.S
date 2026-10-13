/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * VM-INVALIDATION1 — the RISC-V COW / fork invalidation witness user programs.
 *
 * Two kernel-built tasks in ONE address space:
 *   R — requester, logical CPU 0 (the boot hart): code at 0x2000_0000;
 *   O — observer, logical CPU 1 (the secondary hart): code at 0x2002_0000.
 *
 * Everything is position-independent: data is reached through absolute user VAs built with `li`,
 * strings through `lla`. Linker relaxation is off.
 *
 * User VAs:
 *   0x2007_0000  VMIW_MBX: +0x0 R go, +0x8 O cow-ready, +0x10 R cow-done, +0x18 O fork-armed
 *   0x2008_0000  C: one page of a shared object, read-only and COPY-ON-WRITE in this space,
 *                holding VMIW_OLD_BYTE
 *   0x2009_0000  F: an ordinary writable page
 *
 * Phase COW:
 *   O  logs O_COW_READY (its last kernel entry), reads C (its hart now caches the read-only
 *      translation), publishes cow-ready and spins reading C with no kernel entry until it reads
 *      VMIW_NEW_BYTE or R reports done; then logs O_COW_NEW or O_COW_STALE.
 *   R  waits for cow-ready and stores VMIW_NEW_BYTE into C. That store takes the COW fault on
 *      CPU 0: the production private copy replaces the page O caches, and its invalidation must
 *      reach O's hart before the fault may be reported recovered.
 *
 * Phase fork:
 *   O  logs O_FORK_READY (its last kernel entry), stores into F (its hart now caches a WRITABLE
 *      translation), publishes fork-armed and then stores an incrementing counter into F forever,
 *      with no kernel entry unless a store faults.
 *   R  waits for fork-armed and for the counter to move, then forks (NR 12). The fork
 *      write-protects F; its invalidation must reach O's hart before the child is published.
 *   The CHILD reads F twice across a delay and logs both values: equal only if O's stores stopped
 *      reaching the frame the child shares — that is, only if O's writable translation was retired.
 *   The parent logs R_FORKED with the child's TID.
 *
 * Every VM operation is the kernel's: the COW recovery, the fork's write-protection and the
 * shootdowns that must reach O's hart. The programs only drive and observe.
 */
    .pushsection .text.yarm_vmiw_witness, "ax", @progbits
    .option push
    .option norelax

    .set VMIW_MBX, 0x20070000
    .set VMIW_C_VA, 0x20080000
    .set VMIW_F_VA, 0x20090000

    .set VMIW_NR_YIELD, 0
    .set VMIW_NR_FORK, 12
    .set VMIW_NR_LOG, 15

    .set VMIW_OLD_BYTE, 0x11
    .set VMIW_NEW_BYTE, 0x5A

    .set VMIW_M_R_GO, 0x0
    .set VMIW_M_O_COW_READY, 0x8
    .set VMIW_M_R_COW_DONE, 0x10
    .set VMIW_M_O_FORK_ARMED, 0x18

    .set VMIW_WAIT_YIELDS, 2000000
    .set VMIW_O_SPIN_LOADS, 0x100000000
    /* The counter must have advanced this far before R forks: O is storing, unentered. */
    .set VMIW_ADVANCE, 0x10000
    /* The child's delay between its two reads of F. */
    .set VMIW_CHILD_DELAY, 0x4000000

    /* DebugLog of one of this program's strings; a2 = round, a3 = aux. */
    .macro VMIWLOG msg
    lla a0, \msg
    lla a1, \msg\()_end
    sub a1, a1, a0
    li a7, VMIW_NR_LOG
    ecall
    .endm

    .macro VMIWSTR name, text
\name: .ascii "\text"
\name\()_end:
    .endm

/* ══════════════════════════════════ R: CPU 0 ══════════════════════════════════════════════ */
    .balign 16
    .global yarm_vmiw_r_start
yarm_vmiw_r_start:
    li a2, 0
    li a3, 0
    VMIWLOG vr_m_entered
    li t0, VMIW_MBX
    li t1, 1
    sw t1, VMIW_M_R_GO(t0)
    /* ── COW ── */
    li s0, VMIW_WAIT_YIELDS
vr_wait_cow:
    li t0, VMIW_MBX
    lw t1, VMIW_M_O_COW_READY(t0)
    bnez t1, vr_cow
    li a7, VMIW_NR_YIELD
    ecall
    addi s0, s0, -1
    bnez s0, vr_wait_cow
    li a2, 0
    li a3, 0
    VMIWLOG vr_m_no_observer
    j vr_park
vr_cow:
    /* The store that takes the COW fault on CPU 0. */
    li t0, VMIW_C_VA
    li t1, VMIW_NEW_BYTE
    sb t1, 0(t0)
    lbu t2, 0(t0)
    li t0, VMIW_MBX
    li t3, 1
    sw t3, VMIW_M_R_COW_DONE(t0)
    mv a3, t2
    li a2, 0
    bne t2, t1, vr_cow_fail
    VMIWLOG vr_m_cow_ok
    /* ── fork ── */
    li s0, VMIW_WAIT_YIELDS
vr_wait_fork:
    li t0, VMIW_MBX
    lw t1, VMIW_M_O_FORK_ARMED(t0)
    bnez t1, vr_counter
    li a7, VMIW_NR_YIELD
    ecall
    addi s0, s0, -1
    bnez s0, vr_wait_fork
    li a2, 1
    li a3, 0
    VMIWLOG vr_m_no_observer
    j vr_park
vr_counter:
    /* O is storing with no kernel entry: wait (without entering either) until it is visibly so. */
    li t0, VMIW_F_VA
    ld s1, 0(t0)
    li t4, VMIW_ADVANCE
vr_counter_spin:
    ld t1, 0(t0)
    sub t2, t1, s1
    bltu t2, t4, vr_counter_spin
    /* NR 12. a3 is the error lane on return, so clear it first. */
    li a0, 0
    li a1, 0
    li a2, 0
    li a3, 0
    li a7, VMIW_NR_FORK
    ecall
    bnez a3, vr_fork_fail
    beqz a0, vc_child
    mv a3, a0
    li a2, 0
    VMIWLOG vr_m_forked
    j vr_park
vr_fork_fail:
    li a2, 0
    VMIWLOG vr_m_fork_fail
    j vr_park
vr_cow_fail:
    VMIWLOG vr_m_cow_fail
vr_park:
    li a7, VMIW_NR_YIELD
    ecall
    j vr_park
/* The fork child: two reads of F, a delay apart. */
vc_child:
    li t0, VMIW_F_VA
    ld s1, 0(t0)
    li t1, VMIW_CHILD_DELAY
vc_delay:
    addi t1, t1, -1
    bnez t1, vc_delay
    ld s2, 0(t0)
    mv a2, s1
    mv a3, s2
    VMIWLOG vc_m_child_f
vc_park:
    li a7, VMIW_NR_YIELD
    ecall
    j vc_park
    VMIWSTR vr_m_entered, "VMIW R_ENTERED"
    VMIWSTR vr_m_no_observer, "VMIW R_NO_OBSERVER"
    VMIWSTR vr_m_cow_ok, "VMIW R_COW_OK"
    VMIWSTR vr_m_cow_fail, "VMIW R_COW_FAIL"
    VMIWSTR vr_m_forked, "VMIW R_FORKED"
    VMIWSTR vr_m_fork_fail, "VMIW R_FORK_FAIL"
    VMIWSTR vc_m_child_f, "VMIW C_F"
    .balign 16
    .global yarm_vmiw_r_end
yarm_vmiw_r_end:

/* ══════════════════════════════════ O: CPU 1 ══════════════════════════════════════════════ */
    .balign 16
    .global yarm_vmiw_o_start
yarm_vmiw_o_start:
    /* Wait (yielding) until R runs; nothing is primed yet, so kernel entries here are harmless. */
    li s0, VMIW_WAIT_YIELDS
vo_wait_go:
    li t3, VMIW_MBX
    lw t4, VMIW_M_R_GO(t3)
    bnez t4, vo_cow_arm
    li a7, VMIW_NR_YIELD
    ecall
    addi s0, s0, -1
    bnez s0, vo_wait_go
vo_cow_arm:
    li a2, 0
    li a3, 0
    /* The last kernel entry before the COW observation. */
    VMIWLOG vo_m_cow_ready
    li t0, VMIW_C_VA
    lbu t1, 0(t0)
    li t2, VMIW_OLD_BYTE
    bne t1, t2, vo_prime_fail
    li t3, VMIW_MBX
    li t4, 1
    sw t4, VMIW_M_O_COW_READY(t3)
    li s0, VMIW_O_SPIN_LOADS
    li t2, VMIW_NEW_BYTE
vo_cow_spin:
    lbu t1, 0(t0)
    beq t1, t2, vo_cow_new
    lw t4, VMIW_M_R_COW_DONE(t3)
    bnez t4, vo_cow_last
    addi s0, s0, -1
    bnez s0, vo_cow_spin
    j vo_cow_stale
vo_cow_last:
    lbu t1, 0(t0)
    beq t1, t2, vo_cow_new
vo_cow_stale:
    mv a3, t1
    li a2, 0
    VMIWLOG vo_m_cow_stale
    j vo_fork_arm
vo_cow_new:
    mv a3, t1
    li a2, 0
    VMIWLOG vo_m_cow_new
vo_fork_arm:
    /* A first store BEFORE the last entry: the page's first write may itself trap (to set the
     * PTE's dirty bit), and that entry must not fall inside the observation. */
    li t0, VMIW_F_VA
    li s1, 1
    sd s1, 0(t0)
    li a2, 0
    li a3, 0
    /* The last kernel entry before the fork observation. */
    VMIWLOG vo_m_fork_ready
    li t0, VMIW_F_VA
    li s1, 1
    sd s1, 0(t0)
    li t3, VMIW_MBX
    li t4, 1
    sw t4, VMIW_M_O_FORK_ARMED(t3)
vo_fork_spin:
    addi s1, s1, 1
    sd s1, 0(t0)
    j vo_fork_spin
vo_prime_fail:
    mv a3, t1
    li a2, 0
    VMIWLOG vo_m_prime_fail
vo_park:
    li a7, VMIW_NR_YIELD
    ecall
    j vo_park
    VMIWSTR vo_m_cow_ready, "VMIW O_COW_READY"
    VMIWSTR vo_m_prime_fail, "VMIW O_PRIME_FAIL"
    VMIWSTR vo_m_cow_new, "VMIW O_COW_NEW"
    VMIWSTR vo_m_cow_stale, "VMIW O_COW_STALE"
    VMIWSTR vo_m_fork_ready, "VMIW O_FORK_READY"
    .balign 16
    .global yarm_vmiw_o_end
yarm_vmiw_o_end:

    .option pop
    .popsection
