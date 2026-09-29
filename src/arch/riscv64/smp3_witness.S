/* SPDX-License-Identifier: Apache-2.0 */
/* Copyright 2026 Umut Deniz Balkan */
/*
 * QEMU-SMP3 — the RISC-V two-hart IPI / remote-fence / context witness user programs.
 *
 * Four kernel-built tasks, each copied to 0x2000_0000 in its own address space:
 *   S  — server, logical CPU 1 (the secondary hart), address space A;
 *   C  — client, logical CPU 0 (the boot hart), address space B;
 *   H1 — helper blocked on CPU 1;  H0 — helper blocked on CPU 0 (one image, two caps).
 * Everything is position-independent: data is reached through absolute user VAs built with `li`,
 * strings through `lla`, capabilities through PC-relative literal words the kernel patches at
 * provisioning. Linker relaxation is off, so no `auipc` pair can be rewritten against `gp`.
 *
 * User VAs (S and C):
 *   0x2001_0ff0  SP (never moves)
 *   0x2003_0000  receive buffer; +0x100 the framed NR 6 request
 *   0x2004_0000  recv-v2 metadata (40 bytes)
 *   0x2005_0000  reply source "RPLY-OK!"
 *   0x2006_0000  PAT: the GPR pattern base P at +0; +0x300 round, +0x308 primed W value,
 *                +0x320 window loop address, +0x340 last check result, +0x400 capture (xN at 8*N)
 *   0x2007_0000  MBX, ONE frame shared by S and C
 *   0x2008_0000  W, created by each task through NR 13, replaced by the OTHER task through NR 3
 *
 * Phases (the kernel records every step; the programs only drive and check):
 *   P1  parked targets: C NR6 -> blocked S (CPU 1 parked) ; S NR7 -> blocked C (CPU 0 parked)
 *   P2  user targets:   C NR6 -> H1 while S spins in window A on CPU 1 (IPI taken from U-mode);
 *                       S NR6 -> H0 while C spins in window B on CPU 0 (IPI taken from U-mode)
 *   P3  serial remote-fence rounds: odd r C replaces S's W while S is resident; even r the mirror
 *   P4  mutual rounds: both replace each other's W at once
 *
 * Context windows load x1, x3..x29 (P + N) and compare them, and SP, after. t5/t6 (x30/x31) are
 * the window's own instrument (loop address and loaded word); a0..a5 and a7 are syscall lanes
 * across an `ecall`. There is no FP or vector state: user tasks run with FS = VS = Off.
 *
 * P3 residency: the target PRIMES W after its last kernel entry (the S_PRE / C_PRE step) and
 * enters nothing until its observation step, so the kernel can require that the hart's next
 * supervisor entry after PRE is exactly the OBSERVED step's own.
 */
    .pushsection .text.yarm_smp3_witness, "ax", @progbits
    .option push
    .option norelax

    .set PAYLOAD, 0x20030000
    .set META, 0x20040000
    .set REPLY_SRC, 0x20050000
    .set PAT, 0x20060000
    .set MBX, 0x20070000
    .set W_VA, 0x20080000
    .set SP_TOP, 0x20010ff0

    .set NR_YIELD, 0
    .set NR_RECV, 2
    .set NR_VMMAP, 3
    .set NR_RECV_TO, 5
    .set NR_CALL, 6
    .set NR_REPLY, 7
    .set NR_ANON, 13
    .set NR_LOG, 15

    /* GPR check masks: bit N = xN; bit 2 is SP, checked against SP_TOP. */
    .set WIN_MASK, 0x3FFFFFFE
    .set SYS_MASK, 0x3FFD03FE

    .set P1_ROUNDS, 4
    .set TLB_ROUNDS, 8
    .set MUT_ROUNDS, 4
    .set PARK_TICKS, 60

    /* MBX words; each per-round array is 16 words, index = round. */
    .set M_S_WAIT, 0x000
    .set M_C_WAIT, 0x080
    .set M_A_OPEN, 0x100
    .set M_A_DONE, 0x108
    .set M_B_OPEN, 0x110
    .set M_B_DONE, 0x118
    .set M_PRIMED, 0x180
    .set M_DONE, 0x200
    .set M_OBS, 0x280
    .set M_READY_S, 0x300
    .set M_READY_C, 0x380
    .set M_NR3_S, 0x400
    .set M_NR3_C, 0x480
    .set M_CHK_S, 0x500
    .set M_CHK_C, 0x580

    /* Long-range conditional branches (a B-type branch reaches only +-4 KiB). */
    .macro BNEZ_FAR reg, target
    beqz \reg, 8f
    j \target
