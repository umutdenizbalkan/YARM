// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan
//! VFS-LIFETIME1 — the live file-grant lifetime witness (init, `vfs-lifetime-witness`).
//!
//! Every cycle goes through the production path: init asks PM (SpawnV5) for an image, PM opens it
//! through the resident VFS, asks for FILE_GRANT_RO, initramfs creates the slice MemoryObject
//! (NR 28) and replies with it, VFS relays it, PM spawns from it (NR 29). Each holder releases its
//! copy at its own commit point (NR 22). Between cycles init takes a ledger CHECKPOINT: a
//! `CapRelinquish(0)` — refused (`InvalidCapability`), it changes nothing — after which the
//! witness kernel prints `VL1_LEDGER event=nr22_refused tid=<init>` with the live slice objects,
//! their capability references, every process CNode's grant slots and the live delegation links.
//!
//! 1. `warm` — the checkpoint after the ordinary boot's own grant launches.
//! 2. `success_1..3` — three launches of `ramfs_srv` (image 11); each must return to `warm`.
//! 3. failed handoffs, through PM's witness image 14 (the 300-byte VFS fixture — a real grant of a
//!    file that is not an ELF), each followed by a checkpoint that must equal `warm`:
//!    * `hop1_undelivered` — initramfs replies through a stale reply capability, then `BACKEND`;
//!    * `hop1_rejected` — initramfs echoes the wrong fd; VFS rejects the completion it installed;
//!    * `hop2_undelivered` — VFS relays through a stale reply capability, then `BACKEND`;
//!    * `hop2_rejected` — VFS echoes the wrong fd; PM rejects the completion it installed;
//!    * `nr29_refused` — the grant reaches PM intact and NR 29 refuses it (not an ELF).
//! 4. `holder_exit` — a fork child obtains the fixture's grant itself and exits holding it; the
//!    kernel's teardown is the release.
//! 5. `after` — one more ordinary launch: the path still works after every failure.
//!
//! Survival is the kernel's `VL1_GRANT_USE` line at each NR 29: the grant's capability count at
//! the moment PM uses it (1 = PM's own copy only — initramfs and VFS have already released theirs,
//! because PM's fd CLOSE, which follows the grant, is served by VFS and initramfs after their
//! releases). The grader (`scripts/grade-vfs-lifetime-witness.py`) checks both halves.

use yarm_user_rt::vfs_client::{vfs_close, vfs_file_grant_ro, vfs_openat};

const IMAGE_RAMFS: u64 = 11;
const IMAGE_FIXTURE: u64 = 14;
const FIXTURE: &[u8] = b"/initramfs/etc/vfs-fixture";
const FIXTURE_LEN: u64 = 300;
const SUCCESS_CYCLES: u32 = 3;
/// How long the parent sleeps (CLOCK_MONOTONIC) while the fork child runs and exits. The child
/// inherits init's reply endpoint, so the parent makes no request in that window.
const CHILD_WINDOW_NS: u64 = 3_000_000_000;
const GEN_WINDOW: u64 = 64;

fn checkpoint(tag: &str) {
    let r = yarm_user_rt::syscall::cap_relinquish(0);
    yarm_user_rt::user_log!(
        "VL1_CHECKPOINT tag={} probe={}",
        tag,
        match r {
            Err(yarm_user_rt::syscall::SyscallError::InvalidCapability) => "refused_invalid",
            Err(_) => "refused_other",
            Ok(_) => "unexpected_ok",
        }
    );
}

fn launch(pm_send: u64, pm_recv: u64, image: u64, label: &str) -> bool {
    let r = super::spawn_v5_cap(pm_send, pm_recv, image, [0, 0, 0, 0], 1);
    let tid = r.map(|(t, _)| t).unwrap_or(0);
    yarm_user_rt::user_log!(
        "VL1_CYCLE label={} image_id={} spawned={} tid={}",
        label,
        image,
        (tid != 0) as u8,
        tid
    );
    tid != 0
}

// ── the fork child's capabilities (the fd witness's rank mapping) ─────────────────────────────

/// Is `cap` a live capability of this task? NR 21 op 0 answers `WrongObject` for any capability
/// that is not a reply capability and `InvalidCapability` for an empty slot or a stale
/// generation; it mutates nothing.
fn occupied(cap: u64) -> bool {
    matches!(
        yarm_user_rt::syscall::ipc_caller_session(cap),
        Ok(_) | Err(yarm_user_rt::syscall::SyscallError::WrongObject)
    )
}

