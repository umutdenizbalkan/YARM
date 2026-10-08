/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * QEMU-SMP2 — the AArch64 two-CPU interrupt / TLB / context witness user programs.
 *
 * Four kernel-built tasks, each copied to 0x2000_0000 in its own address space:
 *   S  — server, CPU 1 (the AP), address space A;
 *   C  — client, CPU 0 (the BSP), address space B;
 *   H1 — helper blocked on CPU 1;  H0 — helper blocked on CPU 0 (one image, two caps).
 * Everything is position-independent: data is reached through absolute user VAs built with
 * movz/movk, strings through `adr`, capabilities through PC-relative literal words that the
 * kernel patches at provisioning.
 *
 * User VAs (S and C):
 *   0x2001_0ff0  SP (never moves)
 *   0x2003_0000  receive buffer; +0x100 the framed NR 6 request
 *   0x2004_0000  recv-v2 metadata (40 bytes)
 *   0x2005_0000  reply source "RPLY-OK!"
 *   0x2006_0000  PAT: q0..q31, FPCR@512, FPSR@520, NZCV@528, TPIDR_EL0@536, GPR base P@0x240
 *                +0x300 scratch, +0x340 last check result, +0x400 capture (same layout as PAT,
 *                GPRs x0..x30 at +0x640, SP at +0x738)
 *   0x2007_0000  MBX, ONE frame shared by S and C
 *   0x2008_0000  W, created by each task through NR 13, replaced by the OTHER task through NR 3
 *   0x2009_0000  R, the residency probe; the kernel re-points it WITHOUT any invalidation when a
 *                requester arms a round, so a target still reading its primed value provably did
 *                not reload its translations in between
 *
 * Phases (the kernel records every step; the programs only drive and check):
 *   P1  parked targets: C NR6 -> blocked S (CPU 1 parked) ; S NR7 -> blocked C (CPU 0 parked)
 *   P2  EL0 targets:    C NR6 -> H1 while S spins in window A on CPU 1 (SGI taken from EL0);
 *                       S NR6 -> H0 while C spins in window B on CPU 0 (SGI taken from EL0)
 *   P3  serial TLB rounds: odd r C replaces S's W while S is resident; even r the mirror
 *   P4  mutual rounds: both replace each other's W at once
 *
 * Context windows load x0..x15, x19..x30 (P+i), q0..q31, FPCR, FPSR, NZCV and TPIDR_EL0 from
 * PAT and compare after. x16/x17 are the window's own instrument (loop address and loaded
 * word); x18 is the platform register the kernel owns; x0..x5 and x8 are syscall lanes across
 * an `svc`.
 */
    .pushsection .text.yarm_smp2_witness, "ax", %progbits

    .set PAYLOAD, 0x20030000
    .set META, 0x20040000
    .set REPLY_SRC, 0x20050000
    .set PAT, 0x20060000
    .set MBX, 0x20070000
    .set W_VA, 0x20080000
    .set R_VA, 0x20090000
    .set SP_TOP, 0x20010ff0

    .set NR_YIELD, 0
    .set NR_RECV, 2
    .set NR_VMMAP, 3
    .set NR_RECV_TO, 5
    .set NR_CALL, 6
    .set NR_REPLY, 7
    .set NR_ANON, 13
    .set NR_LOG, 15

    /* GPR check masks: bit i = xi, bit 31 = SP. */
    .set WIN_MASK, 0xFFF8FFFF
    .set SYS_MASK, 0xFFF8FEC0

    .set P1_ROUNDS, 4
    .set TLB_ROUNDS, 4
    .set MUT_ROUNDS, YARM_MUT_ROUNDS
    .set PARK_TICKS, 60

    /* MBX words. Round-indexed words live at `M_x + round * 8`, so each field needs room for
     * the most rounds any phase runs: 0x40 holds eight (the plain build's four), 0x100 holds 32
     * (QEMU-LOCK2's twelve mutual rounds). The plain build's offsets are unchanged. */
    .if YARM_MUT_ROUNDS > 7
    .set MSTRIDE, 0x100
    .else
    .set MSTRIDE, 0x40
    .endif
    .set M_S_WAIT, 1 * MSTRIDE
    .set M_C_WAIT, 2 * MSTRIDE
    .set M_A_OPEN, 3 * MSTRIDE
    .set M_A_DONE, 3 * MSTRIDE + 0x08
    .set M_B_OPEN, 3 * MSTRIDE + 0x10
    .set M_B_DONE, 3 * MSTRIDE + 0x18
    .set M_PRIMED, 4 * MSTRIDE
    .set M_DONE, 5 * MSTRIDE
    .set M_OBS, 6 * MSTRIDE
    .set M_READY_S, 7 * MSTRIDE
    .set M_READY_C, 8 * MSTRIDE
    .set M_NR3_S, 9 * MSTRIDE
    .set M_NR3_C, 10 * MSTRIDE
    .set M_CHK_S, 11 * MSTRIDE
    .set M_CHK_C, 12 * MSTRIDE

    .macro LI32 reg, val
    movz \reg, #(((\val) >> 16) & 0xffff), lsl #16
    movk \reg, #((\val) & 0xffff)
    .endm

    .macro LI64 reg, val
    movz \reg, #((\val) & 0xffff)
    movk \reg, #(((\val) >> 16) & 0xffff), lsl #16
    movk \reg, #(((\val) >> 32) & 0xffff), lsl #32
    movk \reg, #(((\val) >> 48) & 0xffff), lsl #48
    .endm

    /* DebugLog of one of this program's strings; x2 = round, x3/x4 = aux. */
    .macro LOG msg
    adr x0, \msg
    adr x1, \msg\()_end
    sub x1, x1, x0
    mov x8, #NR_LOG
    svc #0
    .endm

    /* MBX[off] = 1 */
    .macro MSET off
    LI32 x16, (MBX + \off)
    mov x17, #1
    str x17, [x16]
    .endm

    /* MBX[off + 8 * xreg] = 1 */
    .macro MSETR off, xreg
    LI32 x16, (MBX + \off)
    add x16, x16, \xreg, lsl #3
    mov x17, #1
    str x17, [x16]
    .endm

    /* Spin until MBX[off] != 0. */
    .macro MWAIT off
    LI32 x16, (MBX + \off)