8:
    .endm
    .macro BNE_FAR ra_, rb_, target
    beq \ra_, \rb_, 8f
    j \target
8:
    .endm
    .macro BEQ_FAR ra_, rb_, target
    bne \ra_, \rb_, 8f
    j \target
8:
    .endm

    /* DebugLog of one of this program's strings; a2 = round, a3 = aux. */
    .macro LOG msg
    lla a0, \msg
    lla a1, \msg\()_end
    sub a1, a1, a0
    li a7, NR_LOG
    ecall
    .endm

    /* MBX[off] = 1 */
    .macro MSET off
    li t6, (MBX + \off)
    li t5, 1
    sd t5, 0(t6)
    .endm

    /* MBX[off + 8 * reg] = 1 */
    .macro MSETR off, reg
    li t6, (MBX + \off)
    slli t5, \reg, 3
    add t6, t6, t5
    li t5, 1
    sd t5, 0(t6)
    .endm

    /* Spin until MBX[off] != 0. */
    .macro MWAIT off
    li t6, (MBX + \off)
2:  ld t5, 0(t6)
    beqz t5, 2b
    .endm

    /* Spin until MBX[off + 8 * reg] != 0. */
    .macro MWAITR off, reg
    li t6, (MBX + \off)
    slli t5, \reg, 3
    add t6, t6, t5
2:  ld t5, 0(t6)
    beqz t5, 2b
    .endm

    .macro DELAY n
    li t5, \n
9:  addi t5, t5, -1
    bnez t5, 9b
    .endm

    /* The round counter lives in the scratch word (every register is reloaded by a window). */
    .macro RND_SET val
    li t6, (PAT + 0x300)
    li t5, \val
    sd t5, 0(t6)
    .endm
    .macro RND_GET reg
    li t6, (PAT + 0x300)
    ld \reg, 0(t6)
    .endm
    .macro RND_INC
    li t6, (PAT + 0x300)
    ld t5, 0(t6)
    addi t5, t5, 1
    sd t5, 0(t6)
    .endm

    /* Load the checked context: xN = P + N for x1 and x3..x29; SP stays SP_TOP. */
    .macro LOAD_CTX
    li t6, PAT
    ld t5, 0(t6)
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
    .endm

    /* Capture x1..x29 and SP into PAT+0x400 before anything can change them (t6 only). */
    .macro CAPTURE
    li t6, (PAT + 0x400)
    sd x1, 8(t6)
    sd x2, 16(t6)
    sd x3, 24(t6)
    sd x4, 32(t6)
    sd x5, 40(t6)
    sd x6, 48(t6)
    sd x7, 56(t6)
    sd x8, 64(t6)
    sd x9, 72(t6)
    sd x10, 80(t6)
    sd x11, 88(t6)
    sd x12, 96(t6)
    sd x13, 104(t6)
    sd x14, 112(t6)
    sd x15, 120(t6)
    sd x16, 128(t6)
    sd x17, 136(t6)
    sd x18, 144(t6)
    sd x19, 152(t6)
    sd x20, 160(t6)
    sd x21, 168(t6)
    sd x22, 176(t6)
    sd x23, 184(t6)
    sd x24, 192(t6)
    sd x25, 200(t6)
    sd x26, 208(t6)
    sd x27, 216(t6)
    sd x28, 224(t6)
    sd x29, 232(t6)
    .endm

    /* Compare the capture with the pattern under `mask`; branch to `fail` on any difference. */
    .macro CHECK mask, fail
    li a2, \mask
    jal ra, \fail\()_check
    BNEZ_FAR a0, \fail
    .endm

    /* The comparison body: a2 = mask. a0 = mismatch bits (bit N = xN; bit 2 = SP). */
    .macro CHECK_BODY
    li t0, PAT
    ld t1, 0(t0)
    addi t2, t0, 0x400
    li a0, 0
    li t3, 1
