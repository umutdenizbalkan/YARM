// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan
//! VFS-CORRECT1C — the live fd-namespace witness (init, `vfs-fd-witness`).
//!
//! Runs after init's ordinary `/ram` registration. It spawns a SECOND `ramfs_srv` and mounts it
//! at `/ram/b` through the production mount-administration client, so two backends answer from
//! identical handle counters. Each `ramfs_srv` (same feature) seeds `/ram/vc1c-file`,
//! `/ram/b/vc1c-file` and `/ram/vc1c-failreply`, 96 bytes each, `byte(i)` a function of the
//! SERVING instance's own TID — the bytes a client reads name the backend that served them — and
//! answers a STATX of `…/vc1c-stat` with `(next handle << 16) | open handles`, so this cell can
//! count backend handles in band. Everything goes through `vfs_client` and the resident VFS.
//!
//! 1. `collision` — the two counters are aligned through VFS (open+close on the lagging one), then
//!    one file is opened on each: same backend handle, two client fds; each reads its own bytes;
//!    one is closed and the other continues at its offset.
//! 2. `canonical` — STATX and OPENAT of non-canonical spellings reach the backend the canonical
//!    path names, with the bytes of that backend; mount-component boundaries.
//! 3. `full` — the session's descriptors are filled: the next OPENAT is `NO_FD` (9) and the
//!    backend's open count does not move.
//! 4. `failreply` — VFS answers this OPENAT through a stale reply capability first (VFS's
//!    `vfs-fd-witness` injection): the backend opened a handle (its next handle advanced) and the
//!    rollback closed it (its open count is back); init receives `BACKEND` (5).
//! 5. `thread` — a second thread opens and reads, then exits; the main thread reads on at the
//!    thread's offset and closes.
//! 6. `fork` — a fork child is a separate session: it opens two files on the second backend
//!    (getting the same fd numbers init holds), reads its own bytes, cannot use init's third fd,
//!    and exits without closing. VFS reclaims its descriptors and closes its two handles: the
//!    second backend's open count returns to where it was. Parent and child share init's one
//!    reply endpoint, so the parent sleeps (no VFS request) while the child runs.
//!
//! Each step logs `VC1C_*` lines; the seal is `VC1C_FD_WITNESS_SEAL`.

use core::sync::atomic::{AtomicU64, Ordering::SeqCst};
use yarm_fs_servers::ramfs::service::{RAMFS_FD_WITNESS_LEN, ramfs_fd_witness_byte};
use yarm_user_rt::vfs_client::{VfsClientError, vfs_close, vfs_openat, vfs_read, vfs_statx};

const FILE_A: &[u8] = b"/ram/vc1c-file";
const FILE_B: &[u8] = b"/ram/b/vc1c-file";
const FAILREPLY: &[u8] = b"/ram/vc1c-failreply";
const STAT_A: &[u8] = b"/ram/vc1c-stat";
const STAT_B: &[u8] = b"/ram/b/vc1c-stat";
const NO_FD: u32 = 9;
const BAD_FD: u32 = 4;
const BACKEND: u32 = 5;
const NO_MOUNT: u32 = 3;

struct Cell {
    vfs: u64,
    reply: u64,
    tid_a: u64,
    tid_b: u64,
    fails: u32,
}

/// ramfs's STATX value: the type in bits 60.., the size in bits 16..48, the mode below.
fn statx_size(v: u64) -> u64 {
    (v >> 16) & 0xFFFF_FFFF
}

fn status(r: &Result<u64, VfsClientError>) -> i64 {
    match r {
        Ok(_) => 0,
        Err(VfsClientError::Status(s)) => i64::from(*s),
        Err(_) => -1,
    }
}

impl Cell {
    fn check(&mut self, name: &str, ok: bool) {
        yarm_user_rt::user_log!("VC1C_CHECK name={} ok={}", name, ok as u8);
        if !ok {
            self.fails += 1;
        }
    }

    fn open(&self, path: &[u8]) -> Result<u64, VfsClientError> {
        // SAFETY: init's VFS SEND capability and its own reply endpoint.
        unsafe { vfs_openat(self.vfs, self.reply, path, 0) }
    }

    fn close(&self, fd: u64) -> Result<u64, VfsClientError> {
        // SAFETY: as in `open`.
        unsafe { vfs_close(self.vfs, self.reply, fd) }
    }

    fn statx(&self, path: &[u8]) -> Result<u64, VfsClientError> {
        // SAFETY: as in `open`.
        unsafe { vfs_statx(self.vfs, self.reply, path) }
    }