fn parent_rank(cap: u64, g0: u64) -> usize {
    let idx = cap & 0xFFFF;
    (0..idx)
        .filter(|&i| (g0..g0 + GEN_WINDOW).any(|g| occupied((g << 16) | i)))
        .count()
}

fn child_cap_at_rank(rank: usize, g0: u64) -> Option<u64> {
    let mut n = 0;
    for i in 0..1024u64 {
        let cap = (g0 << 16) | i;
        if occupied(cap) {
            if n == rank {
                return Some(cap);
            }
            n += 1;
        }
    }
    None
}

fn exit_now() -> ! {
    // SAFETY: no pointer arguments; this does not return on success.
    let _ = unsafe { yarm_user_rt::syscall::exit_current_task() };
    loop {
        let _ = yarm_user_rt::syscall::yield_now();
    }
}

/// A fork child obtains the fixture's grant through VFS and exits holding it.
fn holder_exit(vfs: u64, reply: u64) -> bool {
    let g0 = reply >> 16;
    let rank_vfs = parent_rank(vfs, g0);
    let rank_reply = parent_rank(reply, g0);
    // SAFETY: the existing NR 12 wrapper; the child exits below and never returns into init.
    let fr = unsafe { yarm_user_rt::syscall::fork_raw() };
    if fr.ret0 == 0 && fr.err != 0 && fr.err < 0x100 {
        yarm_user_rt::user_log!("VL1_FORK failed err={}", fr.err);
        return false;
    }
    if fr.ret0 == 0 {
        // ── CHILD ──
        let (Some(vfs), Some(reply)) = (
            child_cap_at_rank(rank_vfs, g0),
            child_cap_at_rank(rank_reply, g0),
        ) else {
            yarm_user_rt::user_log!("VL1_CHILD_EXIT holding=0 reason=no_caps");
            exit_now();
        };
        // SAFETY: the child's own copies of init's VFS SEND capability and reply endpoint.
        let fd = unsafe { vfs_openat(vfs, reply, FIXTURE, 0) };
        let grant = match fd {
            // SAFETY: as above.
            Ok(fd) => unsafe { vfs_file_grant_ro(vfs, reply, fd) },
            Err(e) => Err(e),
        };
        if let Ok(fd) = fd {
            // SAFETY: as above.
            let _ = unsafe { vfs_close(vfs, reply, fd) };
        }
        match grant {
            Ok((len, cap)) => {
                yarm_user_rt::user_log!(
                    "VL1_CHILD_HOLDS cap={} len={} len_ok={}",
                    cap,
                    len,
                    (len == FIXTURE_LEN) as u8
                );
                yarm_user_rt::user_log!("VL1_CHILD_EXIT holding=1");
            }
            Err(e) => {
                yarm_user_rt::user_log!("VL1_CHILD_EXIT holding=0 reason={:?}", e);
            }
        }
        exit_now();
    }
    // ── PARENT: no request until the child has had its window. ──
    let slept = yarm_user_rt::syscall::nanosleep(
        &yarm_user_rt::syscall::Timespec::from_ns(CHILD_WINDOW_NS),
        None,
    );
    yarm_user_rt::user_log!(
        "VL1_FORK_PARENT_WAKE child={} slept={}",
        fr.ret0,
        slept.is_ok() as u8
    );
    true
}

// ── entry ─────────────────────────────────────────────────────────────────────────────────────

pub(super) fn run(vfs: u64, pm_send: u64, pm_recv: u64) -> bool {
    yarm_user_rt::user_log!(
        "VL1_BEGIN success_cycles={} fixture_len={}",
        SUCCESS_CYCLES,
        FIXTURE_LEN
    );
    checkpoint("warm");
    let mut ok = true;
    for n in 1..=SUCCESS_CYCLES {
        let label = match n {
            1 => "success_1",
            2 => "success_2",
            _ => "success_3",
        };
        ok &= launch(pm_send, pm_recv, IMAGE_RAMFS, label);
        checkpoint(label);
    }
    for label in [
        "hop1_undelivered",
        "hop1_rejected",
        "hop2_undelivered",
        "hop2_rejected",
        "nr29_refused",
    ] {
        // Every one of these must be refused: a spawn here is a witness failure.
        ok &= !launch(pm_send, pm_recv, IMAGE_FIXTURE, label);
        checkpoint(label);
    }
    ok &= holder_exit(vfs, pm_recv);
    checkpoint("holder_exit");
    ok &= launch(pm_send, pm_recv, IMAGE_RAMFS, "after");
    checkpoint("after");
    yarm_user_rt::user_log!("VL1_WITNESS_DONE driver_ok={}", ok as u8);
    ok
}