7:  srl a3, a2, t3
    andi a3, a3, 1
    beqz a3, 9f
    slli a4, t3, 3
    add a4, t2, a4
    ld a4, 0(a4)
    add a5, t1, t3
    li a3, 2
    bne t3, a3, 8f
    li a5, SP_TOP
8:  beq a4, a5, 9f
    li a3, 1
    sll a3, a3, t3
    or a0, a0, a3
9:  addi t3, t3, 1
    li a3, 32
    bltu t3, a3, 7b
    sd a0, 0x340(t0)
    ret
    .endm

    /* recv-v2 on the cap literal `cap` into the receive buffer; blocks. Args only. */
    .macro RECV_ARGS cap
    lw a0, \cap
    li a1, PAYLOAD
    li a2, 8
    li a3, META
    li a4, 0x28
    li a5, 0
    li a7, NR_RECV
    .endm

    /* NR 6 on send cap `scap`, framed request at PAYLOAD+0x100, reply endpoint `rcap`. */
    .macro CALL_ARGS scap, rcap
    lw a0, \scap
    li a1, (PAYLOAD + 0x100)
    li a2, 10
    li a3, 0
    li a4, 0
    lw a5, \rcap
    li a7, NR_CALL
    .endm

    /* NR 6 with a bounded WouldBlock retry (outside any window). */
    .macro CALL_RETRY scap, rcap, fail
    li s4, 0
6:  addi s4, s4, 1
    CALL_ARGS \scap, \rcap
    ecall
    beqz a0, 5f
    li t5, 7
    BNE_FAR a0, t5, \fail
    li t5, 64
    bltu s4, t5, 3f
    j \fail
3:  li a7, NR_YIELD
    ecall
    DELAY 0x10000
    j 6b
5:
    .endm

    /* NR 7 on the receiver-local Reply cap the metadata carries, bounded WouldBlock retry. */
    .macro REPLY_RETRY fail
    li s5, (META + 0x10)
    lwu s5, 0(s5)
    li s4, 0
6:  addi s4, s4, 1
    mv a0, s5
    li a1, REPLY_SRC
    li a2, 8
    li a3, 0
    li a4, 0
    li a5, -1
    li a7, NR_REPLY
    ecall
    beqz a0, 5f
    li t5, 7
    BNE_FAR a0, t5, \fail
    li t5, 64
    bltu s4, t5, 3f
    j \fail
3:  DELAY 0x10000
    j 6b
5:
    .endm

    /* The receive buffer must hold `lit` (8 bytes, little-endian literal doubleword). */
    .macro EXPECT_PAYLOAD lit, fail
    li t6, PAYLOAD
    ld t5, 0(t6)
    ld t6, \lit
    BNE_FAR t5, t6, \fail
    .endm

    .macro ZERO_PAYLOAD
    li t6, PAYLOAD
    sd zero, 0(t6)
    .endm

    /* NR 13: a fresh anonymous page at W in the caller's own address space, then `old`. */
    .macro MAKE_W old, fail
    li a0, 0
    li a1, W_VA
    li a2, 0x1000
    li a3, 3
    li a4, 0
    li a5, 0
    li a7, NR_ANON
    ecall
    li t6, W_VA
    BNE_FAR a0, t6, \fail
    li t5, \old
    sd t5, 0(t6)
    .endm

    /* NR 3: a fresh anonymous page at W in the address space the cap literal names. */
    .macro REPLACE_W ascap, fail
    lw a0, \ascap
    li a1, W_VA
    li a2, 0x1000
    li a3, 3
    li a4, 0
    li a5, 0
    li a7, NR_VMMAP
    ecall
    li t6, W_VA
    BNE_FAR a0, t6, \fail
    .endm

    /*
     * The remote-fence rounds and the mutual rounds, shared by S and C.
     *   pfx     label prefix            old      this task's W sentinel
     *   parity  1 = target in odd rounds, 0 = in even ones
     *   ascap   literal: cap to the OTHER task's address space
     *   rdy/ordy nr3/onr3 chk/ochk   this task's / the other task's mutual MBX words
     */
    .macro ROUNDS pfx, old, parity, ascap, rdy, ordy, nr3, onr3, chk, ochk
    RND_SET 0
