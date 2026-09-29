// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP3-ACCEPTANCE §2 — the live overtaken-deferral witness, shared by the x86_64 and
//! AArch64 bridges. Built only with `x86-overtaken-witness` / `aarch64-overtaken-witness`, and
//! armed only when the port's provisioning has built its two tasks.
//!
//! Two kernel-built user tasks (the programs are per port):
//!
//! * **W**, pinned to CPU 0, issues a real FutexWait on the witness word, once per round;
//! * **K**, pinned to CPU 1, issues real FutexWakes on that word until one of them wakes W.
//!
//! The ONE synchronization point is here, at the entry of CPU 0's FutexWait drain — after W's
//! blocking commit has cleared `current` and armed the drain, with no lock held: when the drain's
//! outgoing task is W, parked on the witness word, it waits (bounded) until W is no longer
//! `Blocked(Futex)`. The only thing that can change that is K's production wake on the other CPU
//! (nothing else waits on or wakes the word). The drain then runs unmodified: its re-verify fails
//! and the shared bridge's overtaken settlement settles it. The witness only observes and records
//! — the block, the wake, the enqueue, the selection and the resume are all production owners'.
//!
//! A round whose wake had already landed when the drain was entered (`spins=0`) is recorded as
//! NATURAL; one the hold waited for as SYNCHRONIZED; one whose bound expired as a TIMEOUT (the
//! drain then took its ordinary blocked path and the round is not an overtaken one).
//!
//! W reports its own verdicts by WHERE it parks next: back on the witness word for the next round,
//! on `FAIL` when its result lane or callee-saved registers did not survive, on `DONE` after the
//! last round. The `DONE` park emits the sealed summary once K has parked too.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::kernel::scheduler::CpuId;
use crate::kernel::task::{TaskStatus, WaitReason};
use crate::kernel::vm::VirtAddr;

/// The shared mailbox page both programs map at the same user VA.
pub const MBX_VA: u64 = 0x2007_0000;
/// The witness word: W waits on it every round, K wakes it.
pub const WAIT_VA: u64 = MBX_VA;
/// W parks here when a round's check fails.
pub const FAIL_VA: u64 = MBX_VA + 0x40;
/// W parks here after the last round.
pub const DONE_VA: u64 = MBX_VA + 0x80;
/// K parks here after its last wake.
pub const KPARK_VA: u64 = MBX_VA + 0xC0;
/// Rounds per boot. Must match the programs.
pub const ROUNDS: usize = 4;

/// Hold bound, in polls. Each poll is one rank-2 status read followed by a short pause.
const HOLD_POLLS: u64 = 20_000_000;
const POLL_PAUSE: u32 = 64;
/// Bound on waiting for K to park before the summary is emitted.
const DONE_POLLS: u64 = 20_000_000;

const OBS_OVERTAKEN: u64 = 1;
const OBS_TIMEOUT: u64 = 2;

static ARMED: AtomicBool = AtomicBool::new(false);
static DUMPED: AtomicBool = AtomicBool::new(false);
static W_TID: AtomicU64 = AtomicU64::new(0);
static W_ASID: AtomicU64 = AtomicU64::new(0);
static K_TID: AtomicU64 = AtomicU64::new(0);
static K_ASID: AtomicU64 = AtomicU64::new(0);
static HOLDS: AtomicU64 = AtomicU64::new(0);
static HOLD_CPU: AtomicU64 = AtomicU64::new(u64::MAX);
static SPINS: [AtomicU64; ROUNDS] = [const { AtomicU64::new(0) }; ROUNDS];
static OBSERVED: [AtomicU64; ROUNDS] = [const { AtomicU64::new(0) }; ROUNDS];
/// `OvertakenSettlement` slug index + 1 (0 = none recorded for the round).
static SETTLED: [AtomicU64; ROUNDS] = [const { AtomicU64::new(0) }; ROUNDS];
static INCOMING: [AtomicU64; ROUNDS] = [const { AtomicU64::new(u64::MAX) }; ROUNDS];
static INCOMING_ASID: [AtomicU64; ROUNDS] = [const { AtomicU64::new(0) }; ROUNDS];