    /// `(next handle, open handles)` of the backend serving `path`.
    fn stat(&self, path: &[u8]) -> (u64, u64) {
        match self.statx(path) {
            Ok(v) => (v >> 16, v & 0xFFFF),
            Err(_) => (u64::MAX, u64::MAX),
        }
    }

    /// Read `n` bytes from `fd`; true iff exactly the bytes of `(tid, file)` at `off`.
    fn read_is(&self, fd: u64, n: usize, tid: u64, file: usize, off: usize) -> (bool, i64) {
        let mut buf = [0u8; 104];
        // SAFETY: as in `open`; `buf` is this frame's.
        match unsafe { vfs_read(self.vfs, self.reply, fd, &mut buf[..n]) } {
            Ok(got) => {
                let ok =
                    got == n && (0..n).all(|i| buf[i] == ramfs_fd_witness_byte(tid, file, off + i));
                (ok, got as i64)
            }
            Err(VfsClientError::Status(s)) => (false, -i64::from(s)),
            Err(_) => (false, -1000),
        }
    }

    fn read_status(&self, fd: u64) -> i64 {
        let mut buf = [0u8; 4];
        // SAFETY: as in `open`.
        match unsafe { vfs_read(self.vfs, self.reply, fd, &mut buf) } {
            Ok(_) => 0,
            Err(VfsClientError::Status(s)) => i64::from(s),
            Err(_) => -1,
        }
    }
}

// ── 1. collision ──────────────────────────────────────────────────────────────────────────────

fn collision(c: &mut Cell) {
    let (mut na, la) = c.stat(STAT_A);
    let (mut nb, lb) = c.stat(STAT_B);
    let mut aligned = 0;
    while na < nb && aligned < 64 {
        if let Ok(fd) = c.open(FILE_A) {
            let _ = c.close(fd);
        }
        na = c.stat(STAT_A).0;
        aligned += 1;
    }
    while nb < na && aligned < 64 {
        if let Ok(fd) = c.open(FILE_B) {
            let _ = c.close(fd);
        }
        nb = c.stat(STAT_B).0;
        aligned += 1;
    }
    yarm_user_rt::user_log!(
        "VC1C_ALIGN next_a={} next_b={} open_a={} open_b={} steps={}",
        na,
        nb,
        la,
        lb,
        aligned
    );
    c.check("collision_aligned", na == nb && na != u64::MAX);
    let fa = c.open(FILE_A);
    let fb = c.open(FILE_B);
    let (Ok(fa), Ok(fb)) = (fa, fb) else {
        yarm_user_rt::user_log!(
            "VC1C_COLLISION open_a={} open_b={}",
            status(&fa),
            status(&fb)
        );
        c.check("collision_open", false);
        return;
    };
    yarm_user_rt::user_log!(
        "VC1C_COLLISION client_fd_a={} client_fd_b={} backend_handle_a={} backend_handle_b={}",
        fa,
        fb,
        na,
        nb
    );
    c.check("collision_distinct_fds", fa != fb);
    let (ra, _) = c.read_is(fa, 32, c.tid_a, 0, 0);
    let (rb, _) = c.read_is(fb, 32, c.tid_b, 1, 0);
    let close_a = c.close(fa);
    let (rb2, _) = c.read_is(fb, 32, c.tid_b, 1, 32);
    let after = c.read_status(fa);
    let close_b = c.close(fb);
    yarm_user_rt::user_log!(
        "VC1C_COLLISION_RESULT read_a={} read_b={} close_a={} read_b_after={} read_a_after_close={} close_b={}",
        ra as u8,
        rb as u8,
        status(&close_a),
        rb2 as u8,
        after,
        status(&close_b)
    );
    c.check("collision_bytes_a", ra);
    c.check("collision_bytes_b", rb);
    c.check(
        "collision_close_one_keep_other",
        close_a.is_ok() && rb2 && after == i64::from(BAD_FD) && close_b.is_ok(),
    );
    let (_, la2) = c.stat(STAT_A);
    let (_, lb2) = c.stat(STAT_B);
    c.check("collision_backends_released", la2 == la && lb2 == lb);
}

// ── 2. canonical paths ────────────────────────────────────────────────────────────────────────