\pfx\()_round:
    RND_INC
    RND_GET s4
    li t5, TLB_ROUNDS
    bleu s4, t5, 1f
    j \pfx\()_mutual
1:  andi s5, s4, 1
    li t5, \parity
    bne s5, t5, 4f
    j \pfx\()_target
4:
    /* ── requester of round s4 ── */
    MWAITR M_PRIMED, s4
    mv a2, s4
    LOG \pfx\()_m_req
    REPLACE_W \ascap, \pfx\()_vmmap_fail
    RND_GET s4
    MSETR M_DONE, s4
    mv a2, s4
    LOG \pfx\()_m_req_done
    RND_GET s4
    MWAITR M_OBS, s4
    j \pfx\()_round

    /* ── resident target of round s4 ── */
\pfx\()_target:
    mv a2, s4
    LOG \pfx\()_m_pre
    /* Prime W through this hart's translation AFTER that last kernel entry: from here to the
       observation step nothing may enter the kernel on this hart. */
    RND_GET s4
    li t6, W_VA
    li t5, \old
    xor t5, t5, s4
    sd t5, 0(t6)
    ld t5, 0(t6)
    li s5, (PAT + 0x300)
    sd t5, 8(s5)
    /* The window's loop address, built before the context load clobbers s4. */
    li t6, (MBX + M_DONE)
    slli t4, s4, 3
    add t6, t6, t4
    sd t6, 0x20(s5)
    MSETR M_PRIMED, s4
    LOAD_CTX
    .global yarm_smp3_\pfx\()_tlb_win_begin
yarm_smp3_\pfx\()_tlb_win_begin:
    li t6, (PAT + 0x320)
    ld t6, 0(t6)
1:  ld t5, 0(t6)
    beqz t5, 1b
    .global yarm_smp3_\pfx\()_tlb_win_end
yarm_smp3_\pfx\()_tlb_win_end:
    CAPTURE
    CHECK WIN_MASK, \pfx\()_resident_fail
    /* W must now read the replacement's zero page. */
    li t6, W_VA
    ld a3, 0(t6)
    li s5, (PAT + 0x300)
    ld t5, 8(s5)
    BEQ_FAR a3, t5, \pfx\()_stale
    BNEZ_FAR a3, \pfx\()_unexpected
    RND_GET a2
    mv a3, t5
    LOG \pfx\()_m_observed
    RND_GET s4
    MSETR M_OBS, s4
    j \pfx\()_round

    /* ── mutual rounds ── */
\pfx\()_mutual:
    RND_SET 0
\pfx\()_mloop:
    RND_INC
    RND_GET s4
    li t5, MUT_ROUNDS
    bleu s4, t5, 1f
    j \pfx\()_fin
1:  li t6, W_VA
    li t5, \old
    slli t4, s4, 8
    xor t5, t5, t4
    sd t5, 0(t6)
    ld t5, 0(t6)
    li s5, (PAT + 0x300)
    sd t5, 8(s5)
    MSETR \rdy, s4
    MWAITR \ordy, s4
    mv a2, s4
    LOG \pfx\()_m_mut_nr3
    REPLACE_W \ascap, \pfx\()_vmmap_fail
    RND_GET s4
    MSETR \nr3, s4
    MWAITR \onr3, s4
    /* The other NR 3 has returned: the firmware fence of THIS task's W has completed. */
    li t6, W_VA
    ld a3, 0(t6)
    beqz a3, 4f
    li s5, (PAT + 0x300)
    ld t5, 8(s5)
    BEQ_FAR a3, t5, \pfx\()_mstale
    j \pfx\()_unexpected