const SETTLEMENTS: [&str; 5] = [
    "switch",
    "idle",
    "return_to_installed",
    "unauthenticated",
    "torn",
];

/// Called once by the port's provisioning, after both tasks exist with their address spaces.
pub fn arm(w_tid: u64, w_asid: u16, k_tid: u64, k_asid: u16) {
    W_TID.store(w_tid, Ordering::Release);
    W_ASID.store(u64::from(w_asid), Ordering::Release);
    K_TID.store(k_tid, Ordering::Release);
    K_ASID.store(u64::from(k_asid), Ordering::Release);
    ARMED.store(true, Ordering::Release);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "OVT_WITNESS_ARMED w_tid={} w_asid={} k_tid={} k_asid={} rounds={} wait_va=0x{:x}",
        w_tid, w_asid, k_tid, k_asid, ROUNDS, WAIT_VA
    ));
}

fn armed() -> bool {
    ARMED.load(Ordering::Acquire)
}

/// Where `tid` is parked, if it is `Blocked(Futex)`.
fn futex_addr_of(shared: &crate::runtime::SharedKernel, tid: u64) -> Option<u64> {
    shared.with_task_tcbs_split_mut(|tcbs| {
        tcbs.iter()
            .flatten()
            .find(|t| t.tid.0 == tid)
            .and_then(|t| match t.status {
                TaskStatus::Blocked(WaitReason::Futex(VirtAddr(a))) => Some(a),
                _ => None,
            })
    })
}

fn pause() {
    for _ in 0..POLL_PAUSE {
        core::hint::spin_loop();
    }
}

/// THE synchronization point: the entry of a FutexWait drain whose outgoing task is `outgoing`.
/// No lock is held by the caller.
pub fn hold_before_futex_drain(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    outgoing: Option<u64>,
) {
    let w = W_TID.load(Ordering::Acquire);
    if !armed() || outgoing != Some(w) {
        return;
    }
    // The drain's outgoing W has been woken already, or parked somewhere other than the witness
    // word: only the witness word opens a round.
    match futex_addr_of(shared, w) {
        Some(a) if a == WAIT_VA => {}
        Some(a) if a == FAIL_VA => return dump(shared, "w_check_failed"),
        Some(a) if a == DONE_VA => return finish(shared),
        None => {
            // Woken before this drain was even entered: still a witness-word round, and a
            // natural overtaking. It is attributed below exactly like a held one.
        }
        Some(_) => return,
    }
    let round = HOLDS.fetch_add(1, Ordering::AcqRel) as usize;
    if round >= ROUNDS {
        return;
    }
    HOLD_CPU.store(u64::from(cpu.0), Ordering::Release);
    let mut spins = 0u64;
    let observed = loop {
        if futex_addr_of(shared, w) != Some(WAIT_VA) {
            break OBS_OVERTAKEN;
        }
        if spins >= HOLD_POLLS {
            break OBS_TIMEOUT;
        }
        spins += 1;
        pause();
    };
    SPINS[round].store(spins, Ordering::Release);
    OBSERVED[round].store(observed, Ordering::Release);
    crate::yarm_log!(
        "OVT_HOLD_RELEASED cpu={} round={} tid={} spins={} observed={}",
        cpu.0,
        round + 1,
        w,
        spins,
        if observed == OBS_OVERTAKEN {
            "overtaken"
        } else {
            "timeout"
        }
    );
}

/// The bridge's overtaken settlement, recorded against the round it settles.
pub fn note_settlement(outgoing: u64, settlement: &str, incoming: u64, asid: u16) {
    if !armed() || outgoing != W_TID.load(Ordering::Acquire) {
        return;
    }
    let held = HOLDS.load(Ordering::Acquire) as usize;
    if held == 0 || held > ROUNDS {
        return;
    }
    let round = held - 1;
    let code = SETTLEMENTS
        .iter()
        .position(|s| *s == settlement)
        .map(|i| i as u64 + 1)
        .unwrap_or(0);
    if SETTLED[round]
        .compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        INCOMING[round].store(incoming, Ordering::Release);
        INCOMING_ASID[round].store(u64::from(asid), Ordering::Release);
    }
}