fn canonical(c: &mut Cell) {
    // (spelling, serving backend: 0 = /ram, 1 = /ram/b)
    const CASES: [(&[u8], usize); 8] = [
        (b"/ram//vc1c-file", 0),
        (b"/ram/./vc1c-file", 0),
        (b"/ram/b/../vc1c-file", 0),
        (b"/ram/b/x/../../vc1c-file", 0),
        (b"//ram/b//vc1c-file/", 1),
        (b"/ram/b/./vc1c-file", 1),
        (b"/initramfs/../ram/b/vc1c-file", 1),
        (b"/ram/./b/x/../vc1c-file", 1),
    ];
    for (path, which) in CASES {
        let tid = if which == 0 { c.tid_a } else { c.tid_b };
        let st = c.statx(path);
        let op = c.open(path);
        let (bytes, _) = match op {
            Ok(fd) => {
                let r = c.read_is(fd, 8, tid, which, 0);
                let _ = c.close(fd);
                r
            }
            Err(_) => (false, -1),
        };
        yarm_user_rt::user_log!(
            "VC1C_CANON path={} backend={} statx={} open={} bytes={}",
            core::str::from_utf8(path).unwrap_or("?"),
            which,
            st.as_ref()
                .map(|v| *v as i64)
                .unwrap_or_else(|_| -status(&st)),
            status(&op),
            bytes as u8
        );
        c.check(
            "canonical",
            st.as_ref().map(|v| statx_size(*v)) == Ok(RAMFS_FD_WITNESS_LEN as u64)
                && op.is_ok()
                && bytes,
        );
    }
    // The baseline's three initramfs spellings: refused at the backend before (status 2).
    for path in [
        &b"/initramfs//etc/vfs-fixture"[..],
        b"/initramfs/etc/./vfs-fixture",
        b"/ram/../initramfs/etc/vfs-fixture",
    ] {
        let st = c.statx(path);
        let op = c.open(path);
        if let Ok(fd) = op {
            let _ = c.close(fd);
        }
        yarm_user_rt::user_log!(
            "VC1C_CANON path={} backend=initramfs statx={} open={}",
            core::str::from_utf8(path).unwrap_or("?"),
            status(&st),
            status(&op)
        );
        c.check("canonical_initramfs", st.is_ok() && op.is_ok());
    }
    // Boundaries: /ram/bx is not under /ram/b/ (served by /ram, which has no such file); /ram/b
    // is the mount root; /rbx names no mount.
    let bx = c.statx(b"/ram/bx");
    let root_b = c.statx(b"/ram/b");
    let rbx = c.statx(b"/rbx");
    yarm_user_rt::user_log!(
        "VC1C_BOUNDARY ram_bx={} ram_b={} rbx={}",
        status(&bx),
        status(&root_b),
        status(&rbx)
    );
    c.check(
        "boundary",
        bx.is_err()
            && status(&bx) != i64::from(NO_MOUNT)
            && root_b.is_ok()
            && status(&rbx) == i64::from(NO_MOUNT),
    );
}

// ── 3. full table ─────────────────────────────────────────────────────────────────────────────

fn full(c: &mut Cell) {
    let (_, l0) = c.stat(STAT_A);
    let mut fds = [0u64; 24];
    let mut n = 0;
    let mut refused = 0i64;
    while n < fds.len() {
        match c.open(FILE_A) {
            Ok(fd) => {
                fds[n] = fd;
                n += 1;
            }
            Err(e) => {
                refused = status(&Err(e));
                break;
            }
        }
    }
    let (_, l1) = c.stat(STAT_A);
    let again = c.open(FILE_A);
    let (_, l2) = c.stat(STAT_A);
    let mut closed = 0;
    for fd in &fds[..n] {
        if c.close(*fd).is_ok() {
            closed += 1;
        }
    }
    let (_, l3) = c.stat(STAT_A);
    yarm_user_rt::user_log!(
        "VC1C_FULL opened={} refused_status={} again_status={} backend_open_before={} after_fill={} after_refusal={} after_close={} closed={}",
        n,
        refused,
        status(&again),
        l0,
        l1,
        l2,
        l3,
        closed
    );
    c.check(
        "full_no_fd",
        n == yarm_fs_servers::common::fd_space::MAX_FDS_PER_SESSION
            && refused == i64::from(NO_FD)
            && status(&again) == i64::from(NO_FD),
    );
    c.check(
        "full_nothing_opened_at_backend",
        l1 == l0 + n as u64 && l2 == l1 && l3 == l0 && closed == n,
    );
}

// ── 4. failed client reply ────────────────────────────────────────────────────────────────────