4:  mv a2, s4
    LOG \pfx\()_m_mut_ok
    RND_GET s4
    MSETR \chk, s4
    MWAITR \ochk, s4
    j \pfx\()_mloop
\pfx\()_mstale:
    mv a2, s4
    LOG \pfx\()_m_mut_stale
    j \pfx\()_park
\pfx\()_stale:
    RND_GET a2
    LOG \pfx\()_m_stale
    j \pfx\()_park
\pfx\()_resident_fail:
    RND_GET a2
    li t6, (PAT + 0x340)
    ld a3, 0(t6)
    LOG \pfx\()_m_resident_fail
    j \pfx\()_park
\pfx\()_unexpected:
    RND_GET a2
    LOG \pfx\()_m_unexpected
    j \pfx\()_park
\pfx\()_vmmap_fail:
    mv a3, a0
    RND_GET a2
    LOG \pfx\()_m_vmmap_fail
    j \pfx\()_park
\pfx\()_resident_fail_check:
    CHECK_BODY
    .endm

    .macro SMP3STR name, text
\name: .ascii "\text"
\name\()_end:
    .endm

/* ═════════════════════════════════ S: CPU 1, address space A ═════════════════════════════ */
    .set S_OLD_W, 0xA0A0A0A0A0A0A0A0

    .balign 16
    .global yarm_smp3_s_start
yarm_smp3_s_start:
    MAKE_W S_OLD_W, s_setup_fail
    LOG s_m_entered

    /* ── P1: S is the parked target of C's call, then replies to a parked C ── */
    RND_SET 0
s_p1_loop:
    RND_INC
    RND_GET s4
    li t5, P1_ROUNDS
    bleu s4, t5, 1f
    j s_p2a
1:  ZERO_PAYLOAD
    MSETR M_S_WAIT, s4
    LOAD_CTX
    RECV_ARGS yarm_smp3_s_cap_es
    ecall
    CAPTURE
    CHECK SYS_MASK, s_p1_ctx_fail
    EXPECT_PAYLOAD s_req_lit, s_p1_payload_fail
    RND_GET a2
    LOG s_m_p1_resumed
    /* Reply only once C is about to block on its reply endpoint. */
    RND_GET s4
    MWAITR M_C_WAIT, s4
    DELAY 0x100000
    RND_GET a2
    LOG s_m_p1_reply
    REPLY_RETRY s_reply_fail
    j s_p1_loop

    /* ── P2 A: S is RESIDENT in U-mode while C's call wakes H1 on this CPU ── */
s_p2a:
    LOAD_CTX
    .global yarm_smp3_s_win_a_begin
yarm_smp3_s_win_a_begin:
    li t6, (MBX + M_A_OPEN)
    li t5, 1
    sd t5, 0(t6)
    li t6, (MBX + M_A_DONE)
1:  ld t5, 0(t6)
    beqz t5, 1b
    .global yarm_smp3_s_win_a_end
yarm_smp3_s_win_a_end:
    CAPTURE
    CHECK WIN_MASK, s_win_fail
    LOG s_m_win_a_ok
    /* Let the woken H1 run; it replies to C and blocks again, then this task resumes. */
    li a7, NR_YIELD
    ecall

    /* ── P2 B: C is RESIDENT in U-mode while this call wakes H0 on CPU 0 ── */
    MWAIT M_B_OPEN
    LOG s_m_p2b_call
    CALL_RETRY yarm_smp3_s_cap_eh0, yarm_smp3_s_cap_rs, s_call_fail
    DELAY 0x100000
    MSET M_B_DONE
    ZERO_PAYLOAD
    RECV_ARGS yarm_smp3_s_cap_rs
    ecall
    EXPECT_PAYLOAD s_reply_lit, s_p2b_payload_fail
    LOG s_m_p2b_reply_ok

    ROUNDS s, S_OLD_W, 1, yarm_smp3_s_cap_as, M_READY_S, M_READY_C, M_NR3_S, M_NR3_C, M_CHK_S, M_CHK_C