1:  ldr x17, [x16]
    cbz x17, 1b
    .endm

    /* Spin until MBX[off + 8 * xreg] != 0. */
    .macro MWAITR off, xreg
    LI32 x16, (MBX + \off)
    add x16, x16, \xreg, lsl #3
1:  ldr x17, [x16]
    cbz x17, 1b
    .endm

    .macro DELAY n
    LI32 x9, \n
9:  subs x9, x9, #1
    b.ne 9b
    .endm

    /* The round counter lives in the scratch word (every register is reloaded by a window). */
    .macro RND_SET val
    LI32 x16, (PAT + 0x300)
    mov x17, #\val
    str x17, [x16]
    .endm
    .macro RND_GET xreg
    LI32 x16, (PAT + 0x300)
    ldr \xreg, [x16]
    .endm
    .macro RND_INC
    LI32 x16, (PAT + 0x300)
    ldr x17, [x16]
    add x17, x17, #1
    str x17, [x16]
    .endm

    /* Load the whole checked context from PAT. NZCV last. */
    .macro LOAD_CTX
    LI32 x16, PAT
    ldp q0, q1, [x16, #0]
    ldp q2, q3, [x16, #32]
    ldp q4, q5, [x16, #64]
    ldp q6, q7, [x16, #96]
    ldp q8, q9, [x16, #128]
    ldp q10, q11, [x16, #160]
    ldp q12, q13, [x16, #192]
    ldp q14, q15, [x16, #224]
    ldp q16, q17, [x16, #256]
    ldp q18, q19, [x16, #288]
    ldp q20, q21, [x16, #320]
    ldp q22, q23, [x16, #352]
    ldp q24, q25, [x16, #384]
    ldp q26, q27, [x16, #416]
    ldp q28, q29, [x16, #448]
    ldp q30, q31, [x16, #480]
    ldr x17, [x16, #512]
    msr fpcr, x17
    ldr x17, [x16, #520]
    msr fpsr, x17
    ldr x17, [x16, #536]
    msr tpidr_el0, x17
    ldr x17, [x16, #0x240]
    add x0, x17, #0
    add x1, x17, #1
    add x2, x17, #2
    add x3, x17, #3
    add x4, x17, #4
    add x5, x17, #5
    add x6, x17, #6
    add x7, x17, #7
    add x8, x17, #8
    add x9, x17, #9
    add x10, x17, #10
    add x11, x17, #11
    add x12, x17, #12
    add x13, x17, #13
    add x14, x17, #14
    add x15, x17, #15
    add x19, x17, #19
    add x20, x17, #20
    add x21, x17, #21
    add x22, x17, #22
    add x23, x17, #23
    add x24, x17, #24
    add x25, x17, #25
    add x26, x17, #26
    add x27, x17, #27
    add x28, x17, #28
    add x29, x17, #29
    add x30, x17, #30
    ldr x17, [x16, #528]
    msr nzcv, x17
    .endm

    /* Capture the checked context into PAT+0x400 before anything can change it. */
    .macro CAPTURE
    LI32 x16, (PAT + 0x400)
    mrs x17, nzcv
    str x17, [x16, #528]
    mrs x17, fpcr
    str x17, [x16, #512]
    mrs x17, fpsr
    str x17, [x16, #520]
    mrs x17, tpidr_el0
    str x17, [x16, #536]
    stp q0, q1, [x16, #0]
    stp q2, q3, [x16, #32]
    stp q4, q5, [x16, #64]
    stp q6, q7, [x16, #96]
    stp q8, q9, [x16, #128]
    stp q10, q11, [x16, #160]
    stp q12, q13, [x16, #192]
    stp q14, q15, [x16, #224]
    stp q16, q17, [x16, #256]
    stp q18, q19, [x16, #288]
    stp q20, q21, [x16, #320]
    stp q22, q23, [x16, #352]
    stp q24, q25, [x16, #384]
    stp q26, q27, [x16, #416]
    stp q28, q29, [x16, #448]
    stp q30, q31, [x16, #480]
    add x17, x16, #0x240
    stp x0, x1, [x17, #0]
    stp x2, x3, [x17, #16]
    stp x4, x5, [x17, #32]
    stp x6, x7, [x17, #48]
    stp x8, x9, [x17, #64]
    stp x10, x11, [x17, #80]
    stp x12, x13, [x17, #96]
    stp x14, x15, [x17, #112]
    stp x19, x20, [x17, #152]
    stp x21, x22, [x17, #168]
    stp x23, x24, [x17, #184]
    stp x25, x26, [x17, #200]
    stp x27, x28, [x17, #216]
    stp x29, x30, [x17, #232]
    mov x0, sp
    str x0, [x17, #248]
    .endm

    /* Compare the capture with PAT under GPR mask `mask`; branch to `fail` on any difference. */
    .macro CHECK mask, fail
    LI32 x2, \mask
    bl \fail\()_check
    cbnz x0, \fail
    .endm

    /* The comparison body: x2 = mask. x0 = mismatch bits (0..30 GPRs, 31 SP, 32 FP/control). */
    .macro CHECK_BODY
    LI32 x9, PAT
    add x10, x9, #0x400
    mov x0, #0
    mov x11, #0
1:  ldr x12, [x9, x11]
    ldr x13, [x10, x11]
    cmp x12, x13
    b.eq 2f
    orr x0, x0, #0x100000000
2:  add x11, x11, #8
    cmp x11, #544
    b.lo 1b
    ldr x12, [x9, #0x240]
    add x13, x10, #0x240
    mov x11, #0
3:  lsr x14, x2, x11
    tbz x14, #0, 4f
    ldr x15, [x13, x11, lsl #3]
    add x14, x12, x11
    cmp x15, x14
    b.eq 4f
    mov x14, #1
    lsl x14, x14, x11
    orr x0, x0, x14
4:  add x11, x11, #1
    cmp x11, #31
    b.lo 3b
    tbz x2, #31, 5f
    ldr x15, [x13, #248]
    LI32 x14, SP_TOP
    cmp x15, x14
    b.eq 5f
    orr x0, x0, #0x80000000
5:  str x0, [x9, #0x340]
    ret
    .endm

    /* recv-v2 on the cap literal `cap` into the receive buffer; blocks. Args only. */
    .macro RECV_ARGS cap
    ldr w0, \cap
    LI32 x1, PAYLOAD
    mov x2, #8
    LI32 x3, META
    mov x4, #0x28
    mov x5, #0
    mov x8, #NR_RECV
    .endm

    /* NR 6 on send cap `scap`, framed request at PAYLOAD+0x100, reply endpoint `rcap`. */
    .macro CALL_ARGS scap, rcap
    ldr w0, \scap
    LI32 x1, (PAYLOAD + 0x100)
    mov x2, #10
    mov x3, #0
    mov x4, #0
    ldr w5, \rcap
    mov x8, #NR_CALL
    .endm

    /* NR 6 with a bounded WouldBlock retry (outside any window). */
    .macro CALL_RETRY scap, rcap, fail
    mov x20, #0
6:  add x20, x20, #1
    CALL_ARGS \scap, \rcap
    svc #0
    cbz x0, 7f
    cmp x0, #7
    b.ne \fail
    cmp x20, #64
    b.hs \fail
    mov x8, #NR_YIELD
    svc #0
    DELAY 0x10000
    b 6b
7:
    .endm

    /* NR 7 on the receiver-local Reply cap the metadata carries, bounded WouldBlock retry. */
    .macro REPLY_RETRY fail
    LI32 x21, (META + 0x10)
    ldr w21, [x21]
    mov x20, #0
6:  add x20, x20, #1
    mov x0, x21
    LI32 x1, REPLY_SRC
    mov x2, #8
    mov x3, #0
    mov x4, #0
    mov x5, #-1
    mov x8, #NR_REPLY
    svc #0
    cbz x0, 7f
    cmp x0, #7
    b.ne \fail
    cmp x20, #64
    b.hs \fail
    DELAY 0x10000
    b 6b
7:
    .endm

    /* The receive buffer must hold `lit` (8 bytes, little-endian literal word). */
    .macro EXPECT_PAYLOAD lit, fail
    LI32 x16, PAYLOAD
    ldr x17, [x16]
    ldr x16, \lit
    cmp x16, x17
    b.ne \fail
    .endm

    .macro ZERO_PAYLOAD
    LI32 x16, PAYLOAD
    str xzr, [x16]
    .endm

    /* NR 13: a fresh anonymous page at W in the caller's own address space, then `old`. */
    .macro MAKE_W old, fail
    mov x0, #0
    LI32 x1, W_VA
    mov x2, #0x1000
    mov x3, #3
    mov x4, #0
    mov x5, #0
    mov x8, #NR_ANON
    svc #0
    LI32 x16, W_VA
    cmp x0, x16
    b.ne \fail
    LI64 x17, \old
    str x17, [x16]
    .endm

    /* NR 3: a fresh anonymous page at W in the address space the cap literal names. */
    .macro REPLACE_W ascap, fail
    ldr w0, \ascap
    LI32 x1, W_VA
    mov x2, #0x1000
    mov x3, #3
    mov x4, #0
    mov x5, #0
    mov x8, #NR_VMMAP
    svc #0
    LI32 x16, W_VA
    cmp x0, x16
    b.ne \fail
    .endm

    /*
     * The TLB rounds and the mutual rounds, shared by S and C.
     *   pfx     label prefix            old      this task's W sentinel
     *   parity  1 = target in odd rounds, 0 = in even ones
     *   ascap   literal: cap to the OTHER task's address space
     *   rdy/ordy nr3/onr3 chk/ochk   this task's / the other task's mutual MBX words
     */
    .macro ROUNDS pfx, old, parity, ascap, rdy, ordy, nr3, onr3, chk, ochk
    RND_SET 0
\pfx\()_round:
    RND_INC
    RND_GET x20
    cmp x20, #TLB_ROUNDS
    b.hi \pfx\()_mutual
    and x21, x20, #1
    cmp x21, #\parity
    b.eq \pfx\()_target

    /* ── requester of round x20 ── */
    MWAITR M_PRIMED, x20
    mov x2, x20
    LOG \pfx\()_m_arm
    REPLACE_W \ascap, \pfx\()_vmmap_fail
    RND_GET x20
    MSETR M_DONE, x20
    mov x2, x20
    LOG \pfx\()_m_req_done
    RND_GET x20
    MWAITR M_OBS, x20
    b \pfx\()_round

    /* ── resident target of round x20 ── */
\pfx\()_target:
    /* Prime W with this round's sentinel and R, through this CPU's current translations. */
    LI32 x16, W_VA
    LI64 x17, \old
    eor x17, x17, x20
    str x17, [x16]
    ldr x17, [x16]
    LI32 x21, (PAT + 0x300)
    str x17, [x21, #8]
    LI32 x16, R_VA
    ldr x17, [x16]
    str x17, [x21, #16]
    /* The window's loop address, built before the context load clobbers x20. */
    LI32 x16, (MBX + M_DONE)
    add x16, x16, x20, lsl #3
    str x16, [x21, #0x20]
    mov x2, x20
    LOG \pfx\()_m_primed
    RND_GET x20
    MSETR M_PRIMED, x20
    LOAD_CTX
    .global yarm_smp2_\pfx\()_tlb_win_begin
yarm_smp2_\pfx\()_tlb_win_begin:
    LI32 x16, (PAT + 0x320)
    ldr x16, [x16]
1:  ldr x17, [x16]
    cbz x17, 1b
    .global yarm_smp2_\pfx\()_tlb_win_end
yarm_smp2_\pfx\()_tlb_win_end:
    CAPTURE
    CHECK WIN_MASK, \pfx\()_resident_fail
    /* W must now read the replacement's zero page; R must still read the primed value. */
    LI32 x16, W_VA
    ldr x3, [x16]
    LI32 x16, R_VA
    ldr x4, [x16]
    LI32 x21, (PAT + 0x300)
    ldr x17, [x21, #16]
    cmp x4, x17
    b.ne \pfx\()_residency_lost
    ldr x17, [x21, #8]
    cmp x3, x17
    b.eq \pfx\()_stale
    cbnz x3, \pfx\()_unexpected
    RND_GET x2
    LOG \pfx\()_m_observed
    RND_GET x20
    MSETR M_OBS, x20
    b \pfx\()_round

    /* ── mutual rounds ── */
\pfx\()_mutual:
    RND_SET 0
\pfx\()_mloop:
    RND_INC
    RND_GET x20
    cmp x20, #MUT_ROUNDS
    b.hi \pfx\()_fin
    /* Prime W with this round's sentinel. */
    LI32 x16, W_VA
    LI64 x17, \old
    eor x17, x17, x20, lsl #8
    str x17, [x16]
    ldr x17, [x16]
    LI32 x21, (PAT + 0x300)
    str x17, [x21, #8]
    MSETR \rdy, x20
    MWAITR \ordy, x20
    mov x2, x20
    LOG \pfx\()_m_mut_nr3
    REPLACE_W \ascap, \pfx\()_vmmap_fail
    RND_GET x20
    MSETR \nr3, x20
    MWAITR \onr3, x20
    /* The other NR 3 has returned: its invalidation of THIS task's W is complete. */
    LI32 x16, W_VA
    ldr x3, [x16]
    cbz x3, 2f
    LI32 x21, (PAT + 0x300)
    ldr x17, [x21, #8]
    cmp x3, x17
    b.eq \pfx\()_mstale
    b \pfx\()_unexpected
2:  mov x2, x20
    LOG \pfx\()_m_mut_ok
    RND_GET x20
    MSETR \chk, x20
    MWAITR \ochk, x20
    b \pfx\()_mloop
\pfx\()_mstale:
    mov x2, x20
    LOG \pfx\()_m_mut_stale
    b \pfx\()_park
\pfx\()_residency_lost:
    RND_GET x2
    LOG \pfx\()_m_lost
    b \pfx\()_park
\pfx\()_stale:
    RND_GET x2
    LOG \pfx\()_m_stale
    b \pfx\()_park
\pfx\()_resident_fail:
    RND_GET x2
    LI32 x16, (PAT + 0x340)
    ldr x3, [x16]
    LOG \pfx\()_m_resident_fail
    b \pfx\()_park
\pfx\()_unexpected:
    RND_GET x2
    LOG \pfx\()_m_unexpected
    b \pfx\()_park
\pfx\()_vmmap_fail:
    mov x3, x0
    RND_GET x2
    LOG \pfx\()_m_vmmap_fail
    b \pfx\()_park
\pfx\()_resident_fail_check:
    CHECK_BODY
    .endm

    .macro SMP2STR name, text
\name: .ascii "\text"
\name\()_end:
    .endm

/* ═════════════════════════════════ S: CPU 1, address space A ═════════════════════════════ */
    .set S_OLD_W, 0xA0A0A0A0A0A0A0A0

    .balign 16
    .global yarm_smp2_s_start
yarm_smp2_s_start:
    MAKE_W S_OLD_W, s_setup_fail
    LOG s_m_entered

    /* ── P1: S is the parked target of C's call, then replies to a parked C ── */
    RND_SET 0
s_p1_loop:
    RND_INC
    RND_GET x20
    cmp x20, #P1_ROUNDS
    b.hi s_p2a
    ZERO_PAYLOAD
    MSETR M_S_WAIT, x20
    LOAD_CTX
    RECV_ARGS yarm_smp2_s_cap_es
    svc #0
    CAPTURE
    CHECK SYS_MASK, s_p1_ctx_fail
    EXPECT_PAYLOAD s_req_lit, s_p1_payload_fail
    RND_GET x2
    LOG s_m_p1_resumed
    /* Reply only once C is about to block on its reply endpoint. */
    RND_GET x20
    MWAITR M_C_WAIT, x20
    DELAY 0x100000
    RND_GET x2
    LOG s_m_p1_reply
    REPLY_RETRY s_reply_fail
    b s_p1_loop

    /* ── P2 A: S is RESIDENT in EL0 while C's call wakes H1 on this CPU ── */
s_p2a:
    LOAD_CTX
    .global yarm_smp2_s_win_a_begin
yarm_smp2_s_win_a_begin:
    LI32 x16, (MBX + M_A_OPEN)
    mov x17, #1
    str x17, [x16]
    LI32 x16, (MBX + M_A_DONE)
1:  ldr x17, [x16]
    cbz x17, 1b
    .global yarm_smp2_s_win_a_end
yarm_smp2_s_win_a_end:
    CAPTURE
    CHECK WIN_MASK, s_win_fail
    LOG s_m_win_a_ok
    /* Let the woken H1 run; it replies to C and blocks again, then this task resumes. */
    mov x8, #NR_YIELD
    svc #0

    /* ── P2 B: C is RESIDENT in EL0 while this call wakes H0 on CPU 0 ── */
    MWAIT M_B_OPEN
    LOG s_m_p2b_call
    CALL_RETRY yarm_smp2_s_cap_eh0, yarm_smp2_s_cap_rs, s_call_fail
    DELAY 0x100000
    MSET M_B_DONE
    ZERO_PAYLOAD
    RECV_ARGS yarm_smp2_s_cap_rs
    svc #0
    EXPECT_PAYLOAD s_reply_lit, s_p2b_payload_fail
    LOG s_m_p2b_reply_ok

    ROUNDS s, S_OLD_W, 1, yarm_smp2_s_cap_as, M_READY_S, M_READY_C, M_NR3_S, M_NR3_C, M_CHK_S, M_CHK_C
s_fin:
    LOG s_m_done
s_park:
    RECV_ARGS yarm_smp2_s_cap_es
    svc #0
    LOG s_m_again
    b s_park
s_setup_fail:
    LOG s_m_setup_fail
    b s_park
s_p1_ctx_fail:
    RND_GET x2
    LI32 x16, (PAT + 0x340)
    ldr x3, [x16]
    LOG s_m_p1_ctx_fail
    b s_park
s_p1_ctx_fail_check:
    CHECK_BODY
s_win_fail:
    LI32 x16, (PAT + 0x340)
    ldr x3, [x16]
    LOG s_m_win_fail
    b s_park
s_win_fail_check:
    CHECK_BODY
s_p1_payload_fail:
s_p2b_payload_fail:
    LOG s_m_payload_fail
    b s_park
s_reply_fail:
s_call_fail:
    mov x3, x0
    LOG s_m_ipc_fail
    b s_park

    .balign 8
s_req_lit: .ascii "NR6-REQ!"
s_reply_lit: .ascii "RPLY-OK!"
    .global yarm_smp2_s_cap_es
yarm_smp2_s_cap_es: .word 0x51515151
    .global yarm_smp2_s_cap_eh0
yarm_smp2_s_cap_eh0: .word 0x52525252
    .global yarm_smp2_s_cap_rs
yarm_smp2_s_cap_rs: .word 0x53535353
    .global yarm_smp2_s_cap_as
yarm_smp2_s_cap_as: .word 0x54545454

    SMP2STR s_m_entered, "SMP2 S_ENTERED"
    SMP2STR s_m_p1_resumed, "SMP2 S_P1_RESUMED"
    SMP2STR s_m_p1_reply, "SMP2 S_P1_REPLY"
    SMP2STR s_m_win_a_ok, "SMP2 S_WIN_A_OK"
    SMP2STR s_m_p2b_call, "SMP2 S_P2B_CALL"
    SMP2STR s_m_p2b_reply_ok, "SMP2 S_P2B_REPLY_OK"
    SMP2STR s_m_arm, "SMP2 S_ARM_R"
    SMP2STR s_m_req_done, "SMP2 S_REQ_DONE"
    SMP2STR s_m_primed, "SMP2 S_PRIMED"
    SMP2STR s_m_observed, "SMP2 S_OBSERVED"
    SMP2STR s_m_mut_nr3, "SMP2 S_MUT_NR3"
    SMP2STR s_m_mut_ok, "SMP2 S_MUT_OK"
    SMP2STR s_m_done, "SMP2 S_DONE"
    SMP2STR s_m_again, "SMP2 S_FAIL_RECV_AGAIN"
    SMP2STR s_m_setup_fail, "SMP2 S_FAIL_SETUP"
    SMP2STR s_m_p1_ctx_fail, "SMP2 S_FAIL_P1_CONTEXT"
    SMP2STR s_m_win_fail, "SMP2 S_FAIL_WINDOW_A_CONTEXT"
    SMP2STR s_m_payload_fail, "SMP2 S_FAIL_PAYLOAD"
    SMP2STR s_m_ipc_fail, "SMP2 S_FAIL_IPC"
    SMP2STR s_m_mut_stale, "SMP2 S_FAIL_MUT_STALE"
    SMP2STR s_m_lost, "SMP2 S_FAIL_RESIDENCY_LOST"
    SMP2STR s_m_stale, "SMP2 S_FAIL_STALE_AFTER_INVALIDATION"
    SMP2STR s_m_resident_fail, "SMP2 S_FAIL_TLB_WINDOW_CONTEXT"
    SMP2STR s_m_unexpected, "SMP2 S_FAIL_UNEXPECTED_W"
    SMP2STR s_m_vmmap_fail, "SMP2 S_FAIL_VM_MAP"
    .balign 16
    .global yarm_smp2_s_end
yarm_smp2_s_end:

/* ═════════════════════════════════ C: CPU 0, address space B ═════════════════════════════ */
    .set C_OLD_W, 0xB0B0B0B0B0B0B0B0

    .balign 16
    .global yarm_smp2_c_start
yarm_smp2_c_start:
    MAKE_W C_OLD_W, c_setup_fail
    LOG c_m_entered
    /* Let the boot settle: a deadline receive on the reply endpoint nobody sends to yet. */
    ldr w0, yarm_smp2_c_cap_rc
    LI32 x1, PAYLOAD
    mov x2, #8
    mov x3, #PARK_TICKS
    LI32 x4, META
    mov x5, #0x28
    mov x8, #NR_RECV_TO
    svc #0

    /* ── P1: C calls a parked S, then is the parked target of S's reply ── */
    RND_SET 0
c_p1_loop:
    RND_INC
    RND_GET x20
    cmp x20, #P1_ROUNDS
    b.hi c_p2a
    /* Call only once S is about to block on its endpoint. */
    MWAITR M_S_WAIT, x20
    DELAY 0x100000
    RND_GET x2
    LOG c_m_p1_call
    CALL_RETRY yarm_smp2_c_cap_es, yarm_smp2_c_cap_rc, c_call_fail
    ZERO_PAYLOAD
    RND_GET x20
    MSETR M_C_WAIT, x20
    LOAD_CTX
    RECV_ARGS yarm_smp2_c_cap_rc
    svc #0
    CAPTURE
    CHECK SYS_MASK, c_p1_ctx_fail
    EXPECT_PAYLOAD c_reply_lit, c_payload_fail
    RND_GET x2
    LOG c_m_p1_resumed
    b c_p1_loop

    /* ── P2 A: this call wakes H1 on CPU 1 while S is resident there in EL0 ── */
c_p2a:
    MWAIT M_A_OPEN
    LOG c_m_p2a_call
    CALL_RETRY yarm_smp2_c_cap_eh1, yarm_smp2_c_cap_rc, c_call_fail
    DELAY 0x100000
    MSET M_A_DONE
    ZERO_PAYLOAD
    RECV_ARGS yarm_smp2_c_cap_rc
    svc #0
    EXPECT_PAYLOAD c_reply_lit, c_payload_fail
    LOG c_m_p2a_reply_ok

    /* ── P2 B: C is RESIDENT in EL0 while S's call wakes H0 on this CPU ── */
    LOAD_CTX
    .global yarm_smp2_c_win_b_begin
yarm_smp2_c_win_b_begin:
    LI32 x16, (MBX + M_B_OPEN)
    mov x17, #1
    str x17, [x16]
    LI32 x16, (MBX + M_B_DONE)
1:  ldr x17, [x16]
    cbz x17, 1b
    .global yarm_smp2_c_win_b_end
yarm_smp2_c_win_b_end:
    CAPTURE
    CHECK WIN_MASK, c_win_fail
    LOG c_m_win_b_ok
    mov x8, #NR_YIELD
    svc #0

    ROUNDS c, C_OLD_W, 0, yarm_smp2_c_cap_as, M_READY_C, M_READY_S, M_NR3_C, M_NR3_S, M_CHK_C, M_CHK_S
c_fin:
    LOG c_m_done
c_park:
    RECV_ARGS yarm_smp2_c_cap_rc
    svc #0
    b c_park
c_setup_fail:
    LOG c_m_setup_fail
    b c_park
c_p1_ctx_fail:
    RND_GET x2
    LI32 x16, (PAT + 0x340)
    ldr x3, [x16]
    LOG c_m_p1_ctx_fail
    b c_park
c_p1_ctx_fail_check:
    CHECK_BODY
c_win_fail:
    LI32 x16, (PAT + 0x340)
    ldr x3, [x16]
    LOG c_m_win_fail
    b c_park
c_win_fail_check:
    CHECK_BODY
c_payload_fail:
    LOG c_m_payload_fail
    b c_park
c_call_fail:
    mov x3, x0
    LOG c_m_ipc_fail
    b c_park

    .balign 8
c_reply_lit: .ascii "RPLY-OK!"
    .global yarm_smp2_c_cap_es
yarm_smp2_c_cap_es: .word 0x61616161
    .global yarm_smp2_c_cap_eh1
yarm_smp2_c_cap_eh1: .word 0x62626262
    .global yarm_smp2_c_cap_rc
yarm_smp2_c_cap_rc: .word 0x63636363
    .global yarm_smp2_c_cap_as
yarm_smp2_c_cap_as: .word 0x64646464

    SMP2STR c_m_entered, "SMP2 C_ENTERED"
    SMP2STR c_m_p1_call, "SMP2 C_P1_CALL"
    SMP2STR c_m_p1_resumed, "SMP2 C_P1_RESUMED"
    SMP2STR c_m_p2a_call, "SMP2 C_P2A_CALL"
    SMP2STR c_m_p2a_reply_ok, "SMP2 C_P2A_REPLY_OK"
    SMP2STR c_m_win_b_ok, "SMP2 C_WIN_B_OK"
    SMP2STR c_m_arm, "SMP2 C_ARM_R"
    SMP2STR c_m_req_done, "SMP2 C_REQ_DONE"
    SMP2STR c_m_primed, "SMP2 C_PRIMED"
    SMP2STR c_m_observed, "SMP2 C_OBSERVED"
    SMP2STR c_m_mut_nr3, "SMP2 C_MUT_NR3"
    SMP2STR c_m_mut_ok, "SMP2 C_MUT_OK"
    SMP2STR c_m_done, "SMP2 C_DONE"
    SMP2STR c_m_setup_fail, "SMP2 C_FAIL_SETUP"
    SMP2STR c_m_p1_ctx_fail, "SMP2 C_FAIL_P1_CONTEXT"
    SMP2STR c_m_win_fail, "SMP2 C_FAIL_WINDOW_B_CONTEXT"
    SMP2STR c_m_payload_fail, "SMP2 C_FAIL_PAYLOAD"
    SMP2STR c_m_ipc_fail, "SMP2 C_FAIL_IPC"
    SMP2STR c_m_mut_stale, "SMP2 C_FAIL_MUT_STALE"
    SMP2STR c_m_lost, "SMP2 C_FAIL_RESIDENCY_LOST"
    SMP2STR c_m_stale, "SMP2 C_FAIL_STALE_AFTER_INVALIDATION"
    SMP2STR c_m_resident_fail, "SMP2 C_FAIL_TLB_WINDOW_CONTEXT"
    SMP2STR c_m_unexpected, "SMP2 C_FAIL_UNEXPECTED_W"
    SMP2STR c_m_vmmap_fail, "SMP2 C_FAIL_VM_MAP"
    .balign 16
    .global yarm_smp2_c_end
yarm_smp2_c_end:

/* ══════════════════════════ H: a helper blocked on its own CPU ═══════════════════════════ */
    .balign 16
    .global yarm_smp2_h_start
yarm_smp2_h_start:
    LOG h_m_entered
h_loop:
    ZERO_PAYLOAD
    RECV_ARGS yarm_smp2_h_cap
    svc #0
    EXPECT_PAYLOAD h_req_lit, h_fail
    REPLY_RETRY h_fail
    LOG h_m_served
    b h_loop
h_fail:
    mov x3, x0
    LOG h_m_fail
h_park:
    RECV_ARGS yarm_smp2_h_cap
    svc #0
    b h_park
    .balign 8
h_req_lit: .ascii "NR6-REQ!"
    .global yarm_smp2_h_cap
yarm_smp2_h_cap: .word 0x71717171
    SMP2STR h_m_entered, "SMP2 H_ENTERED"
    SMP2STR h_m_served, "SMP2 H_SERVED"
    SMP2STR h_m_fail, "SMP2 H_FAIL"
    .balign 16
    .global yarm_smp2_h_end
yarm_smp2_h_end:

    .popsection