fn failreply(c: &mut Cell) {
    let (n0, l0) = c.stat(STAT_A);
    let r = c.open(FAILREPLY);
    let (n1, l1) = c.stat(STAT_A);
    yarm_user_rt::user_log!(
        "VC1C_FAILREPLY status={} backend_next_before={} after={} backend_open_before={} after={}",
        status(&r),
        n0,
        n1,
        l0,
        l1
    );
    c.check(
        "failreply_rolled_back",
        status(&r) == i64::from(BACKEND) && n1 == n0 + 1 && l1 == l0,
    );
}

// ── 5. threads of one session ─────────────────────────────────────────────────────────────────

const STACK_BYTES: usize = 16384;
#[repr(C, align(16))]
struct Stack([u8; STACK_BYTES]);
static mut T_STACK: Stack = Stack([0; STACK_BYTES]);
static mut T_TLS: [u8; 256] = [0; 256];
static T_VFS: AtomicU64 = AtomicU64::new(0);
static T_REPLY: AtomicU64 = AtomicU64::new(0);
static T_TID_A: AtomicU64 = AtomicU64::new(0);
static T_FD: AtomicU64 = AtomicU64::new(0);
static T_READ_OK: AtomicU64 = AtomicU64::new(0);
static T_DONE: AtomicU64 = AtomicU64::new(0);

extern "C" fn thread_body() -> ! {
    let c = Cell {
        vfs: T_VFS.load(SeqCst),
        reply: T_REPLY.load(SeqCst),
        tid_a: T_TID_A.load(SeqCst),
        tid_b: 0,
        fails: 0,
    };
    let fd = c.open(FILE_A).unwrap_or(u64::MAX);
    let (ok, _) = c.read_is(fd, 8, c.tid_a, 0, 0);
    yarm_user_rt::user_log!("VC1C_THREAD_OPENED fd={} read_ok={}", fd, ok as u8);
    T_FD.store(fd, SeqCst);
    T_READ_OK.store(ok as u64, SeqCst);
    T_DONE.store(1, SeqCst);
    yarm_user_rt::user_log!("VC1C_THREAD_EXIT");
    // SAFETY: no pointer arguments; ends only this thread.
    let _ = unsafe { yarm_user_rt::syscall::exit_current_task() };
    loop {
        let _ = yarm_user_rt::syscall::yield_now();
    }
}

// The kernel starts a thread with its 16-byte-aligned stack top in `rsp`. A Rust function on
// x86_64 expects to be entered by a `call` (`rsp ≡ 8 mod 16`), and its aligned SSE spills fault
// otherwise; this two-instruction trampoline makes that call. AArch64 and RISC-V enter directly.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".globl vc1c_fd_thread_trampoline",
    "vc1c_fd_thread_trampoline:",
    "call {body}",
    "ud2",
    body = sym thread_body,
);
#[cfg(target_arch = "x86_64")]
unsafe extern "C" {
    fn vc1c_fd_thread_trampoline() -> !;
}

fn thread_entry() -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        vc1c_fd_thread_trampoline as usize
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        thread_body as usize
    }
}

fn thread(c: &mut Cell) {
    T_VFS.store(c.vfs, SeqCst);
    T_REPLY.store(c.reply, SeqCst);
    T_TID_A.store(c.tid_a, SeqCst);
    let stack = core::ptr::addr_of_mut!(T_STACK) as usize;
    let tls = core::ptr::addr_of_mut!(T_TLS) as usize;
    let top = (stack + STACK_BYTES) & !0xF;
    // SAFETY: a valid entry; the static stack and TLS outlive the thread, which exits.
    let tid = unsafe { yarm_user_rt::syscall::spawn_thread(tls, top, thread_entry()) };
    let tick = yarm_user_rt::syscall::Timespec::from_ns(10_000_000);
    let mut waits = 0u32;
    while T_DONE.load(SeqCst) == 0 && waits < 3000 {
        let _ = yarm_user_rt::syscall::nanosleep(&tick, None);
        waits += 1;
    }
    // Let the thread's exit complete before the main thread uses its descriptor.
    let _ = yarm_user_rt::syscall::nanosleep(
        &yarm_user_rt::syscall::Timespec::from_ns(200_000_000),
        None,
    );
    let fd = T_FD.load(SeqCst);
    let (main_ok, _) = c.read_is(fd, 8, c.tid_a, 0, 8);
    let closed = c.close(fd);
    yarm_user_rt::user_log!(
        "VC1C_THREAD tid={} fd={} thread_read={} main_read_after_exit={} main_close={}",
        tid.unwrap_or(0),
        fd,
        T_READ_OK.load(SeqCst),
        main_ok as u8,
        status(&closed)
    );
    c.check(
        "thread_shares_session",
        tid.is_ok() && T_READ_OK.load(SeqCst) == 1 && main_ok && closed.is_ok(),
    );
}