s_fin:
    LOG s_m_done
s_park:
    RECV_ARGS yarm_smp3_s_cap_es
    ecall
    LOG s_m_again
    j s_park
s_setup_fail:
    LOG s_m_setup_fail
    j s_park
s_p1_ctx_fail:
    RND_GET a2
    li t6, (PAT + 0x340)
    ld a3, 0(t6)
    LOG s_m_p1_ctx_fail
    j s_park
s_p1_ctx_fail_check:
    CHECK_BODY
s_win_fail:
    li t6, (PAT + 0x340)
    ld a3, 0(t6)
    LOG s_m_win_fail
    j s_park
s_win_fail_check:
    CHECK_BODY
s_p1_payload_fail:
s_p2b_payload_fail:
    LOG s_m_payload_fail
    j s_park
s_reply_fail:
s_call_fail:
    mv a3, a0
    LOG s_m_ipc_fail
    j s_park

    .balign 8
s_req_lit: .ascii "NR6-REQ!"
s_reply_lit: .ascii "RPLY-OK!"
    .global yarm_smp3_s_cap_es
yarm_smp3_s_cap_es: .word 0x51515151
    .global yarm_smp3_s_cap_eh0
yarm_smp3_s_cap_eh0: .word 0x52525252
    .global yarm_smp3_s_cap_rs
yarm_smp3_s_cap_rs: .word 0x53535353
    .global yarm_smp3_s_cap_as
yarm_smp3_s_cap_as: .word 0x54545454

    SMP3STR s_m_entered, "SMP3 S_ENTERED"
    SMP3STR s_m_p1_resumed, "SMP3 S_P1_RESUMED"
    SMP3STR s_m_p1_reply, "SMP3 S_P1_REPLY"
    SMP3STR s_m_win_a_ok, "SMP3 S_WIN_A_OK"
    SMP3STR s_m_p2b_call, "SMP3 S_P2B_CALL"
    SMP3STR s_m_p2b_reply_ok, "SMP3 S_P2B_REPLY_OK"
    SMP3STR s_m_req, "SMP3 S_REQ"
    SMP3STR s_m_req_done, "SMP3 S_REQ_DONE"
    SMP3STR s_m_pre, "SMP3 S_PRE"
    SMP3STR s_m_observed, "SMP3 S_OBSERVED"
    SMP3STR s_m_mut_nr3, "SMP3 S_MUT_NR3"
    SMP3STR s_m_mut_ok, "SMP3 S_MUT_OK"
    SMP3STR s_m_done, "SMP3 S_DONE"
    SMP3STR s_m_again, "SMP3 S_FAIL_RECV_AGAIN"
    SMP3STR s_m_setup_fail, "SMP3 S_FAIL_SETUP"
    SMP3STR s_m_p1_ctx_fail, "SMP3 S_FAIL_P1_CONTEXT"
    SMP3STR s_m_win_fail, "SMP3 S_FAIL_WINDOW_A_CONTEXT"
    SMP3STR s_m_payload_fail, "SMP3 S_FAIL_PAYLOAD"
    SMP3STR s_m_ipc_fail, "SMP3 S_FAIL_IPC"
    SMP3STR s_m_mut_stale, "SMP3 S_FAIL_MUT_STALE"
    SMP3STR s_m_stale, "SMP3 S_FAIL_STALE_AFTER_INVALIDATION"
    SMP3STR s_m_resident_fail, "SMP3 S_FAIL_TLB_WINDOW_CONTEXT"
    SMP3STR s_m_unexpected, "SMP3 S_FAIL_UNEXPECTED_W"
    SMP3STR s_m_vmmap_fail, "SMP3 S_FAIL_VM_MAP"
    .balign 16
    .global yarm_smp3_s_end