/// W parked on `DONE`: wait (bounded) for K to park, so the summary is not interleaved with its
/// output, then emit it.
fn finish(shared: &crate::runtime::SharedKernel) {
    let k = K_TID.load(Ordering::Acquire);
    let mut polls = 0u64;
    while futex_addr_of(shared, k) != Some(KPARK_VA) && polls < DONE_POLLS {
        polls += 1;
        pause();
    }
    dump(shared, "none");
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for &b in bytes {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    h
}

fn dump(_shared: &crate::runtime::SharedKernel, reason: &str) {
    if DUMPED.swap(true, Ordering::AcqRel) {
        return;
    }
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "aarch64"
    };
    let w = W_TID.load(Ordering::Acquire);
    let w_asid = W_ASID.load(Ordering::Acquire);
    // The console truncates long lines, so the summary is emitted as short parts: a header, one
    // part per round and a verdict. Each part carries its own checksum; the grader re-joins them.
    let mut parts: [Line; ROUNDS + 2] = [const { Line::new() }; ROUNDS + 2];
    let _ = core::fmt::write(
        &mut parts[0],
        format_args!(
            "arch={} hold_cpu={} w_tid={} w_asid={} k_tid={} k_asid={} rounds={} held={}",
            arch,
            HOLD_CPU.load(Ordering::Acquire),
            w,
            w_asid,
            K_TID.load(Ordering::Acquire),
            K_ASID.load(Ordering::Acquire),
            ROUNDS,
            HOLDS.load(Ordering::Acquire).min(ROUNDS as u64)
        ),
    );
    let (mut overtaken, mut synchronized, mut natural, mut timeouts) = (0, 0, 0, 0);
    let (mut settled, mut to_w) = (0, 0);
    for r in 0..ROUNDS {
        let spins = SPINS[r].load(Ordering::Acquire);
        let obs = OBSERVED[r].load(Ordering::Acquire);
        let code = SETTLED[r].load(Ordering::Acquire);
        let inc = INCOMING[r].load(Ordering::Acquire);
        let inc_asid = INCOMING_ASID[r].load(Ordering::Acquire);
        if obs == OBS_OVERTAKEN {
            overtaken += 1;
            if spins > 0 {
                synchronized += 1;
            } else {
                natural += 1;
            }
        } else if obs == OBS_TIMEOUT {
            timeouts += 1;
        }
        let name = match code {
            0 => "none",
            c => SETTLEMENTS[(c - 1) as usize],
        };
        if code == 1 || code == 2 {
            settled += 1;
        }
        if code == 1 && inc == w && inc_asid == w_asid {
            to_w += 1;
        }
        let _ = core::fmt::write(
            &mut parts[1 + r],
            format_args!(
                "r{}={}/{}/{}/{}/{}",
                r + 1,
                spins,
                match obs {
                    OBS_OVERTAKEN => "overtaken",
                    OBS_TIMEOUT => "timeout",
                    _ => "none",
                },
                name,
                if inc == u64::MAX { 0 } else { inc },
                inc_asid
            ),
        );
    }
    let ok = reason == "none" && overtaken == ROUNDS && settled == ROUNDS;
    let _ = core::fmt::write(
        &mut parts[ROUNDS + 1],
        format_args!(
            "overtaken={} synchronized={} natural={} timeouts={} settled={} switched_to_w={} reason={} result={}",
            overtaken,
            synchronized,
            natural,
            timeouts,
            settled,
            to_w,
            reason,
            if ok { "ok" } else { "fail" }
        ),
    );
    let total = parts.len();
    for pass in 1..=2 {
        for (i, part) in parts.iter().enumerate() {
            let text = part.as_str();
            crate::kernel::printk::printk_emit_sync(format_args!(
                "OVT_SUM part={}/{} {} pass={} crc=0x{:08x}",
                i + 1,
                total,
                text,
                pass,
                fnv1a(text.as_bytes())
            ));
        }
    }
}

/// A fixed-capacity line buffer: the summary is formatted once and emitted twice, byte-identical.
pub(crate) struct Line {
    buf: [u8; 160],
    len: usize,
}

impl Line {
    const fn new() -> Self {
        Self {
            buf: [0; 160],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("result=fail")
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let b = s.as_bytes();
        let n = b.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&b[..n]);
        self.len += n;
        Ok(())
    }
}