// ── 6. a separate session: the fork child ─────────────────────────────────────────────────────

/// How long the parent sleeps (CLOCK_MONOTONIC) while the child runs. Init has one reply
/// endpoint, which the child inherits, so the two must not call VFS at once: the parent makes no
/// request until it wakes, and the grader checks that the child's exit came first.
const CHILD_WINDOW_NS: u64 = 3_000_000_000;
/// Generations a parent slot may have reached above the CNode's initial one.
const GEN_WINDOW: u64 = 64;

/// Is `cap` a live capability of this task? NR 21 op 0 answers `WrongObject` for any capability
/// that is not a reply capability and `InvalidCapability` for an empty slot or a stale
/// generation; it mutates nothing and logs nothing, so it doubles as a silent slot probe.
fn occupied(cap: u64) -> bool {
    matches!(
        yarm_user_rt::syscall::ipc_caller_session(cap),
        Ok(_) | Err(yarm_user_rt::syscall::SyscallError::WrongObject)
    )
}

/// A fork child's capabilities are the parent's inheritable ones, delegated in the parent's slot
/// order into a fresh CNode, so they get new numbers. The child finds its copy of a parent
/// capability by rank: how many live slots lie below it in the parent (init holds no
/// non-inheritable capability), and the same count of live slots in the child's own CNode, all at
/// the initial generation `g0`.
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

fn fork_session(c: &mut Cell) {
    let mut p = [0u64; 3];
    for slot in &mut p {
        *slot = c.open(FILE_A).unwrap_or(u64::MAX);
    }
    let (p_read0, _) = c.read_is(p[0], 8, c.tid_a, 0, 0);
    let (_, lb0) = c.stat(STAT_B);
    yarm_user_rt::user_log!(
        "VC1C_FORK_PARENT_HOLDS fds={},{},{} read={} open_b={}",
        p[0],
        p[1],
        p[2],
        p_read0 as u8,
        lb0
    );
    // init's PM reply endpoint was granted at boot into a fresh slot: its generation is the
    // CNode's initial one.
    let g0 = c.reply >> 16;
    let rank_vfs = parent_rank(c.vfs, g0);
    let rank_reply = parent_rank(c.reply, g0);
    yarm_user_rt::user_log!(
        "VC1C_FORK_CAPMAP parent_vfs={} rank_vfs={} parent_reply={} rank_reply={} g0={}",
        c.vfs,
        rank_vfs,
        c.reply,
        rank_reply,
        g0
    );
    // SAFETY: the existing NR 12 wrapper; the child exits below and never returns into init.
    let fr = unsafe { yarm_user_rt::syscall::fork_raw() };
    if fr.ret0 == 0 && fr.err != 0 && fr.err < 0x100 {
        yarm_user_rt::user_log!("VC1C_FORK failed err={}", fr.err);
        c.check("fork_session", false);
        for fd in p {
            let _ = c.close(fd);
        }
        return;
    }
    if fr.ret0 == 0 {
        // ── CHILD: a new process, so a new session with no descriptors. ──
        let vfs = child_cap_at_rank(rank_vfs, g0);
        let reply = child_cap_at_rank(rank_reply, g0);
        yarm_user_rt::user_log!(
            "VC1C_CHILD_CAPS vfs={} reply={}",
            vfs.unwrap_or(0),
            reply.unwrap_or(0)
        );
        let (Some(vfs), Some(reply)) = (vfs, reply) else {
            yarm_user_rt::user_log!("VC1C_CHILD_EXIT closed=0 reason=no_caps");
            // SAFETY: no pointer arguments; this does not return on success.
            let _ = unsafe { yarm_user_rt::syscall::exit_current_task() };
            loop {
                let _ = yarm_user_rt::syscall::yield_now();
            }
        };
        c.vfs = vfs;
        c.reply = reply;
        let c1 = c.open(FILE_B);
        let c2 = c.open(FILE_B);
        let (r1, _) = c.read_is(*c1.as_ref().unwrap_or(&u64::MAX), 8, c.tid_b, 1, 0);
        let parents_third = c.read_status(p[2]);
        let (_, lb1) = c.stat(STAT_B);
        yarm_user_rt::user_log!(
            "VC1C_CHILD fd1={} fd2={} read_own={} read_parents_fd={} parents_fd={} open_b={} open_b_before={}",
            c1.as_ref().map(|v| *v as i64).unwrap_or(-1),
            c2.as_ref().map(|v| *v as i64).unwrap_or(-1),
            r1 as u8,
            parents_third,
            p[2],
            lb1,
            lb0
        );
        yarm_user_rt::user_log!("VC1C_CHILD_EXIT closed=0");
        // SAFETY: no pointer arguments; this does not return on success.
        let _ = unsafe { yarm_user_rt::syscall::exit_current_task() };
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    }
    // ── PARENT: no VFS request until the child has had its window. ──
    let slept = yarm_user_rt::syscall::nanosleep(
        &yarm_user_rt::syscall::Timespec::from_ns(CHILD_WINDOW_NS),
        None,
    );
    yarm_user_rt::user_log!(
        "VC1C_FORK_PARENT_WAKE child={} slept={}",
        fr.ret0,
        slept.is_ok() as u8
    );
    // The first request after the child's exit makes VFS sweep: the child's session has no live
    // thread, so its two descriptors go and their two backend handles are closed.
    let (_, lb2) = c.stat(STAT_B);
    let (p_read1, _) = c.read_is(p[0], 8, c.tid_a, 0, 8);
    let mut closed = 0;
    for fd in p {
        if c.close(fd).is_ok() {
            closed += 1;
        }
    }
    yarm_user_rt::user_log!(
        "VC1C_FORK_PARENT child={} open_b_before={} open_b_after_child_exit={} parent_read_after={} parent_closed={}",
        fr.ret0,
        lb0,
        lb2,
        p_read1 as u8,
        closed
    );
    c.check("fork_session_own_files", p_read0 && p_read1 && closed == 3);
    c.check("fork_session_reclaimed_on_exit", lb2 == lb0);
}