yarm_smp3_s_end:

/* ═════════════════════════════════ C: CPU 0, address space B ═════════════════════════════ */
    .set C_OLD_W, 0xB0B0B0B0B0B0B0B0

    .balign 16
    .global yarm_smp3_c_start
yarm_smp3_c_start:
    MAKE_W C_OLD_W, c_setup_fail
    LOG c_m_entered
    /* Let the boot settle: a deadline receive on the reply endpoint nobody sends to yet. */
    lw a0, yarm_smp3_c_cap_rc
    li a1, PAYLOAD
    li a2, 8
    li a3, PARK_TICKS
    li a4, META
    li a5, 0x28
    li a7, NR_RECV_TO
    ecall

    /* ── P1: C calls a parked S, then is the parked target of S's reply ── */
    RND_SET 0
c_p1_loop:
    RND_INC
    RND_GET s4
    li t5, P1_ROUNDS
    bleu s4, t5, 1f
    j c_p2a
1:  MWAITR M_S_WAIT, s4
    DELAY 0x100000
    RND_GET a2
    LOG c_m_p1_call
    CALL_RETRY yarm_smp3_c_cap_es, yarm_smp3_c_cap_rc, c_call_fail
    ZERO_PAYLOAD
    RND_GET s4
    MSETR M_C_WAIT, s4
    LOAD_CTX
    RECV_ARGS yarm_smp3_c_cap_rc
    ecall
    CAPTURE
    CHECK SYS_MASK, c_p1_ctx_fail
    EXPECT_PAYLOAD c_reply_lit, c_payload_fail
    RND_GET a2
    LOG c_m_p1_resumed
    j c_p1_loop

    /* ── P2 A: this call wakes H1 on CPU 1 while S is resident there in U-mode ── */
c_p2a:
    MWAIT M_A_OPEN
    LOG c_m_p2a_call
    CALL_RETRY yarm_smp3_c_cap_eh1, yarm_smp3_c_cap_rc, c_call_fail
    DELAY 0x100000
    MSET M_A_DONE
    ZERO_PAYLOAD
    RECV_ARGS yarm_smp3_c_cap_rc
    ecall
    EXPECT_PAYLOAD c_reply_lit, c_payload_fail
    LOG c_m_p2a_reply_ok

    /* ── P2 B: C is RESIDENT in U-mode while S's call wakes H0 on this CPU ── */
    LOAD_CTX
    .global yarm_smp3_c_win_b_begin
yarm_smp3_c_win_b_begin:
    li t6, (MBX + M_B_OPEN)
    li t5, 1
    sd t5, 0(t6)
    li t6, (MBX + M_B_DONE)
1:  ld t5, 0(t6)
    beqz t5, 1b
    .global yarm_smp3_c_win_b_end
yarm_smp3_c_win_b_end:
    CAPTURE
    CHECK WIN_MASK, c_win_fail
    LOG c_m_win_b_ok
    li a7, NR_YIELD
    ecall

    ROUNDS c, C_OLD_W, 0, yarm_smp3_c_cap_as, M_READY_C, M_READY_S, M_NR3_C, M_NR3_S, M_CHK_C, M_CHK_S
c_fin:
    LOG c_m_done
c_park:
    RECV_ARGS yarm_smp3_c_cap_rc
    ecall
    j c_park
c_setup_fail:
    LOG c_m_setup_fail
    j c_park
c_p1_ctx_fail:
    RND_GET a2
    li t6, (PAT + 0x340)
    ld a3, 0(t6)
    LOG c_m_p1_ctx_fail
    j c_park
c_p1_ctx_fail_check:
    CHECK_BODY
c_win_fail:
    li t6, (PAT + 0x340)
    ld a3, 0(t6)
    LOG c_m_win_fail
    j c_park
c_win_fail_check:
    CHECK_BODY