// ── entry ─────────────────────────────────────────────────────────────────────────────────────

/// Spawn and mount the second ramfs, then run the cases. `tid_a` is the first ramfs's TID.
pub(super) fn run(
    vfs: u64,
    vfs_admin_send: Option<u64>,
    pm_send: u64,
    pm_recv: u64,
    tid_a: u64,
) -> bool {
    yarm_user_rt::user_log!("VC1C_BEGIN tid_a={}", tid_a);
    let Some((tid_b, cap_b)) = super::spawn_v5_cap(pm_send, pm_recv, 11, [0, 0, 0, 0], 1) else {
        yarm_user_rt::user_log!("VC1C_FD_WITNESS_SEAL result=fail reason=second_ramfs_spawn");
        return false;
    };
    let reg = super::register_mount_with_vfs(vfs, vfs_admin_send, pm_recv, cap_b, 0, b"/ram/b");
    yarm_user_rt::user_log!(
        "VC1C_SECOND_RAMFS tid_b={} init_local_backend={} mount_status={} reason={}",
        tid_b,
        cap_b,
        reg.status.map(i64::from).unwrap_or(-1),
        reg.reason
    );
    if reg.status != Some(yarm_ipc_abi::vfs_abi::VFS_MOUNT_STATUS_OK) {
        yarm_user_rt::user_log!("VC1C_FD_WITNESS_SEAL result=fail reason=second_mount");
        return false;
    }
    let mut c = Cell {
        vfs,
        reply: pm_recv,
        tid_a,
        tid_b,
        fails: 0,
    };
    let (na, la) = c.stat(STAT_A);
    let (nb, lb) = c.stat(STAT_B);
    yarm_user_rt::user_log!(
        "VC1C_BACKENDS next_a={} open_a={} next_b={} open_b={}",
        na,
        la,
        nb,
        lb
    );
    collision(&mut c);
    canonical(&mut c);
    full(&mut c);
    failreply(&mut c);
    thread(&mut c);
    fork_session(&mut c);
    let (_, la_end) = c.stat(STAT_A);
    let (_, lb_end) = c.stat(STAT_B);
    yarm_user_rt::user_log!(
        "VC1C_FINAL open_a={} open_a_start={} open_b={} open_b_start={}",
        la_end,
        la,
        lb_end,
        lb
    );
    c.check("final_backend_counts", la_end == la && lb_end == lb);
    yarm_user_rt::user_log!(
        "VC1C_FD_WITNESS_SEAL result={} fails={} tid_a={} tid_b={}",
        if c.fails == 0 { "ok" } else { "fail" },
        c.fails,
        tid_a,
        tid_b
    );
    c.fails == 0
}