c_payload_fail:
    LOG c_m_payload_fail
    j c_park
c_call_fail:
    mv a3, a0
    LOG c_m_ipc_fail
    j c_park

    .balign 8
c_reply_lit: .ascii "RPLY-OK!"
    .global yarm_smp3_c_cap_es
yarm_smp3_c_cap_es: .word 0x61616161
    .global yarm_smp3_c_cap_eh1
yarm_smp3_c_cap_eh1: .word 0x62626262
    .global yarm_smp3_c_cap_rc
yarm_smp3_c_cap_rc: .word 0x63636363
    .global yarm_smp3_c_cap_as
yarm_smp3_c_cap_as: .word 0x64646464

    SMP3STR c_m_entered, "SMP3 C_ENTERED"
    SMP3STR c_m_p1_call, "SMP3 C_P1_CALL"
    SMP3STR c_m_p1_resumed, "SMP3 C_P1_RESUMED"
    SMP3STR c_m_p2a_call, "SMP3 C_P2A_CALL"
    SMP3STR c_m_p2a_reply_ok, "SMP3 C_P2A_REPLY_OK"
    SMP3STR c_m_win_b_ok, "SMP3 C_WIN_B_OK"
    SMP3STR c_m_req, "SMP3 C_REQ"
    SMP3STR c_m_req_done, "SMP3 C_REQ_DONE"
    SMP3STR c_m_pre, "SMP3 C_PRE"
    SMP3STR c_m_observed, "SMP3 C_OBSERVED"
    SMP3STR c_m_mut_nr3, "SMP3 C_MUT_NR3"
    SMP3STR c_m_mut_ok, "SMP3 C_MUT_OK"
    SMP3STR c_m_done, "SMP3 C_DONE"
    SMP3STR c_m_setup_fail, "SMP3 C_FAIL_SETUP"
    SMP3STR c_m_p1_ctx_fail, "SMP3 C_FAIL_P1_CONTEXT"
    SMP3STR c_m_win_fail, "SMP3 C_FAIL_WINDOW_B_CONTEXT"
    SMP3STR c_m_payload_fail, "SMP3 C_FAIL_PAYLOAD"
    SMP3STR c_m_ipc_fail, "SMP3 C_FAIL_IPC"
    SMP3STR c_m_mut_stale, "SMP3 C_FAIL_MUT_STALE"
    SMP3STR c_m_stale, "SMP3 C_FAIL_STALE_AFTER_INVALIDATION"
    SMP3STR c_m_resident_fail, "SMP3 C_FAIL_TLB_WINDOW_CONTEXT"
    SMP3STR c_m_unexpected, "SMP3 C_FAIL_UNEXPECTED_W"
    SMP3STR c_m_vmmap_fail, "SMP3 C_FAIL_VM_MAP"
    .balign 16
    .global yarm_smp3_c_end
yarm_smp3_c_end:

/* ══════════════════════════ H: a helper blocked on its own CPU ═══════════════════════════ */
    .balign 16
    .global yarm_smp3_h_start
yarm_smp3_h_start:
    LOG h_m_entered
h_loop:
    ZERO_PAYLOAD
    RECV_ARGS yarm_smp3_h_cap
    ecall
    EXPECT_PAYLOAD h_req_lit, h_fail
    REPLY_RETRY h_fail
    LOG h_m_served
    j h_loop
h_fail:
    mv a3, a0
    LOG h_m_fail
h_park:
    RECV_ARGS yarm_smp3_h_cap
    ecall
    j h_park
    .balign 8
h_req_lit: .ascii "NR6-REQ!"
    .global yarm_smp3_h_cap
yarm_smp3_h_cap: .word 0x71717171
    SMP3STR h_m_entered, "SMP3 H_ENTERED"
    SMP3STR h_m_served, "SMP3 H_SERVED"
    SMP3STR h_m_fail, "SMP3 H_FAIL"
    .balign 16
    .global yarm_smp3_h_end
yarm_smp3_h_end:

    .option pop
    .popsection
