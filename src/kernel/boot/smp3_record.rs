// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP3 — the RISC-V two-hart witness's bounded transaction record, and the pure verifier
//! that grades it.
//!
//! # The record
//!
//! Each owner appends ONE record at its own commit point: the IPI owner when it has PUBLISHED a
//! reschedule request (before it asks the firmware) and again once the firmware answered; the
//! bridge's entry owner when it consumed a supervisor software interrupt (SSIP cleared, mailbox
//! swapped); the idle queue advance once its resume is committed; the VM transaction at operation
//! entry/exit, displacement, shootdown answer and settlement; the remote-fence owner immediately
//! before the firmware call and once it returned; and the DebugLog route for each user step, with
//! the stepping hart's supervisor-entry count where residency is graded. A slot is claimed by one
//! `fetch_add` and published by a `Release` store after every field is written, so the claim
//! order is a total order consistent with every happens-before edge between owners.
//!
//! # What is graded, and what is not
//!
//! Only CAUSAL edges: every "before" the verifier requires is one the sender records before the
//! action the receiver can observe. There is no end-of-interrupt on this port and none is looked
//! for: a supervisor software interrupt is consumed by clearing `sip.SSIP`, and that consumption is
//! the arrival record itself. Firmware requests, arrivals and consumed publications are counted
//! apart; several requests merging into one arrival, and a publication merging into one already
//! pending, are neither loss nor duplication — but an arrival no publication explains, a
//! publication nothing consumes, or more arrivals than firmware requests, all fail.

/// Record capacity. Bounded: a witness that overflows it fails rather than truncating quietly.
pub const SLOTS: usize = 2048;

/// What a record says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// `cpu` published a request for CPU `f[0]` (hart `f[1]`), kind `f[2]` (0 wake, 1 release,
    /// 2 self-kick); `f[3]` = 1 when the bit was already pending (merged). Recorded AFTER the
    /// publication and BEFORE the firmware call. SMP3-EVIDENCE1: `f[4]` is the publication group
    /// — the sender's count of its FRESH publications to that CPU, this one included when fresh
    /// (0 for a release, which is not a mailbox publication).
    IpiPublished = 1,
    /// The firmware answered that request: `f[0]` target CPU, `f[1]` hart, `f[2]` kind, `f[3]` 0 =
    /// `SBI_SUCCESS`, else the error code as `u64`.
    IpiRequested = 2,
    /// `cpu` consumed a supervisor software interrupt: `f[0]` the source CPUs it consumed (0 = an
    /// empty arrival), `f[1]` origin | window << 8 | entries << 16 (the hart's supervisor-entry
    /// count, this entry included), `f[2]` `sepc`, `f[3]` the task current when it was taken (0 =
    /// none) | the address space `satp` named then << 32, `f[4]` the interrupted `sstatus`.
    IpiArrived = 3,
    /// `cpu`'s idle advance resumed task `f[0]`; `f[1]` = 1 when the IPI drove it, 0 the timer.
    IdleDispatch = 4,
    /// `cpu` is about to ask the firmware to fence: `f[0]` asid, `f[1]` va, `f[2]` hart mask,
    /// `f[3]` request generation, `f[4]` target CPU mask. After the `fence rw,rw`.
    FenceRequest = 5,
    /// The firmware returned for that request: `f[0]` asid, `f[1]` va, `f[2]` hart mask, `f[3]`
    /// generation, `f[4]` 0 = completed on every target, else the error code.
    FenceDone = 6,
    /// The VM transaction displaced `f[2]` at (`f[0]`, `f[1]`), still pinned.
    VmDisplaced = 7,
    /// Its shootdown owner answered `f[2]` (1 = acknowledged).
    VmShootdown = 8,
    /// The displaced backing `f[2]` was released for reclaim.
    VmSettled = 9,
    /// A user step: `f[0]` tid, `f[1]` asid, `f[2]` step code, `f[3]` round, `f[4]` aux.
    User = 10,
    /// The stepping hart's supervisor-entry count at that step: `f[0]` tid, `f[1]` count, `f[2]`
    /// step code, `f[3]` round.
    Residency = 11,
    /// The production VM mapping transaction entered: `f[0]` target asid, `f[1]` va, `f[2]`
    /// length, `f[3]` operation generation, `f[4]` caller tid.
    VmOpBegin = 12,
    /// That transaction returned: `f[0]` asid, `f[1]` va, `f[2]` its begin's generation, `f[3]`
    /// the outcome (0 = `Ok`, else the error code), `f[4]` the address returned.
    VmOpEnd = 13,
    /// A parked secondary consumed its release: `f[0]` hart.
    ParkReleased = 14,
    /// Contended spin-lock acquisitions so far, all CPUs: `f[0]` the count, `f[1]` 0 at a watched
    /// operation's entry, 1 at its exit, `f[2]` that operation's generation.
    Contention = 15,
    /// QEMU-SMP3-SEAL — a waker (P2) or requester (P3) established readiness before its production
    /// operation: `f[0]` target CPU | phase << 8 | met << 16 (1 = the bounded wait saw the target
    /// current and not idle — and, for P2, its hart's helper parked) | helper_parked << 17 (P2: the
    /// helper the call wakes was blocked in receive, so the send owed a wake and its IPI), `f[1]` the target CPU's current tid then (0 = none), `f[2]` the
    /// target hart's supervisor-entry count then, `f[3]` the attempt's round, `f[4]` its
    /// generation.
    Ready = 16,
    /// The shootdown owner's own target computation for the witness page, before any remote
    /// request: `f[0]` asid, `f[1]` va, `f[2]` the remote target CPU mask it computed, `f[3]` /
    /// `f[4]` CPU 0's / CPU 1's current tid in that same snapshot (0 = none).
    ShootTargets = 17,
    /// `cpu` is about to write `satp` (whose write issues `sfence.vma x0, x0`) while activation
    /// history is armed on it: `f[0]` the asid that `satp` names, `f[1]` the `satp` value.
    Activation = 18,
    /// SMP3-EVIDENCE1 — the inputs the shootdown owner's target computation consumed, recorded
    /// with (immediately before) its `ShootTargets` by the requesting CPU: `f[0]` asid, `f[1]` va,
    /// `f[2]` online | wake-only << 32, `f[3]` the placement contribution, `f[4]` the loaded-root
    /// residency contribution (both as computed, already filtered to the candidates). `u64::MAX`
    /// fields: no snapshot of this computation was kept.
    ShootInputs = 19,
    /// SMP3-EVIDENCE1 — `cpu` consumed source `f[0]`'s bit in the arrival it recorded just before
    /// (one per consumed source, ascending), and this is its `f[1]`-th consumption of that source.
    /// Only `cpu` swaps its own mailbox, so the ordinal is its program order of consumptions.
    IpiConsumed = 20,
    /// SMP3-EVIDENCE1 — LABELLED WITNESS SYNCHRONIZATION took effect on `cpu` (never production
    /// behaviour; graded by neither verifier, kept so synchronized cases are told from natural
    /// ones): `f[0]` what — `SYNC_GAP` (a requester's bounded wait, before its shootdown decision,
    /// for the target CPU's dispatch gap) or `SYNC_MERGED` (a bounded delay of a merged
    /// publication's record until the target consumed its group) — `f[1]` the target CPU, `f[2]`
    /// the round (gap) or group (merged), `f[3]` 1 when the wait was met, 0 when it timed out.
    WitnessSync = 21,
}

/// `WitnessSync.f[0]`.
pub const SYNC_GAP: u64 = 1;
pub const SYNC_MERGED: u64 = 2;

impl Kind {
    pub fn from_u8(v: u8) -> Option<Kind> {
        Some(match v {
            1 => Kind::IpiPublished,
            2 => Kind::IpiRequested,
            3 => Kind::IpiArrived,
            4 => Kind::IdleDispatch,
            5 => Kind::FenceRequest,
            6 => Kind::FenceDone,
            7 => Kind::VmDisplaced,
            8 => Kind::VmShootdown,
            9 => Kind::VmSettled,
            10 => Kind::User,
            11 => Kind::Residency,
            12 => Kind::VmOpBegin,
            13 => Kind::VmOpEnd,
            14 => Kind::ParkReleased,
            15 => Kind::Contention,
            16 => Kind::Ready,
            17 => Kind::ShootTargets,
            18 => Kind::Activation,
            19 => Kind::ShootInputs,
            20 => Kind::IpiConsumed,
            21 => Kind::WitnessSync,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::IpiPublished => "ipi_published",
            Kind::IpiRequested => "ipi_requested",
            Kind::IpiArrived => "ipi_arrived",
            Kind::IdleDispatch => "idle_dispatch",
            Kind::FenceRequest => "fence_request",
            Kind::FenceDone => "fence_done",
            Kind::VmDisplaced => "vm_displaced",
            Kind::VmShootdown => "vm_shootdown",
            Kind::VmSettled => "vm_settled",
            Kind::User => "user",
            Kind::Residency => "residency",
            Kind::VmOpBegin => "vm_op_begin",
            Kind::VmOpEnd => "vm_op_end",
            Kind::ParkReleased => "park_released",
            Kind::Contention => "contention",
            Kind::Ready => "ready",
            Kind::ShootTargets => "shoot_targets",
            Kind::Activation => "activation",
            Kind::ShootInputs => "shoot_inputs",
            Kind::IpiConsumed => "ipi_consumed",
            Kind::WitnessSync => "witness_sync",
        }
    }
}

/// Publication kinds (`IpiPublished.f[2]`).
pub const PUB_WAKE: u64 = 0;
pub const PUB_RELEASE: u64 = 1;
pub const PUB_KICK: u64 = 2;

/// Arrival origins (low byte of `IpiArrived.f[1]`).
pub const ORIGIN_USER: u64 = 0;
pub const ORIGIN_IDLE: u64 = 1;

/// Windows (second byte of `IpiArrived.f[1]`): where in the witness programs `sepc` was.
pub const WINDOW_NONE: u64 = 0;
pub const WINDOW_S_A: u64 = 1;
pub const WINDOW_C_B: u64 = 2;
pub const WINDOW_S_TLB: u64 = 3;
pub const WINDOW_C_TLB: u64 = 4;

/// Readiness phases (`Ready.f[0]` bits 15:8).
pub const PHASE_P2: u64 = 2;
pub const PHASE_P3: u64 = 3;

/// `IpiArrived` decoding: origin, window, the hart's entry count, the interrupted tid and asid.
pub fn arr_origin(r: &Rec) -> u64 {
    r.f[1] & 0xff
}
pub fn arr_window(r: &Rec) -> u64 {
    (r.f[1] >> 8) & 0xff
}
pub fn arr_entries(r: &Rec) -> u64 {
    r.f[1] >> 16
}
pub fn arr_tid(r: &Rec) -> u64 {
    r.f[3] & 0xffff_ffff
}
pub fn arr_asid(r: &Rec) -> u64 {
    r.f[3] >> 32
}
/// `Ready` decoding.
pub fn ready_target_cpu(r: &Rec) -> u64 {
    r.f[0] & 0xff
}
pub fn ready_phase(r: &Rec) -> u64 {
    (r.f[0] >> 8) & 0xff
}
pub fn ready_met(r: &Rec) -> bool {
    (r.f[0] >> 16) & 1 == 1
}
/// P2: the helper on the target's hart was parked in receive when readiness was recorded.
pub fn ready_helper_parked(r: &Rec) -> bool {
    (r.f[0] >> 17) & 1 == 1
}

/// `sstatus.FS` (bits 14:13) and `sstatus.VS` (bits 10:9).
pub const SSTATUS_FS_VS: u64 = (0b11 << 13) | (0b11 << 9);

/// One record as the verifier sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rec {
    pub seq: u32,
    pub kind: Kind,
    pub cpu: u8,
    pub f: [u64; 5],
}

/// The user steps, by the exact message after `SMP3 `. Failure steps are >= `STEP_FAIL`.
pub const STEP_FAIL: u64 = 100;
pub const STEPS: &[(&str, u64)] = &[
    ("S_ENTERED", 1),
    ("S_P1_RESUMED", 2),
    ("S_P1_REPLY", 3),
    ("S_WIN_A_OK", 4),
    ("S_P2B_CALL", 5),
    ("S_P2B_REPLY_OK", 6),
    ("S_REQ", 7),
    ("S_REQ_DONE", 8),
    ("S_PRE", 9),
    ("S_OBSERVED", 10),
    ("S_MUT_NR3", 11),
    ("S_MUT_OK", 12),
    ("S_DONE", 13),
    ("S_P2B_SENT", 14),
    ("C_ENTERED", 21),
    ("C_P1_CALL", 22),
    ("C_P1_RESUMED", 23),
    ("C_P2A_CALL", 24),
    ("C_P2A_REPLY_OK", 25),
    ("C_WIN_B_OK", 26),
    ("C_REQ", 27),
    ("C_REQ_DONE", 28),
    ("C_PRE", 29),
    ("C_OBSERVED", 30),
    ("C_MUT_NR3", 31),
    ("C_MUT_OK", 32),
    ("C_DONE", 33),
    ("C_P2A_SENT", 34),
    ("H_ENTERED", 41),
    ("H_SERVED", 42),
];

/// The step code for a witness message (`SMP3 <step>`); `STEP_FAIL + 1` for a failure step and
/// `STEP_FAIL` for anything unrecognised — an unknown step is never silently accepted.
pub fn step_code(msg: &str) -> Option<u64> {
    let step = msg.strip_prefix("SMP3 ")?.trim_end();
    if step.contains("_FAIL") {
        return Some(STEP_FAIL + 1);
    }
    Some(
        STEPS
            .iter()
            .find(|(name, _)| *name == step)
            .map_or(STEP_FAIL, |(_, code)| *code),
    )
}

pub fn step_name(code: u64) -> &'static str {
    STEPS
        .iter()
        .find(|(_, c)| *c == code)
        .map_or("FAIL", |(name, _)| name)
}

fn code(name: &str) -> u64 {
    STEPS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .unwrap_or(u64::MAX)
}

// ─────────────────────────────── the pure verifier ───────────────────────────────

/// Parked-target rounds per direction, and the IPI-driven dispatches CPU 0 must show of them.
pub const P1_ROUNDS: u64 = 8;
pub const P1_MIN_IPI_TO_C: usize = 2;
/// QEMU-SMP3-SEAL — the fixed attempt budgets, chosen before qualification. P2: attempts per
/// direction at an IPI taken in a resident user target, and the credited attempts each direction
/// needs (unchanged from SMP3's one). P3: serial remote-invalidation attempts (odd: S is the
/// target; even: C is), and the credited attempts each direction needs (unchanged from SMP3's
/// two) — credited meaning the target was resident over the whole interval.
pub const P2_ATTEMPTS: u64 = 6;
pub const P2_MIN_CREDITED_PER_DIRECTION: usize = 1;
pub const TLB_ROUNDS: u64 = 24;
pub const TLB_MIN_CREDITED_PER_DIRECTION: usize = 2;
/// Mutual rounds; every one must show its two production operations in flight together.
// QEMU-LOCK1: the LOCK1 build drives twelve mutual rounds (six per contention direction); the plain
// SMP3 build keeps four. The verifier grades exactly the rounds the workload runs, so the count
// tracks the assembly's injected `YARM_MUT_ROUNDS`.
#[cfg(not(feature = "riscv64-lock1-witness"))]
pub const MUT_ROUNDS: u64 = 4;
#[cfg(feature = "riscv64-lock1-witness")]
pub const MUT_ROUNDS: u64 = 12;

/// A witness task: `(tid, asid, cpu, hart)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Role {
    pub tid: u64,
    pub asid: u64,
    pub cpu: u8,
    pub hart: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Roles {
    pub s: Role,
    pub c: Role,
    pub h1: Role,
    pub h0: Role,
    /// The page each task creates and the other replaces.
    pub w_va: u64,
}

/// Why the IPI population does not balance.
///
/// SMP3-EVIDENCE1 — attribution follows the mailbox protocol, not record order. For one (source,
/// target) pair, bit `source` of the target's mailbox is set only by the source's `fetch_or` and
/// cleared only by the target's `swap`, so its modification order alternates: a FRESH publication
/// (0 -> 1) opens a group, any MERGED publications (bit already 1) join it, and the target's next
/// swap that sees the bit consumes it (1 -> 0). The k-th fresh publication therefore opens group
/// k and the k-th consumption takes exactly group k. Each side numbers its own events in program
/// order (the sender its groups, the target its consumptions), so the pairing is exact while the
/// RECORDS of the two sides may appear in either order: a record is pushed after its operation,
/// and the other CPU can act in between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpiRefusal {
    /// A consumption of a group its source never opened.
    Unexplained { arrival: u32 },
    /// A fresh publication that does not open its sender's next group, or a merged one outside
    /// the sender's current group: the publication accounting is inconsistent (or stale).
    GroupInconsistent { publication: u32 },
    /// A group no arrival consumed (a lost publication).
    Undelivered { publication: u32 },
    /// An arrival's consumed sources and the consumption records that follow it on its CPU do not
    /// match one for one, in order — or a consumption skipped an ordinal.
    ConsumptionUnbound { record: u32 },
    /// A group consumed twice.
    DuplicateAttribution { record: u32 },
    /// A CPU consumed more supervisor software interrupts than the firmware was successfully asked
    /// to raise on it.
    ArrivalsExceedRequests { cpu: u8 },
    /// A firmware request failed.
    FirmwareRefused { request: u32 },
}

/// What the IPI population accounted for: mailbox publications (fresh and merged), successful
/// firmware requests, hardware arrivals (empty ones included), and consumed source bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IpiCounts {
    pub published: usize,
    pub fresh: usize,
    pub merged: usize,
    pub requests: usize,
    pub arrivals: usize,
    pub empty: usize,
    pub consumed: usize,
}

const NCPU: usize = 64;

/// Pair every publication group with the consumption that took it, and bound arrivals by
/// successful requests. Pure; the first refusal in a fixed order: the record scan (in record
/// order), then unexplained consumptions, then undelivered groups, then arrivals per CPU.
pub fn check_ipi_population(recs: &[Rec]) -> Result<IpiCounts, IpiRefusal> {
    let mut n = IpiCounts::default();
    // Groups each sender opened per target, and consumptions each target made per source.
    let mut opened = alloc::vec![0u64; NCPU * NCPU];
    let mut took = alloc::vec![0u64; NCPU * NCPU];
    // Source bits an arrival still owes consumption records for, and that arrival's seq.
    let mut owed = [(0u64, 0u32); NCPU];
    let mut requests = [0usize; NCPU];
    let mut taken = [0usize; NCPU];
    // (src, dst, k, arrival seq) per consumption record, in record order.
    let mut consumptions: alloc::vec::Vec<(usize, usize, u64, u32)> = alloc::vec::Vec::new();
    let pair = |a: usize, b: usize| (a & (NCPU - 1)) * NCPU + (b & (NCPU - 1));
    for r in recs {
        let cpu = r.cpu as usize & (NCPU - 1);
        if owed[cpu].0 != 0 && r.kind != Kind::IpiConsumed {
            return Err(IpiRefusal::ConsumptionUnbound {
                record: owed[cpu].1,
            });
        }
        match r.kind {
            Kind::IpiPublished if r.f[2] == PUB_WAKE || r.f[2] == PUB_KICK => {
                n.published += 1;
                let g = &mut opened[pair(cpu, r.f[0] as usize)];
                if r.f[3] == 0 {
                    if r.f[4] != *g + 1 {
                        return Err(IpiRefusal::GroupInconsistent { publication: r.seq });
                    }
                    *g = r.f[4];
                    n.fresh += 1;
                } else {
                    if r.f[3] != 1 || r.f[4] == 0 || r.f[4] != *g {
                        return Err(IpiRefusal::GroupInconsistent { publication: r.seq });
                    }
                    n.merged += 1;
                }
            }
            Kind::IpiRequested => {
                if r.f[3] != 0 {
                    return Err(IpiRefusal::FirmwareRefused { request: r.seq });
                }
                requests[r.f[0] as usize & (NCPU - 1)] += 1;
                n.requests += 1;
            }
            Kind::ParkReleased => taken[cpu] += 1,
            Kind::IpiArrived => {
                taken[cpu] += 1;
                n.arrivals += 1;
                if r.f[0] == 0 {
                    n.empty += 1;
                }
                owed[cpu] = (r.f[0], r.seq);
            }
            Kind::IpiConsumed => {
                let (bits, at) = owed[cpu];
                let src = r.f[0] as usize;
                // The lowest source still owed, exactly: consumption records follow their arrival
                // one per consumed source, ascending.
                if bits == 0
                    || src >= NCPU
                    || bits & ((1u64 << src) | ((1u64 << src) - 1)) != 1u64 << src
                {
                    return Err(IpiRefusal::ConsumptionUnbound { record: r.seq });
                }
                owed[cpu].0 &= !(1u64 << src);
                let k = &mut took[pair(src, cpu)];
                if r.f[1] <= *k {
                    return Err(IpiRefusal::DuplicateAttribution { record: r.seq });
                }
                if r.f[1] != *k + 1 {
                    return Err(IpiRefusal::ConsumptionUnbound { record: r.seq });
                }
                *k = r.f[1];
                n.consumed += 1;
                consumptions.push((src, cpu, r.f[1], at));
            }
            _ => {}
        }
    }
    if let Some((_, at)) = owed.iter().find(|(bits, _)| *bits != 0) {
        return Err(IpiRefusal::ConsumptionUnbound { record: *at });
    }
    for &(src, dst, k, at) in &consumptions {
        if k > opened[pair(src, dst)] {
            return Err(IpiRefusal::Unexplained { arrival: at });
        }
    }
    for r in recs {
        if r.kind == Kind::IpiPublished
            && (r.f[2] == PUB_WAKE || r.f[2] == PUB_KICK)
            && r.f[3] == 0
            && r.f[4] > took[pair(r.cpu as usize, r.f[0] as usize)]
        {
            return Err(IpiRefusal::Undelivered { publication: r.seq });
        }
    }
    for cpu in 0..NCPU {
        if taken[cpu] > requests[cpu] {
            return Err(IpiRefusal::ArrivalsExceedRequests { cpu: cpu as u8 });
        }
    }
    Ok(n)
}

/// The arrival that consumed publication `recs[p]`'s group, and whether `p` opened that group
/// (fresh) rather than merging into it. Pure. Found by group and ordinal — never by record order,
/// so a publication recorded after the arrival that consumed it is attributed correctly.
pub fn consumer_of(recs: &[Rec], p: usize) -> Option<(usize, bool)> {
    let pb = recs.get(p)?;
    if pb.kind != Kind::IpiPublished || pb.f[4] == 0 {
        return None;
    }
    let dst = pb.f[0];
    let c = recs.iter().position(|r| {
        r.kind == Kind::IpiConsumed
            && u64::from(r.cpu) == dst
            && r.f[0] == u64::from(pb.cpu)
            && r.f[1] == pb.f[4]
    })?;
    let a = (0..c)
        .rev()
        .find(|&i| u64::from(recs[i].cpu) == dst && recs[i].kind != Kind::IpiConsumed)?;
    if recs[a].kind != Kind::IpiArrived || recs[a].f[0] & (1u64 << (pb.cpu & 63)) == 0 {
        return None;
    }
    Some((a, pb.f[3] == 0))
}

/// Why a production VM operation's begin is not a completed, successful operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmOpRefusal {
    NotABegin,
    Incomplete,
    Duplicate,
    Failed,
}

/// The completion of the production VM operation begun at `recs[begin]`: same CPU, same target
/// address space, same mapping and the same generation, after it, returning `Ok` with the
/// requested address. Pure.
pub fn match_vm_op(recs: &[Rec], begin: usize) -> Result<usize, VmOpRefusal> {
    let b = recs.get(begin).ok_or(VmOpRefusal::NotABegin)?;
    if b.kind != Kind::VmOpBegin {
        return Err(VmOpRefusal::NotABegin);
    }
    let mut found: Option<usize> = None;
    for (i, r) in recs.iter().enumerate().skip(begin + 1) {
        if r.cpu != b.cpu {
            continue;
        }
        if r.kind == Kind::VmOpBegin && found.is_none() {
            return Err(VmOpRefusal::Incomplete);
        }
        if r.kind == Kind::VmOpEnd && r.f[0] == b.f[0] && r.f[1] == b.f[1] && r.f[2] == b.f[3] {
            if found.is_some() {
                return Err(VmOpRefusal::Duplicate);
            }
            found = Some(i);
        }
    }
    let end = found.ok_or(VmOpRefusal::Incomplete)?;
    if recs[end].f[3] != 0 || recs[end].f[4] != b.f[1] {
        return Err(VmOpRefusal::Failed);
    }
    Ok(end)
}

/// Whether two operations' intervals overlap: both began before either completed.
pub fn ops_overlap(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0.max(b.0) < a.1.min(b.1)
}

/// Why a remote fence does not complete a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FenceRefusal {
    /// No completion with this request's identity and generation after it.
    Missing,
    /// A completion for the same mapping names a different generation before this one's.
    StaleGeneration,
    /// More than one completion names this request.
    Duplicate,
    /// The firmware returned an error: the fence is not known to have run.
    Failed,
}

/// The completion of the fence requested at `recs[req]`: same CPU, asid, va, hart mask and
/// generation, after it, reporting success. Pure.
pub fn match_fence(recs: &[Rec], req: usize) -> Result<usize, FenceRefusal> {
    let q = recs.get(req).ok_or(FenceRefusal::Missing)?;
    if q.kind != Kind::FenceRequest {
        return Err(FenceRefusal::Missing);
    }
    let mut found: Option<usize> = None;
    for (i, r) in recs.iter().enumerate().skip(req + 1) {
        if r.kind != Kind::FenceDone || r.cpu != q.cpu {
            continue;
        }
        if r.f[0] != q.f[0] || r.f[1] != q.f[1] || r.f[2] != q.f[2] {
            continue;
        }
        if r.f[3] != q.f[3] {
            if found.is_none() {
                return Err(FenceRefusal::StaleGeneration);
            }
            continue;
        }
        if found.is_some() {
            return Err(FenceRefusal::Duplicate);
        }
        found = Some(i);
    }
    let done = found.ok_or(FenceRefusal::Missing)?;
    if recs[done].f[4] != 0 {
        return Err(FenceRefusal::Failed);
    }
    Ok(done)
}

/// The verdict: every check, named, with the first failure's reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    pub records: usize,
    /// SMP3-EVIDENCE1 — mailbox publications (wake and kick), the groups they opened (fresh), the
    /// merged ones, and the firmware requests answered `SBI_SUCCESS`.
    pub ipi_published: usize,
    pub ipi_fresh: usize,
    pub ipi_requests: usize,
    pub ipi_arrivals: usize,
    pub ipi_consumed: usize,
    pub ipi_empty: usize,
    pub ipi_merged: usize,
    pub p1_parked: usize,
    pub p1_ipi_to_s: usize,
    pub p1_ipi_to_c: usize,
    pub p1_timer_first: usize,
    pub p1_busy: usize,
    pub p1_preceded: usize,
    /// Parked wakes whose publication merged into another's group: not uniquely attributable.
    pub p1_merged: usize,
    /// P2 attempts, and how each was graded: credited per target, uncredited (prerequisites not
    /// established), and where every arrival landed (inside the window, outside it in the target,
    /// or displaced).
    pub p2_attempts: usize,
    pub p2_credited_s: usize,
    pub p2_credited_c: usize,
    pub p2_uncredited: usize,
    pub p2_in_window: usize,
    pub p2_outside: usize,
    pub p2_displaced: usize,
    /// Serial rounds whose whole chain verified.
    pub tlb_rounds: usize,
    /// ... of which the target hart took no supervisor entry in its window, per target.
    pub tlb_credited_s: usize,
    pub tlb_credited_c: usize,
    /// ... of which the target took other entries in its window (not credited).
    pub tlb_interfered: usize,
    /// ... of which the shootdown found the target off its hart (activation-evidenced).
    pub tlb_off_cpu: usize,
    /// ... of which the readiness record did not see the target current.
    pub tlb_not_ready: usize,
    /// ... of which the shootdown named and fenced the target's CPU without seeing the target
    /// current there (the dispatch gap, or another task of the address space).
    pub tlb_not_current: usize,
    pub mutual_rounds: usize,
    pub mutual_overlapped: usize,
    /// Contended spin-lock acquisitions inside the mutual rounds' operation windows.
    pub mutual_contended: u64,
    pub settled_after_completion: usize,
    /// Settlements whose target computation named no remote CPU (the requester's local fence and
    /// every other hart's next activation are the whole shootdown).
    pub settled_local_only: usize,
    /// Mutual operations whose shootdown found the target off its hart.
    pub mutual_off_cpu: usize,
    /// Mutual operations that fenced the target's CPU without seeing the target current there.
    pub mutual_not_current: usize,
    pub user_fp_vs_off: usize,
    pub failure: Option<&'static str>,
    pub failure_at: u32,
}

impl Verdict {
    fn fail(&mut self, why: &'static str, at: u32) {
        if self.failure.is_none() {
            self.failure = Some(why);
            self.failure_at = at;
        }
    }
    pub fn ok(&self) -> bool {
        self.failure.is_none()
    }
}

fn find_from(recs: &[Rec], from: usize, pred: impl Fn(&Rec) -> bool) -> Option<usize> {
    recs.iter()
        .enumerate()
        .skip(from)
        .find(|(_, r)| pred(r))
        .map(|(i, _)| i)
}

fn user(recs: &[Rec], from: usize, role: Role, step: &str, round: Option<u64>) -> Option<usize> {
    let c = code(step);
    find_from(recs, from, |r| {
        r.kind == Kind::User
            && r.f[0] == role.tid
            && r.f[2] == c
            && round.is_none_or(|k| r.f[3] == k)
    })
}

fn residency(recs: &[Rec], role: Role, step: &str, round: u64) -> Option<(usize, u64)> {
    let c = code(step);
    find_from(recs, 0, |r| {
        r.kind == Kind::Residency && r.f[0] == role.tid && r.f[2] == c && r.f[3] == round
    })
    .map(|i| (i, recs[i].f[1]))
}

/// A wake from `from` to `to_cpu`, published after `after`, and the arrival that consumed it —
/// found through the publication's group (`consumer_of`), never by record order. The flag is
/// whether the wake opened that group (fresh); a merged wake rode on another publication's bit
/// and cannot be credited to its arrival alone.
fn wake_chain(
    recs: &[Rec],
    after: usize,
    from: Role,
    to_cpu: u8,
) -> Result<(usize, bool), &'static str> {
    let published = find_from(recs, after, |r| {
        r.kind == Kind::IpiPublished
            && r.cpu == from.cpu
            && r.f[0] == u64::from(to_cpu)
            && r.f[2] == PUB_WAKE
    })
    .ok_or("ipi_not_published")?;
    let (arrived, fresh) = consumer_of(recs, published).ok_or("ipi_not_consumed")?;
    // The consuming swap follows the publication's `fetch_or`, which follows every record its
    // sender pushed before it: a consumption recorded before `after` is not this wake's.
    if arrived <= after {
        return Err("ipi_attribution_inconsistent");
    }
    Ok((arrived, fresh))
}

/// How a parked target was resumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParkedRoute {
    /// The IPI was taken at the target's armed idle boundary and its idle advance resumed exactly
    /// the woken task.
    Ipi,
    /// The target's periodic idle advance resumed the woken task first; the IPI then arrived in
    /// that task. Only CPU 0 has a timer.
    TimerFirst,
    /// The target was running something else when the IPI arrived; the woken task was picked up
    /// at its next scheduling point. Not a parked-target wake; counted apart.
    Busy,
    /// The IPI was taken at the target's armed idle boundary and its idle advance ran, but the
    /// scheduler selected ANOTHER runnable task first — one made runnable in that same trap (a
    /// receive deadline the post-lock collector expired) or already ahead of the woken task. The
    /// woken task's own context-checked resume on that hart is still required afterwards. The IPI
    /// did its job — ended the idle wait and drove a queue advance — but it did not dispatch the
    /// woken task, so this is counted apart and never as an IPI-driven parked dispatch.
    Preceded,
    /// SMP3-EVIDENCE1 — the wake's publication MERGED into a group another publication opened, so
    /// the arrival that consumed it is not uniquely this wake's. Counted apart, never credited.
    Merged,
}

/// A wake of the blocked `woken` on `to_cpu` by `from`, after step `after`, classified. Pure.
pub fn parked_wake(
    recs: &[Rec],
    after: usize,
    from: Role,
    to_cpu: u8,
    woken: Role,
) -> Result<(usize, ParkedRoute), &'static str> {
    let (arrived, fresh) = wake_chain(recs, after, from, to_cpu)?;
    if !fresh {
        return Ok((after, ParkedRoute::Merged));
    }
    let a = recs[arrived];
    if arr_origin(&a) == ORIGIN_IDLE {
        let d = find_from(recs, arrived + 1, |r| {
            r.cpu == to_cpu && (r.kind == Kind::IdleDispatch || r.kind == Kind::IpiArrived)
        })
        .ok_or("ipi_dispatch_missing")?;
        let dr = recs[d];
        // Anything but the IPI's own idle advance after an idle-boundary arrival — a timer's
        // dispatch, a second arrival, a dispatch of nothing — is a substitution.
        if dr.kind != Kind::IdleDispatch || dr.f[1] != 1 || dr.f[0] == 0 {
            return Err("ipi_dispatch_substituted");
        }
        if dr.f[0] != woken.tid {
            return Ok((d, ParkedRoute::Preceded));
        }
        return Ok((d, ParkedRoute::Ipi));
    }
    let timer = recs[after..arrived]
        .iter()
        .position(|r| {
            r.kind == Kind::IdleDispatch && r.cpu == to_cpu && r.f[1] == 0 && r.f[0] == woken.tid
        })
        .map(|i| after + i);
    match timer {
        Some(d) if arr_tid(&a) == woken.tid => Ok((d, ParkedRoute::TimerFirst)),
        Some(_) => Err("ipi_arrived_in_another_task"),
        None => Ok((after, ParkedRoute::Busy)),
    }
}

/// The readiness record a waker (P2) or requester (P3) left for attempt `round` against the hart
/// `target_cpu`, at or after `from`.
fn ready(
    recs: &[Rec],
    from: usize,
    waker_cpu: u8,
    target_cpu: u8,
    phase: u64,
    round: u64,
) -> Option<usize> {
    find_from(recs, from, |r| {
        r.kind == Kind::Ready
            && r.cpu == waker_cpu
            && ready_target_cpu(r) == u64::from(target_cpu)
            && ready_phase(r) == phase
            && r.f[3] == round
    })
}

/// Where an arrival aimed at a user-mode target landed, as its own record shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrivalClass {
    /// Taken from U-mode in the target's exact incarnation (tid and address space), with `sepc`
    /// inside its register-checked window.
    InWindow,
    /// Taken from U-mode in the target, outside the window. Earns no user-context coverage.
    Outside,
    /// Taken in another task, or at the hart's idle boundary.
    Displaced,
    /// QEMU-SMP3-SEAL — no arrival was owed: readiness recorded the helper on the target's hart NOT
    /// parked in receive, so the production send queued the call and owed no wake. Nothing is
    /// attributed to the attempt.
    NotOwed,
}

/// One graded attempt of a phase whose population may legitimately include uncredited attempts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attempt {
    /// 2 = IPI to a resident user target, 3 = serial remote fence, 4 = mutual replacement.
    pub phase: u8,
    /// The attempt (round) number within its phase.
    pub n: u8,
    /// The CPU of the task the attempt targets.
    pub target_cpu: u8,
    /// P2: the readiness generation; P3/P4: the production operation's generation.
    pub generation: u64,
    /// Whether the witness's prerequisites were established (derived only from independently
    /// recorded identity, placement, entry and activation history — never from a missing fence or
    /// a failed check).
    pub eligible: bool,
    /// Why not (`none` when eligible), or the failure's reason.
    pub reason: &'static str,
    pub outcome: Outcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Eligible, and every obligation held: counts toward coverage.
    Credited,
    /// Every obligation held, but the prerequisites were not established: no coverage.
    Uncredited,
    /// A mutual round's operation, every obligation held (mutual rounds are not attempts at
    /// coverage: all of them must verify and overlap).
    Verified,
    /// An obligation failed. Never retried into success.
    Failed,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Outcome::Credited => "credited",
            Outcome::Uncredited => "uncredited",
            Outcome::Verified => "verified",
            Outcome::Failed => "failed",
        }
    }
}

/// Every attempt of one verification, in phase order.
pub const MAX_ATTEMPTS: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct Attempts {
    pub items: [Option<Attempt>; MAX_ATTEMPTS],
    pub len: usize,
}

impl Default for Attempts {
    fn default() -> Self {
        Attempts {
            items: [None; MAX_ATTEMPTS],
            len: 0,
        }
    }
}

impl Attempts {
    fn push(&mut self, a: Attempt) {
        if let Some(slot) = self.items.get_mut(self.len) {
            *slot = Some(a);
            self.len += 1;
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = &Attempt> {
        self.items.iter().take(self.len).flatten()
    }
}

/// What one P2 attempt showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct P2Attempt {
    /// The target's window-check step (the next attempt searches from here).
    pub end: usize,
    pub generation: u64,
    pub eligible: bool,
    pub reason: &'static str,
    pub class: ArrivalClass,
}

/// One P2 attempt: `waker` wakes a helper on `target`'s hart while `target` spins in its checked
/// window. Pure.
///
/// Obligations (a failure, never ineligibility): the waker's call step, its readiness record, the
/// waker's post-send step, and the target's own passing window check on its hart. When readiness
/// recorded the target hart's helper PARKED in receive, the send owed a wake: the publication (at
/// or before the post-send step, so a later attempt's cannot be borrowed), the arrival that
/// consumed it, and the window check after that arrival are owed too. When it recorded the helper
/// NOT parked, no wake was owed — the production send queues — and the attempt is ineligible
/// (`helper_not_parked`) from that record, never from the absence of an IPI.
///
/// Eligibility, from independently recorded evidence only: the readiness record saw `target`
/// current on its hart; the arrival was that hart's very next supervisor entry after readiness
/// (entry history); and no other address space was activated on that hart between the call step
/// and the arrival (activation history). An eligible attempt's arrival MUST be in the target's
/// exact incarnation inside its window — anything else fails. An ineligible attempt is counted by
/// where its arrival landed and earns no coverage.
#[allow(clippy::too_many_arguments)]
pub fn p2_attempt(
    recs: &[Rec],
    from: usize,
    waker: Role,
    target: Role,
    call: &str,
    sent: &str,
    ok: &str,
    window: u64,
    k: u64,
) -> Result<P2Attempt, &'static str> {
    let call_i = user(recs, from, waker, call, Some(k)).ok_or("p2_call_missing")?;
    let rd = ready(recs, call_i, waker.cpu, target.cpu, PHASE_P2, k).ok_or("p2_ready_missing")?;
    let sent_i = user(recs, rd, waker, sent, Some(k)).ok_or("p2_sent_missing")?;
    if !ready_helper_parked(&recs[rd]) {
        let ok_i = user(recs, rd, target, ok, Some(k)).ok_or("p2_window_check_missing")?;
        if recs[ok_i].cpu != target.cpu {
            return Err("p2_window_checked_elsewhere");
        }
        return Ok(P2Attempt {
            end: ok_i,
            generation: recs[rd].f[4],
            eligible: false,
            reason: "helper_not_parked",
            class: ArrivalClass::NotOwed,
        });
    }
    // The wake's publication: the waker's first after readiness, at or before its post-send step.
    find_from(recs, rd, |r| {
        r.kind == Kind::IpiPublished
            && r.cpu == waker.cpu
            && r.f[0] == u64::from(target.cpu)
            && r.f[2] == PUB_WAKE
    })
    .filter(|&p| p < sent_i)
    .ok_or("ipi_not_published")?;
    // ... and the arrival that consumed its group, searched from the readiness record the waker
    // pushed before publishing (the arrival may be recorded before the publication is).
    let (arrived, fresh) = wake_chain(recs, rd, waker, target.cpu)?;
    let ok_i = user(recs, arrived, target, ok, Some(k)).ok_or("p2_window_check_missing")?;
    if recs[ok_i].cpu != target.cpu {
        return Err("p2_window_checked_elsewhere");
    }
    let a = recs[arrived];
    let r = recs[rd];
    if arr_entries(&a) <= r.f[2] {
        return Err("p2_entry_history_inconsistent");
    }
    let in_target =
        arr_origin(&a) == ORIGIN_USER && arr_tid(&a) == target.tid && arr_asid(&a) == target.asid;
    let class = if in_target && arr_window(&a) == window {
        ArrivalClass::InWindow
    } else if in_target {
        ArrivalClass::Outside
    } else {
        ArrivalClass::Displaced
    };
    let switched = recs[call_i + 1..arrived]
        .iter()
        .any(|x| x.kind == Kind::Activation && x.cpu == target.cpu && x.f[0] != target.asid);
    // Readiness is BOTH recorded facts: the bounded wait reported it met, and it saw the target
    // current. Either alone is not readiness.
    let reason = if !ready_met(&r) || r.f[1] != target.tid {
        "not_ready"
    } else if !fresh {
        "merged_publication"
    } else if arr_entries(&a) != r.f[2] + 1 {
        "intervening_entry"
    } else if switched {
        "switched"
    } else {
        "none"
    };
    let eligible = reason == "none";
    if eligible && class != ArrivalClass::InWindow {
        return Err(if in_target {
            "ipi_sepc_outside_window"
        } else {
            "ipi_eligible_arrival_not_in_target"
        });
    }
    Ok(P2Attempt {
        end: ok_i,
        generation: r.f[4],
        eligible,
        reason,
        class,
    })
}

/// One production replacement of `target`'s W by `req`, as its owners recorded it. Pure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Replacement {
    pub begin: usize,
    pub end: usize,
    pub displaced: usize,
    /// The shootdown's own target computation (its recorded inputs immediately precede it).
    pub shoot: usize,
    /// The completed firmware fence, whenever the computation named any remote CPU.
    pub fence_done: Option<usize>,
    /// SMP3-EVIDENCE1 — whether the computation named the target's CPU, and so owed it the fence.
    /// Correctness: a named CPU is fenced and completed, whatever it was running.
    pub fenced: bool,
    /// Whether that computation's own snapshot had the target current on its CPU. Coverage only:
    /// it never decides what the operation owed.
    pub resident: bool,
}

/// The witness's CPU -> hart map, from the roles (the harts the boot actually ran): the hart mask
/// a request naming the CPUs in `cpus` must carry. `None` for a CPU no role sits on.
pub fn hart_mask_of(roles: &Roles, cpus: u64) -> Option<u64> {
    let mut harts = 0u64;
    for cpu in 0..64u8 {
        if cpus & (1u64 << cpu) == 0 {
            continue;
        }
        let hart = if cpu == roles.s.cpu {
            roles.s.hart
        } else if cpu == roles.c.cpu {
            roles.c.hart
        } else {
            return None;
        };
        harts |= 1u64.checked_shl(hart as u32)?;
    }
    Some(harts)
}

/// SMP3-EVIDENCE1 — the shootdown owner's decision at `recs[shoot]` checked against the production
/// policy, from the inputs that owner recorded it consumed (`ShootInputs`, its own record just
/// before): the candidates are online and not wake-only; the placement and loaded-root residency
/// contributions are subsets of them; the remote target set is exactly their union without the
/// requester; and the placement contribution agrees with the per-CPU current tasks the same
/// computation read (an empty CPU contributes no placement; a known task contributes exactly when
/// its address space is the operation's). Residency is taken from the recorded loaded-root
/// contribution, never inferred from the target set or from `current`. Pure.
pub fn check_shoot_decision(
    recs: &[Rec],
    roles: &Roles,
    shoot: usize,
    requester: u8,
) -> Result<usize, &'static str> {
    let s = recs.get(shoot).ok_or("shoot_targets_missing")?;
    if s.kind != Kind::ShootTargets || s.cpu != requester {
        return Err("shoot_targets_missing");
    }
    let inputs = (0..shoot)
        .rev()
        .find(|&i| recs[i].cpu == s.cpu)
        .filter(|&i| {
            recs[i].kind == Kind::ShootInputs && recs[i].f[0] == s.f[0] && recs[i].f[1] == s.f[1]
        })
        .ok_or("shoot_inputs_missing")?;
    let x = recs[inputs];
    if x.f[2..].contains(&u64::MAX) || s.f[3] == u64::MAX || s.f[4] == u64::MAX {
        return Err("shoot_inputs_unrecorded");
    }
    let (online, wake_only) = (x.f[2] & 0xffff_ffff, x.f[2] >> 32);
    let candidates = online & !wake_only;
    let (placement, loaded) = (x.f[3], x.f[4]);
    if (placement | loaded) & !candidates != 0 {
        return Err("shoot_inputs_outside_candidates");
    }
    let req_bit = 1u64.checked_shl(u32::from(requester)).unwrap_or(0);
    let mask = s.f[2];
    if mask & req_bit != 0 {
        return Err("shoot_targets_named_requester");
    }
    if mask != (placement | loaded) & !req_bit {
        return Err("shoot_targets_not_the_policy");
    }
    let known = [roles.s, roles.c, roles.h1, roles.h0];
    for cpu in 0..2u8 {
        let bit = 1u64 << cpu;
        if candidates & bit == 0 {
            continue;
        }
        let cur = s.f[3 + usize::from(cpu)];
        let placed = placement & bit != 0;
        let want = if cur == 0 {
            Some(false)
        } else {
            known
                .iter()
                .find(|r| r.tid == cur)
                .map(|r| r.asid == s.f[0])
        };
        if want.is_some_and(|w| w != placed) {
            return Err("shoot_placement_inconsistent");
        }
    }
    Ok(inputs)
}

/// One production replacement of `target`'s W by `req`, verified end to end, starting its search
/// at `from`: the operation's begin .. completion (`Ok`, W), and inside it, in this order, the
/// displacement, the shootdown's recorded decision (`check_shoot_decision`), and — whenever that
/// decision named any remote CPU — one fence request naming exactly those CPUs and exactly their
/// harts, and its successful completion; then the acknowledged shootdown and the settlement. A
/// target the decision did not name needs no fence here; the caller requires its activation
/// evidence. Pure.
pub fn replacement(
    recs: &[Rec],
    roles: &Roles,
    from: usize,
    req: Role,
    target: Role,
) -> Result<Replacement, &'static str> {
    let begin = find_from(recs, from, |x| {
        x.kind == Kind::VmOpBegin
            && x.cpu == req.cpu
            && x.f[0] == target.asid
            && x.f[1] == roles.w_va
            && x.f[4] == req.tid
    })
    .ok_or("operation_missing")?;
    let end = match match_vm_op(recs, begin) {
        Ok(e) => e,
        Err(VmOpRefusal::Failed) => return Err("operation_failed"),
        Err(VmOpRefusal::Duplicate) => return Err("operation_duplicate_completion"),
        Err(_) => return Err("operation_incomplete"),
    };
    let inside = |k: Kind, after: usize, extra: &dyn Fn(&Rec) -> bool| {
        find_from(recs, after, |x| {
            x.kind == k
                && x.cpu == req.cpu
                && x.f[0] == target.asid
                && x.f[1] == roles.w_va
                && extra(x)
        })
        .filter(|&i| i < end)
    };
    let disp =
        inside(Kind::VmDisplaced, begin, &|_| true).ok_or("displacement_outside_operation")?;
    let shoot = inside(Kind::ShootTargets, disp, &|_| true).ok_or("shoot_targets_missing")?;
    check_shoot_decision(recs, roles, shoot, req.cpu)?;
    let s = recs[shoot];
    let mask = s.f[2];
    let current = match target.cpu {
        0 => s.f[3],
        1 => s.f[4],
        _ => return Err("shoot_targets_cpu_unrecorded"),
    };
    let fenced = (mask >> (target.cpu & 63)) & 1 == 1;
    let resident = current == target.tid;
    let fence_done = if mask != 0 {
        let fence = inside(Kind::FenceRequest, shoot, &|_| true)
            .ok_or("fence_not_requested_for_the_targets")?;
        let q = recs[fence];
        if q.f[2] & 1u64.checked_shl(req.hart as u32).unwrap_or(0) != 0 {
            return Err("requester_fenced_itself");
        }
        if q.f[4] != mask {
            return Err("fence_targets_not_the_decision");
        }
        if Some(q.f[2]) != hart_mask_of(roles, mask) {
            return Err("fence_harts_not_the_targets");
        }
        let done = match match_fence(recs, fence) {
            Ok(d) => d,
            Err(FenceRefusal::Failed) => return Err("fence_failed"),
            Err(FenceRefusal::StaleGeneration) => return Err("fence_stale_completion"),
            Err(FenceRefusal::Duplicate) => return Err("fence_duplicate_completion"),
            Err(FenceRefusal::Missing) => return Err("fence_completion_missing"),
        };
        if done > end {
            return Err("fence_completion_outside_operation");
        }
        Some(done)
    } else {
        None
    };
    let sd = inside(Kind::VmShootdown, fence_done.unwrap_or(shoot), &|_| true)
        .ok_or("shootdown_outside_operation")?;
    if recs[sd].f[2] != 1 {
        return Err("shootdown_not_acknowledged");
    }
    let old = recs[disp].f[2];
    inside(Kind::VmSettled, sd, &|x| x.f[2] == old).ok_or("settlement_outside_operation")?;
    Ok(Replacement {
        begin,
        end,
        displaced: disp,
        shoot,
        fence_done,
        fenced,
        resident,
    })
}

/// The activation contract, for a target the shootdown found off its hart: that hart activated
/// the target's address space (a `satp` write, whose `sfence.vma` retires every stale
/// translation) after the displacement and before `before` — the target's observation.
pub fn reactivated(recs: &[Rec], target: Role, displaced: usize, before: usize) -> Option<usize> {
    find_from(recs, displaced + 1, |x| {
        x.kind == Kind::Activation && x.cpu == target.cpu && x.f[0] == target.asid
    })
    .filter(|&i| i < before)
}

/// What one serial remote-fence attempt showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct P3Attempt {
    pub generation: u64,
    pub eligible: bool,
    pub reason: &'static str,
}

/// Serial attempt `r`: `q` replaces `t`'s W while `t`, primed after its last kernel entry, spins
/// in its window on its own hart. Pure.
///
/// Obligations (a failure, never ineligibility): the PRE / REQ / readiness / observation steps on
/// their harts in order; the whole production replacement (`replacement`); the observation after
/// the operation (and the fence) completed; monotone residency counts; and, for a target the
/// shootdown found off its hart, the activation evidence (`reactivated`).
///
/// Eligibility — the target's residency over the whole interval — from independent evidence only:
/// the readiness record saw `t` current on its hart; the shootdown's own computation saw `t`
/// current; and `t`'s hart took no supervisor entry between PRE and OBSERVED (its next entry after
/// priming W is the observation's own), so nothing but the requested firmware fence can have
/// retired its translation. Eligible attempts are credited.
pub fn p3_attempt(
    recs: &[Rec],
    roles: &Roles,
    r: u64,
    t: Role,
    q: Role,
    names: (&str, &str, &str),
) -> Result<P3Attempt, &'static str> {
    let (pre_name, req_name, obs_name) = names;
    let pre = user(recs, 0, t, pre_name, Some(r)).ok_or("tlb_pre_missing")?;
    let req = user(recs, pre, q, req_name, Some(r)).ok_or("tlb_request_step_missing")?;
    let rd = ready(recs, req, q.cpu, t.cpu, PHASE_P3, r).ok_or("tlb_ready_missing")?;
    let rep = replacement(recs, roles, rd, q, t)?;
    let observed = user(recs, pre, t, obs_name, Some(r)).ok_or("tlb_observation_missing")?;
    if observed < rep.end || rep.fence_done.is_some_and(|d| observed < d) {
        return Err("tlb_observed_before_completion");
    }
    if recs[observed].cpu != t.cpu || recs[pre].cpu != t.cpu || recs[req].cpu != q.cpu {
        return Err("tlb_roles_off_their_harts");
    }
    let (_, c0) = residency(recs, t, pre_name, r).ok_or("tlb_residency_missing")?;
    let (_, c1) = residency(recs, t, obs_name, r).ok_or("tlb_residency_missing")?;
    if c1 <= c0 {
        return Err("tlb_residency_count_not_monotone");
    }
    if !rep.fenced && reactivated(recs, t, rep.displaced, observed).is_none() {
        return Err("off_cpu_target_not_reactivated");
    }
    let reason = if !ready_met(&recs[rd]) || recs[rd].f[1] != t.tid {
        "not_ready"
    } else if !rep.fenced {
        "off_cpu"
    } else if !rep.resident {
        // Fenced and completed — the obligation held — but the computation did not see the target
        // current on its hart (an empty slot across the dispatch gap, or another task): residency
        // over the interval is not established, so no coverage.
        "not_current"
    } else if c1 != c0 + 1 {
        "interfered"
    } else {
        "none"
    };
    Ok(P3Attempt {
        generation: recs[rep.begin].f[3],
        eligible: reason == "none",
        reason,
    })
}

/// What one fully verified mutual round showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MutualRound {
    /// Both production operations entered before either completed.
    pub overlapped: bool,
    /// Contended spin-lock acquisitions from the first entry to the last exit.
    pub contended: u64,
    /// The two operations: S's (target C), then C's (target S).
    pub s_op: Replacement,
    pub c_op: Replacement,
}

/// Mutual round `m`: S's NR 3 on S's hart replaces C's W while C's NR 3 on C's hart replaces S's.
/// Each operation obeys `replacement`; a target its shootdown found off-CPU must show the
/// activation evidence before its own observation step.
pub fn mutual_round(recs: &[Rec], roles: &Roles, m: u64) -> Result<MutualRound, &'static str> {
    let (s, c) = (roles.s, roles.c);
    let sn = user(recs, 0, s, "S_MUT_NR3", Some(m)).ok_or("mut_request_missing")?;
    let cn = user(recs, 0, c, "C_MUT_NR3", Some(m)).ok_or("mut_request_missing")?;
    let s_op = replacement(recs, roles, sn, s, c).map_err(|_| "mut_s_operation_invalid")?;
    let c_op = replacement(recs, roles, cn, c, s).map_err(|_| "mut_c_operation_invalid")?;
    let so = user(recs, 0, s, "S_MUT_OK", Some(m)).ok_or("mut_observation_missing")?;
    let co = user(recs, 0, c, "C_MUT_OK", Some(m)).ok_or("mut_observation_missing")?;
    if so < c_op.end || co < s_op.end {
        return Err("mut_observed_before_operation_completed");
    }
    // S's operation targets C: C's observation bounds C's reactivation, and vice versa.
    if !s_op.fenced && reactivated(recs, c, s_op.displaced, co).is_none() {
        return Err("off_cpu_target_not_reactivated");
    }
    if !c_op.fenced && reactivated(recs, s, c_op.displaced, so).is_none() {
        return Err("off_cpu_target_not_reactivated");
    }
    let (first, last) = (s_op.begin.min(c_op.begin), s_op.end.max(c_op.end));
    let count_at = |i: usize, phase: u64| {
        find_from(recs, i, |x| x.kind == Kind::Contention && x.f[1] == phase).map(|j| recs[j].f[0])
    };
    let contended = match (count_at(first, 0), count_at(last, 1)) {
        (Some(a), Some(b)) => b.saturating_sub(a),
        _ => 0,
    };
    Ok(MutualRound {
        overlapped: ops_overlap((s_op.begin, s_op.end), (c_op.begin, c_op.end)),
        contended,
        s_op,
        c_op,
    })
}

/// Grade a sealed record. Pure; the kernel runs it at the dump and the grader re-derives it.
pub fn verify(recs: &[Rec], roles: &Roles, overflowed: bool) -> Verdict {
    verify_attempts(recs, roles, overflowed).0
}

/// `verify`, with every attempt it graded.
pub fn verify_attempts(recs: &[Rec], roles: &Roles, overflowed: bool) -> (Verdict, Attempts) {
    let mut v = Verdict {
        records: recs.len(),
        ..Verdict::default()
    };
    let mut att = Attempts::default();
    if overflowed {
        v.fail("record_overflow", 0);
    }
    for r in recs {
        if r.kind == Kind::User && r.f[2] >= STEP_FAIL {
            v.fail("user_step_failed", r.seq);
        }
    }
    match check_ipi_population(recs) {
        Ok(n) => {
            v.ipi_published = n.published;
            v.ipi_fresh = n.fresh;
            v.ipi_requests = n.requests;
            v.ipi_arrivals = n.arrivals;
            v.ipi_consumed = n.consumed;
            v.ipi_empty = n.empty;
            v.ipi_merged = n.merged;
        }
        Err(IpiRefusal::Unexplained { arrival }) => v.fail("ipi_unexplained_arrival", arrival),
        Err(IpiRefusal::GroupInconsistent { publication }) => {
            v.fail("ipi_group_inconsistent", publication)
        }
        Err(IpiRefusal::Undelivered { publication }) => v.fail("ipi_undelivered", publication),
        Err(IpiRefusal::ConsumptionUnbound { record }) => v.fail("ipi_consumption_unbound", record),
        Err(IpiRefusal::DuplicateAttribution { record }) => {
            v.fail("ipi_duplicate_attribution", record)
        }
        Err(IpiRefusal::ArrivalsExceedRequests { cpu }) => {
            v.fail("ipi_arrivals_exceed_requests", u32::from(cpu))
        }
        Err(IpiRefusal::FirmwareRefused { request }) => v.fail("ipi_firmware_refused", request),
    }
    // Each mailbox publication names its target CPU's own hart (CPU ids and hart ids are distinct).
    for r in recs {
        if r.kind == Kind::IpiPublished
            && (r.f[2] == PUB_WAKE || r.f[2] == PUB_KICK)
            && hart_mask_of(roles, 1u64.checked_shl(r.f[0] as u32).unwrap_or(0))
                != 1u64.checked_shl(r.f[1] as u32)
        {
            v.fail("ipi_publication_wrong_hart", r.seq);
        }
    }
    // The soft-float contract, as the interrupted user frames show it: FP and vector Off.
    for r in recs {
        if r.kind == Kind::IpiArrived && arr_origin(r) == ORIGIN_USER {
            if r.f[4] & SSTATUS_FS_VS != 0 {
                v.fail("fp_or_vector_on_in_user", r.seq);
            } else {
                v.user_fp_vs_off += 1;
            }
        }
    }
    let (s, c) = (roles.s, roles.c);

    // P1 — parked targets, both directions, every round accounted for.
    let mut at = 0usize;
    for k in 1..=P1_ROUNDS {
        let step = (|| -> Result<(usize, ParkedRoute, ParkedRoute), &'static str> {
            let call = user(recs, at, c, "C_P1_CALL", Some(k)).ok_or("p1_call_missing")?;
            let (d, to_s) = parked_wake(recs, call, c, s.cpu, s)?;
            let resumed =
                user(recs, d, s, "S_P1_RESUMED", Some(k)).ok_or("p1_server_resume_missing")?;
            if recs[resumed].cpu != s.cpu {
                return Err("p1_server_resumed_elsewhere");
            }
            let reply = user(recs, resumed, s, "S_P1_REPLY", Some(k)).ok_or("p1_reply_missing")?;
            let (d, to_c) = parked_wake(recs, reply, s, c.cpu, c)?;
            let back =
                user(recs, d, c, "C_P1_RESUMED", Some(k)).ok_or("p1_client_resume_missing")?;
            if recs[back].cpu != c.cpu {
                return Err("p1_client_resumed_elsewhere");
            }
            Ok((back, to_s, to_c))
        })();
        match step {
            Ok((end, to_s, to_c)) => {
                v.p1_parked += 2;
                for (route, ipi) in [(to_s, &mut v.p1_ipi_to_s), (to_c, &mut v.p1_ipi_to_c)] {
                    match route {
                        ParkedRoute::Ipi => *ipi += 1,
                        ParkedRoute::TimerFirst => v.p1_timer_first += 1,
                        ParkedRoute::Busy => v.p1_busy += 1,
                        ParkedRoute::Preceded => v.p1_preceded += 1,
                        ParkedRoute::Merged => v.p1_merged += 1,
                    }
                }
                at = end;
            }
            Err(why) => v.fail(why, k as u32),
        }
    }
    // CPU 1 has no timer: every wake of S must be the IPI's. On CPU 0 the timer may win a race it
    // is entitled to win, but not the whole phase.
    if v.p1_ipi_to_s < P1_ROUNDS as usize || v.p1_ipi_to_c < P1_MIN_IPI_TO_C {
        v.fail("p1_too_few_ipi_driven_parked_dispatches", 0);
    }

    // P2 — P2_ATTEMPTS bounded attempts per direction; each is graded for its obligations, then
    // classified eligible or not from independent evidence (`p2_attempt`). Every direction needs
    // P2_MIN_CREDITED_PER_DIRECTION credited attempts.
    for k in 1..=P2_ATTEMPTS {
        for (waker, call, sent, target, ok, window) in [
            (c, "C_P2A_CALL", "C_P2A_SENT", s, "S_WIN_A_OK", WINDOW_S_A),
            (s, "S_P2B_CALL", "S_P2B_SENT", c, "C_WIN_B_OK", WINDOW_C_B),
        ] {
            v.p2_attempts += 1;
            match p2_attempt(recs, at, waker, target, call, sent, ok, window, k) {
                Ok(a) => {
                    match a.class {
                        ArrivalClass::InWindow => v.p2_in_window += 1,
                        ArrivalClass::Outside => v.p2_outside += 1,
                        ArrivalClass::Displaced => v.p2_displaced += 1,
                        ArrivalClass::NotOwed => {}
                    }
                    if a.eligible {
                        if target.tid == s.tid {
                            v.p2_credited_s += 1;
                        } else {
                            v.p2_credited_c += 1;
                        }
                    } else {
                        v.p2_uncredited += 1;
                    }
                    att.push(Attempt {
                        phase: 2,
                        n: k as u8,
                        target_cpu: target.cpu,
                        generation: a.generation,
                        eligible: a.eligible,
                        reason: a.reason,
                        outcome: if a.eligible {
                            Outcome::Credited
                        } else {
                            Outcome::Uncredited
                        },
                    });
                    if target.tid == c.tid {
                        at = a.end;
                    }
                }
                Err(why) => {
                    att.push(Attempt {
                        phase: 2,
                        n: k as u8,
                        target_cpu: target.cpu,
                        generation: 0,
                        eligible: false,
                        reason: why,
                        outcome: Outcome::Failed,
                    });
                    v.fail(why, k as u32);
                }
            }
        }
    }
    if v.p2_credited_s < P2_MIN_CREDITED_PER_DIRECTION
        || v.p2_credited_c < P2_MIN_CREDITED_PER_DIRECTION
    {
        v.fail("p2_too_few_credited_attempts_per_direction", 0);
    }

    // P3 — TLB_ROUNDS bounded serial attempts (odd: S is the target; even: C is).
    for r in 1..=TLB_ROUNDS {
        let (t, q, names) = if r % 2 == 1 {
            (s, c, ("S_PRE", "C_REQ", "S_OBSERVED"))
        } else {
            (c, s, ("C_PRE", "S_REQ", "C_OBSERVED"))
        };
        match p3_attempt(recs, roles, r, t, q, names) {
            Ok(a) => {
                v.tlb_rounds += 1;
                match a.reason {
                    "none" if t.tid == s.tid => v.tlb_credited_s += 1,
                    "none" => v.tlb_credited_c += 1,
                    "interfered" => v.tlb_interfered += 1,
                    "off_cpu" => v.tlb_off_cpu += 1,
                    "not_current" => v.tlb_not_current += 1,
                    _ => v.tlb_not_ready += 1,
                }
                att.push(Attempt {
                    phase: 3,
                    n: r as u8,
                    target_cpu: t.cpu,
                    generation: a.generation,
                    eligible: a.eligible,
                    reason: a.reason,
                    outcome: if a.eligible {
                        Outcome::Credited
                    } else {
                        Outcome::Uncredited
                    },
                });
            }
            Err(why) => {
                att.push(Attempt {
                    phase: 3,
                    n: r as u8,
                    target_cpu: t.cpu,
                    generation: 0,
                    eligible: false,
                    reason: why,
                    outcome: Outcome::Failed,
                });
                v.fail(why, r as u32);
            }
        }
    }
    if v.tlb_credited_s < TLB_MIN_CREDITED_PER_DIRECTION
        || v.tlb_credited_c < TLB_MIN_CREDITED_PER_DIRECTION
    {
        v.fail("tlb_too_few_credited_rounds_per_direction", 0);
    }

    // P4 — mutual rounds, graded on the production operations themselves; every one must verify
    // and overlap (no attempt budget: these are obligations, not coverage).
    for m in 1..=MUT_ROUNDS {
        match mutual_round(recs, roles, m) {
            Ok(round) => {
                v.mutual_rounds += 1;
                v.mutual_overlapped += usize::from(round.overlapped);
                v.mutual_contended += round.contended;
                for (op, target) in [(round.s_op, c), (round.c_op, s)] {
                    v.mutual_off_cpu += usize::from(!op.fenced);
                    v.mutual_not_current += usize::from(op.fenced && !op.resident);
                    att.push(Attempt {
                        phase: 4,
                        n: m as u8,
                        target_cpu: target.cpu,
                        generation: recs[op.begin].f[3],
                        eligible: true,
                        reason: if !op.fenced {
                            "off_cpu"
                        } else if !op.resident {
                            "not_current"
                        } else {
                            "resident"
                        },
                        outcome: Outcome::Verified,
                    });
                }
                if !round.overlapped {
                    v.fail("mut_operations_serialized", m as u32);
                }
            }
            Err(why) => {
                att.push(Attempt {
                    phase: 4,
                    n: m as u8,
                    target_cpu: 0,
                    generation: 0,
                    eligible: false,
                    reason: why,
                    outcome: Outcome::Failed,
                });
                v.fail(why, m as u32)
            }
        }
    }

    // Reclaim only after the shootdown the decision owed completed, exactly once, per displaced
    // page: the recorded decision (which must be the production policy over its own inputs), then
    // — when it named any remote CPU — a SUCCESSFUL firmware fence requested for exactly those CPUs
    // and exactly their harts, then the acknowledgement, then the one settlement.
    for (i, d) in recs.iter().enumerate() {
        if d.kind != Kind::VmDisplaced || d.f[1] != roles.w_va {
            continue;
        }
        let settled: alloc::vec::Vec<usize> = recs
            .iter()
            .enumerate()
            .skip(i + 1)
            .filter(|(_, x)| {
                x.kind == Kind::VmSettled
                    && x.f[0] == d.f[0]
                    && x.f[1] == d.f[1]
                    && x.f[2] == d.f[2]
            })
            .map(|(j, _)| j)
            .collect();
        if settled.len() != 1 {
            v.fail("settlement_not_exactly_once", d.seq);
            continue;
        }
        let st = settled[0];
        let same = |x: &Rec, k: Kind| {
            x.kind == k && x.cpu == d.cpu && x.f[0] == d.f[0] && x.f[1] == d.f[1]
        };
        let Some(shoot) = (i + 1..st).find(|&j| same(&recs[j], Kind::ShootTargets)) else {
            v.fail("settled_without_shootdown_targets", d.seq);
            continue;
        };
        if let Err(why) = check_shoot_decision(recs, roles, shoot, d.cpu) {
            v.fail(why, d.seq);
            continue;
        }
        let Some(ack) =
            (shoot + 1..st).find(|&j| same(&recs[j], Kind::VmShootdown) && recs[j].f[2] == 1)
        else {
            v.fail("settled_before_shootdown_acknowledged", d.seq);
            continue;
        };
        let mask = recs[shoot].f[2];
        if mask == 0 {
            v.settled_local_only += 1;
            continue;
        }
        let need = hart_mask_of(roles, mask);
        let fenced = (shoot + 1..ack).any(|j| {
            same(&recs[j], Kind::FenceRequest)
                && recs[j].f[4] == mask
                && Some(recs[j].f[2]) == need
                && match_fence(recs, j).is_ok_and(|done| done < ack)
        });
        if fenced {
            v.settled_after_completion += 1;
        } else {
            v.fail("settled_before_fence_completion", d.seq);
        }
    }
    if v.tlb_rounds + v.mutual_rounds * 2 > v.settled_after_completion + v.settled_local_only {
        v.fail("displaced_pages_unsettled", 0);
    }
    if user(recs, 0, s, "S_DONE", None).is_none() || user(recs, 0, c, "C_DONE", None).is_none() {
        v.fail("witness_not_complete", 0);
    }
    (v, att)
}

// ─────────────────────────────── the recorder ───────────────────────────────

#[cfg(any(test, all(feature = "riscv64-smp3-witness", target_arch = "riscv64")))]
mod recorder {
    use super::{Kind, Rec, SLOTS};
    use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

    struct Slot {
        state: AtomicU8,
        kind: AtomicU8,
        cpu: AtomicU8,
        f: [AtomicU64; 5],
    }

    #[allow(clippy::declare_interior_mutable_const)]
    const EMPTY: Slot = Slot {
        state: AtomicU8::new(0),
        kind: AtomicU8::new(0),
        cpu: AtomicU8::new(0),
        f: [const { AtomicU64::new(0) }; 5],
    };

    static SLOT: [Slot; SLOTS] = [EMPTY; SLOTS];
    static NEXT: AtomicU32 = AtomicU32::new(0);
    static OVERFLOW: AtomicU32 = AtomicU32::new(0);
    static ARMED: AtomicBool = AtomicBool::new(false);
    static GEN: AtomicU64 = AtomicU64::new(0);

    pub fn arm() {
        ARMED.store(true, Ordering::Release);
    }

    pub fn armed() -> bool {
        ARMED.load(Ordering::Acquire)
    }

    /// A fresh generation for an operation or a fence request.
    pub fn next_generation() -> u64 {
        GEN.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Append one record at the caller's commit point. Inert until armed.
    pub fn push(kind: Kind, cpu: u8, f: [u64; 5]) {
        if !armed() {
            return;
        }
        let i = NEXT.fetch_add(1, Ordering::AcqRel) as usize;
        let Some(slot) = SLOT.get(i) else {
            OVERFLOW.fetch_add(1, Ordering::AcqRel);
            return;
        };
        slot.kind.store(kind as u8, Ordering::Relaxed);
        slot.cpu.store(cpu, Ordering::Relaxed);
        for (dst, v) in slot.f.iter().zip(f) {
            dst.store(v, Ordering::Relaxed);
        }
        slot.state.store(2, Ordering::Release);
    }

    /// The sealed record: every slot claimed so far, once each has been published.
    pub fn seal(out: &mut [Option<Rec>; SLOTS], spins: u32) -> Option<(usize, bool)> {
        let n = (NEXT.load(Ordering::Acquire) as usize).min(SLOTS);
        for (i, dst) in out.iter_mut().enumerate().take(n) {
            let slot = &SLOT[i];
            let mut left = spins;
            while slot.state.load(Ordering::Acquire) != 2 {
                if left == 0 {
                    return None;
                }
                left -= 1;
                core::hint::spin_loop();
            }
            let kind = Kind::from_u8(slot.kind.load(Ordering::Relaxed))?;
            let mut f = [0u64; 5];
            for (d, s) in f.iter_mut().zip(&slot.f) {
                *d = s.load(Ordering::Relaxed);
            }
            *dst = Some(Rec {
                seq: i as u32,
                kind,
                cpu: slot.cpu.load(Ordering::Relaxed),
                f,
            });
        }
        Some((n, OVERFLOW.load(Ordering::Acquire) != 0))
    }
}

#[cfg(any(test, all(feature = "riscv64-smp3-witness", target_arch = "riscv64")))]
pub use recorder::{arm, armed, next_generation, push, seal};

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    type R = (Kind, u8, [u64; 5]);

    fn seqd(v: Vec<R>) -> Vec<Rec> {
        v.into_iter()
            .enumerate()
            .map(|(i, (kind, cpu, f))| Rec {
                seq: i as u32,
                kind,
                cpu,
                f,
            })
            .collect()
    }

    const W: u64 = 0x2008_0000;
    const IDLE: u64 = ORIGIN_IDLE;
    const USER: u64 = ORIGIN_USER;

    fn roles() -> Roles {
        Roles {
            s: Role {
                tid: 9300,
                asid: 11,
                cpu: 1,
                hart: 0,
            },
            c: Role {
                tid: 9301,
                asid: 12,
                cpu: 0,
                hart: 1,
            },
            h1: Role {
                tid: 9302,
                asid: 13,
                cpu: 1,
                hart: 0,
            },
            h0: Role {
                tid: 9303,
                asid: 14,
                cpu: 0,
                hart: 1,
            },
            w_va: W,
        }
    }

    /// The witness's harts: CPU 1 is hart 0, CPU 0 is hart 1 (CPU ids and hart ids differ).
    fn hart(cpu: u64) -> u64 {
        if cpu == 1 { 0 } else { 1 }
    }
    /// A wake publication `from` -> `to`, merged or fresh, in the sender's publication group `g`.
    fn pubrec(from: u8, to: u64, merged: u64, g: u64) -> R {
        (
            Kind::IpiPublished,
            from,
            [to, hart(to), PUB_WAKE, merged, g],
        )
    }
    fn reqrec(from: u8, to: u64, err: u64) -> R {
        (Kind::IpiRequested, from, [to, hart(to), PUB_WAKE, err, 0])
    }
    fn arr(cpu: u8, sources: u64, origin: u64) -> R {
        (Kind::IpiArrived, cpu, [sources, origin, 0, 0, 0])
    }
    /// `cpu`'s `k`-th consumption of source `src` (recorded right after its arrival).
    fn cons(cpu: u8, src: u64, k: u64) -> R {
        (Kind::IpiConsumed, cpu, [src, k, 0, 0, 0])
    }
    fn counts(p: usize, f: usize, m: usize, q: usize, a: usize, e: usize, c: usize) -> IpiCounts {
        IpiCounts {
            published: p,
            fresh: f,
            merged: m,
            requests: q,
            arrivals: a,
            empty: e,
            consumed: c,
        }
    }

    /// One P2 attempt, direction A (C wakes H1 on CPU 1 while S spins in window A), as the owners
    /// record it: C's call step, C's readiness (S current, CPU 1 at `entries` 40), the publication
    /// (group 1) and request, the arrival and its consumption of C's bit, C's post-send step, S's
    /// window check.
    fn p2(ready_cur: u64, arrival: (u64, u64, u64, u64)) -> Vec<R> {
        let (origin, window, entries, tid_asid) = arrival;
        vec![
            (Kind::User, 0, [9301, 12, code("C_P2A_CALL"), 1, 0]),
            (
                Kind::Ready,
                0,
                [
                    1 | (PHASE_P2 << 8) | (1 << 16) | (1 << 17),
                    ready_cur,
                    40,
                    1,
                    77,
                ],
            ),
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            (
                Kind::IpiArrived,
                1,
                [
                    0b1,
                    origin | (window << 8) | (entries << 16),
                    0x376,
                    tid_asid,
                    0,
                ],
            ),
            cons(1, 0, 1),
            (Kind::User, 0, [9301, 12, code("C_P2A_SENT"), 1, 0]),
            (Kind::User, 1, [9300, 11, code("S_WIN_A_OK"), 1, 0]),
        ]
    }
    const S_INC: u64 = 9300 | (11 << 32);

    fn p2a(v: Vec<R>) -> Result<P2Attempt, &'static str> {
        let ro = roles();
        p2_attempt(
            &seqd(v),
            0,
            ro.c,
            ro.s,
            "C_P2A_CALL",
            "C_P2A_SENT",
            "S_WIN_A_OK",
            WINDOW_S_A,
            1,
        )
    }

    /// The helper on the target's hart was NOT parked when readiness was recorded: the production
    /// send queued the call and owed no wake. The attempt is ineligible from that record — its
    /// window check is still owed, no arrival is attributed, and a later attempt's publication is
    /// never borrowed for it.
    #[test]
    fn an_unparked_helper_owes_no_wake_and_the_attempt_is_ineligible() {
        let mut v = p2(9300, (USER, WINDOW_S_A, 41, S_INC));
        v[1].2[0] &= !((1 << 16) | (1 << 17));
        // No wake: drop the publication, the request, the arrival and its consumption.
        v.retain(|(kind, _, _)| {
            !matches!(
                kind,
                Kind::IpiPublished | Kind::IpiRequested | Kind::IpiArrived | Kind::IpiConsumed
            )
        });
        let a = p2a(v.clone()).expect("graded, not failed");
        assert_eq!(
            (a.eligible, a.reason, a.class),
            (false, "helper_not_parked", ArrivalClass::NotOwed)
        );
        // The window check is still an obligation.
        let mut w = v.clone();
        w.pop();
        assert_eq!(p2a(w), Err("p2_window_check_missing"));
        // Parked, but the only publication comes AFTER the waker's post-send step (a later
        // attempt's): not this attempt's wake.
        let mut late = p2(9300, (USER, WINDOW_S_A, 41, S_INC));
        let publ = late.remove(2);
        late.insert(6, publ);
        assert_eq!(p2a(late), Err("ipi_not_published"));
    }

    /// Readiness is both recorded facts. A bounded wait that timed out is not readiness even if its
    /// last look saw the target current — the attempt is uncredited (`not_ready`), and a failed
    /// window check is then not an obligation it owed.
    #[test]
    fn a_timed_out_readiness_wait_is_not_ready_even_when_it_saw_the_target() {
        let mut v = p2(9300, (USER, WINDOW_NONE, 41, S_INC));
        v[1].2[0] &= !(1 << 16);
        let a = p2a(v).expect("graded, not failed");
        assert!(!a.eligible);
        assert_eq!(a.reason, "not_ready");
        let mut w = p3(9300, op(s_by_c()), 10, 11);
        w[3].2[0] &= !(1 << 16);
        let b = p3a(w).expect("graded");
        assert_eq!((b.eligible, b.reason), (false, "not_ready"));
    }

    #[test]
    fn an_eligible_p2_attempt_is_credited_only_inside_the_exact_targets_window() {
        let ok = p2a(p2(9300, (USER, WINDOW_S_A, 41, S_INC))).expect("graded");
        assert!(ok.eligible && ok.class == ArrivalClass::InWindow && ok.generation == 77);
        // Eligible (ready, next entry, no switch) but outside the window: a failure, not a skip.
        assert_eq!(
            p2a(p2(9300, (USER, WINDOW_NONE, 41, S_INC))),
            Err("ipi_sepc_outside_window")
        );
        // Eligible, and the arrival names the same tid in ANOTHER address space: not the target's
        // incarnation, so a failure even at a window PC.
        assert_eq!(
            p2a(p2(9300, (USER, WINDOW_S_A, 41, 9300 | (99 << 32)))),
            Err("ipi_eligible_arrival_not_in_target")
        );
        // Eligible, and the arrival landed in another task or at idle: a failure.
        assert_eq!(
            p2a(p2(9300, (USER, WINDOW_NONE, 41, 2 | (2 << 32)))),
            Err("ipi_eligible_arrival_not_in_target")
        );
        assert_eq!(
            p2a(p2(9300, (IDLE, WINDOW_NONE, 41, 0))),
            Err("ipi_eligible_arrival_not_in_target")
        );
    }

    #[test]
    fn a_p2_attempt_is_ineligible_only_on_independent_evidence_and_earns_nothing() {
        // Another entry on the target hart between readiness and the arrival (the tick): the
        // arrival outside the window is COUNTED apart, not failed, and credits nothing.
        let a = p2a(p2(9300, (USER, WINDOW_NONE, 42, S_INC))).expect("graded");
        assert!(!a.eligible && a.reason == "intervening_entry" && a.class == ArrivalClass::Outside);
        // Readiness did not see the target current.
        let a = p2a(p2(2, (USER, WINDOW_NONE, 41, 2 | (2 << 32)))).expect("graded");
        assert!(!a.eligible && a.reason == "not_ready" && a.class == ArrivalClass::Displaced);
        // Another address space was activated on the target hart after the call step.
        let mut v = p2(9300, (USER, WINDOW_NONE, 41, 2 | (2 << 32)));
        v.insert(
            3,
            (Kind::Activation, 1, [2, 0x8000_2000_0000_0001, 0, 0, 0]),
        );
        let a = p2a(v).expect("graded");
        assert!(!a.eligible && a.reason == "switched");
        // ... but an activation of the TARGET's own address space is no evidence of displacement.
        let mut v = p2(9300, (USER, WINDOW_S_A, 41, S_INC));
        v.insert(
            3,
            (Kind::Activation, 1, [11, 0x8000_b000_0000_0001, 0, 0, 0]),
        );
        assert!(p2a(v).expect("graded").eligible);
        // An entry count that went BACKWARDS is not history, it is a contradiction.
        assert_eq!(
            p2a(p2(9300, (USER, WINDOW_S_A, 40, S_INC))),
            Err("p2_entry_history_inconsistent")
        );
    }

    #[test]
    fn a_p2_attempt_fails_on_missing_evidence_or_progress_never_skips_it() {
        // No readiness record.
        let mut v = p2(9300, (USER, WINDOW_S_A, 41, S_INC));
        v.remove(1);
        assert_eq!(p2a(v), Err("p2_ready_missing"));
        // The IPI suppressed: published, never consumed.
        let mut v = p2(9300, (USER, WINDOW_S_A, 41, S_INC));
        v.drain(4..6);
        assert_eq!(p2a(v), Err("ipi_not_consumed"));
        // No window check after the arrival (a corrupted context reports a FAIL step instead).
        let mut v = p2(9300, (USER, WINDOW_S_A, 41, S_INC));
        v.pop();
        assert_eq!(p2a(v), Err("p2_window_check_missing"));
        // The waker's post-send step missing.
        let mut v = p2(9300, (USER, WINDOW_S_A, 41, S_INC));
        v.remove(6);
        assert_eq!(p2a(v), Err("p2_sent_missing"));
    }

    /// SMP3-EVIDENCE1 §3 — a wake is attributed to its arrival through its publication group, in
    /// whatever order the two sides' records landed, and a merged wake earns no coverage.
    #[test]
    fn a_p2_wake_is_attributed_by_group_not_by_record_order() {
        // The publication record lands after the arrival that consumed its group (the target swapped
        // between the sender's `fetch_or` and its record): still this wake's arrival, credited.
        let mut v = p2(9300, (USER, WINDOW_S_A, 41, S_INC));
        let (p, q) = (v.remove(2), v.remove(2));
        v.insert(4, q);
        v.insert(4, p);
        assert!(p2a(v.clone()).expect("graded").eligible);
        assert!(check_ipi_population(&seqd(v)).is_ok());
        // The wake MERGED into a group an earlier publication from C opened: its arrival is not
        // uniquely this wake's, so the attempt is uncredited — never failed, never credited.
        let mut v = p2(9300, (USER, WINDOW_S_A, 41, S_INC));
        v[2] = pubrec(0, 1, 1, 1);
        v.insert(0, reqrec(0, 1, 0));
        v.insert(0, pubrec(0, 1, 0, 1));
        let a = p2a(v.clone()).expect("graded");
        assert_eq!((a.eligible, a.reason), (false, "merged_publication"));
        assert_eq!(
            check_ipi_population(&seqd(v)),
            Ok(counts(2, 1, 1, 2, 1, 0, 1))
        );
    }

    #[test]
    fn a_publication_is_consumed_exactly_once() {
        let r = seqd(vec![
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
        ]);
        assert_eq!(check_ipi_population(&r), Ok(counts(1, 1, 0, 1, 1, 0, 1)));
    }

    #[test]
    fn requests_merging_into_one_arrival_are_not_loss() {
        // Two publications (the second merged into the first's group), two requests, one arrival
        // consuming the group.
        let mut v = vec![
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            pubrec(0, 1, 1, 1),
            reqrec(0, 1, 0),
            arr(1, 0b1, USER),
            cons(1, 0, 1),
        ];
        assert_eq!(
            check_ipi_population(&seqd(v.clone())),
            Ok(counts(2, 1, 1, 2, 1, 0, 1))
        );
        // ... and an empty arrival later (the second SSIP after the consumption) is not duplication.
        v.push(arr(1, 0, USER));
        assert_eq!(
            check_ipi_population(&seqd(v)),
            Ok(counts(2, 1, 1, 2, 2, 1, 1))
        );
    }

    /// SMP3-EVIDENCE1 §1 B — the legal interleaving the delivered verifier refused: C's `fetch_or`
    /// found its bit set (merged into group 1), then S's `swap` consumed group 1 and S pushed its
    /// arrival and consumption records, and only then did C push its merged publication record.
    /// Every operation is accounted for exactly once.
    #[test]
    fn a_merged_publication_recorded_after_the_arrival_that_consumed_its_group_is_legal() {
        let r = seqd(vec![
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
            pubrec(0, 1, 1, 1),
            reqrec(0, 1, 0),
        ]);
        assert_eq!(check_ipi_population(&r), Ok(counts(2, 1, 1, 2, 1, 0, 1)));
        // ... the same for a fresh publication recorded after the arrival that consumed it.
        let r = seqd(vec![
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
        ]);
        assert_eq!(check_ipi_population(&r), Ok(counts(1, 1, 0, 1, 1, 0, 1)));
    }

    /// AP-SCHED3's case, now from the protocol rather than an allowance: CPU 1 swapped group 1, CPU
    /// 0 published again (fresh, group 2: the bit was clear) and recorded it, and only then did CPU 1
    /// push its arrival record; a second arrival consumed group 2.
    #[test]
    fn a_fresh_publication_after_a_consumption_opens_a_new_group() {
        let r = seqd(vec![
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            pubrec(0, 1, 0, 2),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
            arr(1, 0b1, USER),
            cons(1, 0, 2),
        ]);
        assert_eq!(check_ipi_population(&r), Ok(counts(2, 2, 0, 2, 2, 0, 2)));
        // Without the second consumption, group 2 is undelivered (a lost publication).
        let r = seqd(vec![
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            pubrec(0, 1, 0, 2),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
        ]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::Undelivered { publication: 2 })
        );
    }

    #[test]
    fn both_directions_are_accounted_independently() {
        let r = seqd(vec![
            pubrec(0, 1, 0, 1),
            pubrec(1, 0, 0, 1),
            reqrec(0, 1, 0),
            reqrec(1, 0, 0),
            arr(0, 0b10, USER),
            cons(0, 1, 1),
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
            pubrec(1, 0, 0, 2),
            reqrec(1, 0, 0),
            arr(0, 0b10, IDLE),
            cons(0, 1, 2),
        ]);
        assert_eq!(check_ipi_population(&r), Ok(counts(3, 3, 0, 3, 3, 0, 3)));
    }

    #[test]
    fn an_unexplained_consumption_fails() {
        let r = seqd(vec![reqrec(0, 1, 0), arr(1, 0b1, IDLE), cons(1, 0, 1)]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::Unexplained { arrival: 1 })
        );
        // A consumption of the WRONG source is unexplained too.
        let r = seqd(vec![
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            arr(1, 0b10, IDLE),
            cons(1, 1, 1),
        ]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::Unexplained { arrival: 2 })
        );
    }

    #[test]
    fn consumption_records_bind_one_for_one_to_their_arrival() {
        // An arrival that consumed a bit with no consumption record after it.
        let r = seqd(vec![pubrec(0, 1, 0, 1), reqrec(0, 1, 0), arr(1, 0b1, IDLE)]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::ConsumptionUnbound { record: 2 })
        );
        // ... or with another record of that CPU in between.
        let r = seqd(vec![
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            (Kind::IdleDispatch, 1, [9300, 1, 0, 0, 0]),
            cons(1, 0, 1),
        ]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::ConsumptionUnbound { record: 2 })
        );
        // A consumption naming a source the arrival did not consume.
        let r = seqd(vec![
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            cons(1, 1, 1),
        ]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::ConsumptionUnbound { record: 3 })
        );
        // A consumption that skipped an ordinal.
        let r = seqd(vec![
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            cons(1, 0, 2),
        ]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::ConsumptionUnbound { record: 3 })
        );
    }

    #[test]
    fn a_group_attributed_twice_fails() {
        let r = seqd(vec![
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
            pubrec(0, 1, 0, 2),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
        ]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::DuplicateAttribution { record: 7 })
        );
    }

    #[test]
    fn a_suppressed_notification_is_undelivered() {
        // Published and never consumed (the firmware call was suppressed).
        let r = seqd(vec![pubrec(0, 1, 0, 1)]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::Undelivered { publication: 0 })
        );
    }

    #[test]
    fn more_arrivals_than_requests_fail() {
        // An SSIP never cleared re-traps: the second arrival has no request behind it.
        let r = seqd(vec![
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
            arr(1, 0, IDLE),
        ]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::ArrivalsExceedRequests { cpu: 1 })
        );
    }

    #[test]
    fn inconsistent_or_stale_group_accounting_fails() {
        // A merged publication with no group opened.
        let r = seqd(vec![pubrec(0, 1, 1, 0)]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::GroupInconsistent { publication: 0 })
        );
        // A fresh publication that does not open the next group (skipped, or a stale repeat).
        let r = seqd(vec![pubrec(0, 1, 0, 2)]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::GroupInconsistent { publication: 0 })
        );
        let r = seqd(vec![pubrec(0, 1, 0, 1), pubrec(0, 1, 0, 1)]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::GroupInconsistent { publication: 1 })
        );
        // A merged publication outside the sender's current group.
        let r = seqd(vec![
            pubrec(0, 1, 0, 1),
            pubrec(0, 1, 0, 2),
            pubrec(0, 1, 1, 1),
        ]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::GroupInconsistent { publication: 2 })
        );
    }

    #[test]
    fn a_failed_firmware_request_fails() {
        let r = seqd(vec![pubrec(0, 1, 0, 1), reqrec(0, 1, (-2i64) as u64)]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::FirmwareRefused { request: 1 })
        );
    }

    fn fence(cpu: u8, generation: u64, err: u64) -> Vec<R> {
        vec![
            (Kind::FenceRequest, cpu, [11, W, 0b1, generation, 0b10]),
            (Kind::FenceDone, cpu, [11, W, 0b1, generation, err]),
        ]
    }

    #[test]
    fn a_fence_completion_is_credited_only_to_its_own_request() {
        let r = seqd(fence(0, 3, 0));
        assert_eq!(match_fence(&r, 0), Ok(1));
        let r = seqd(vec![
            (Kind::FenceRequest, 0, [11, W, 0b1, 4, 0b10]),
            (Kind::FenceDone, 0, [11, W, 0b1, 3, 0]),
            (Kind::FenceDone, 0, [11, W, 0b1, 4, 0]),
        ]);
        assert_eq!(match_fence(&r, 0), Err(FenceRefusal::StaleGeneration));
        let r = seqd(vec![
            (Kind::FenceRequest, 0, [11, W, 0b1, 4, 0b10]),
            (Kind::FenceDone, 0, [11, W, 0b1, 4, 0]),
            (Kind::FenceDone, 0, [11, W, 0b1, 4, 0]),
        ]);
        assert_eq!(match_fence(&r, 0), Err(FenceRefusal::Duplicate));
        // Another CPU's completion, or another mapping's, does not count.
        let r = seqd(vec![
            (Kind::FenceRequest, 0, [11, W, 0b1, 4, 0b10]),
            (Kind::FenceDone, 1, [11, W, 0b1, 4, 0]),
            (Kind::FenceDone, 0, [12, W, 0b1, 4, 0]),
        ]);
        assert_eq!(match_fence(&r, 0), Err(FenceRefusal::Missing));
        // A firmware error is never completion.
        let r = seqd(fence(0, 5, (-3i64) as u64));
        assert_eq!(match_fence(&r, 0), Err(FenceRefusal::Failed));
    }

    /// One production replacement as its owners record it: the requester on `req_cpu` replaces the
    /// W of address space `asid`; its shootdown decision consumed `online` / `wake_only`, computed
    /// the `placement` and loaded-root `loaded` contributions, read `cur` as CPU 0's / CPU 1's
    /// current tid, and named `mask`; when `fence`, it asked the firmware to fence `fence_cpus` on
    /// `fence_harts` and that completed.
    #[derive(Clone, Copy)]
    struct Op {
        req_cpu: u8,
        req_tid: u64,
        asid: u64,
        generation: u64,
        online: u64,
        wake_only: u64,
        placement: u64,
        loaded: u64,
        mask: u64,
        cur: (u64, u64),
        fence: bool,
        fence_cpus: u64,
        fence_harts: u64,
    }

    /// C (CPU 0) replaces S's W while S is current on CPU 1 (hart 0) with its root loaded.
    fn s_by_c() -> Op {
        Op {
            req_cpu: 0,
            req_tid: 9301,
            asid: 11,
            generation: 1,
            online: 0b11,
            wake_only: 0,
            placement: 0b10,
            loaded: 0b10,
            mask: 0b10,
            cur: (9301, 9300),
            fence: true,
            fence_cpus: 0b10,
            fence_harts: 0b01,
        }
    }

    /// ... with S gone from CPU 1: H1 (another address space) current there, S's root unloaded and
    /// flushed by the idle-root handoff. Nothing is named; no fence.
    fn s_left() -> Op {
        Op {
            placement: 0,
            loaded: 0,
            mask: 0,
            cur: (9301, 9302),
            fence: false,
            ..s_by_c()
        }
    }

    /// Indices: 0 begin, 1 displaced, 2 inputs, 3 targets, [4 fence request, 5 fence done], then
    /// the acknowledged shootdown, the settlement and the completion.
    fn op(o: Op) -> Vec<R> {
        let (q, a) = (o.req_cpu, o.asid);
        let mut v = vec![
            (Kind::VmOpBegin, q, [a, W, 0x1000, o.generation, o.req_tid]),
            (Kind::VmDisplaced, q, [a, W, 0xAAA000, 0, 0]),
            (
                Kind::ShootInputs,
                q,
                [a, W, o.online | (o.wake_only << 32), o.placement, o.loaded],
            ),
            (Kind::ShootTargets, q, [a, W, o.mask, o.cur.0, o.cur.1]),
        ];
        if o.fence {
            let g = o.generation + 100;
            v.push((
                Kind::FenceRequest,
                q,
                [a, W, o.fence_harts, g, o.fence_cpus],
            ));
            v.push((Kind::FenceDone, q, [a, W, o.fence_harts, g, 0]));
        }
        v.push((Kind::VmShootdown, q, [a, W, 1, 0, 0]));
        v.push((Kind::VmSettled, q, [a, W, 0xAAA000, 0, 0]));
        v.push((Kind::VmOpEnd, q, [a, W, o.generation, 0, W]));
        v
    }

    fn repl_of(v: Vec<R>) -> Result<Replacement, &'static str> {
        let ro = roles();
        replacement(&seqd(v), &ro, 0, ro.c, ro.s)
    }

    #[test]
    fn a_replacement_needs_the_fence_to_every_named_cpu_completed_before_settlement() {
        // C (cpu 0) replaces S's W; S is current on cpu 1 = hart 0.
        let r = repl_of(op(s_by_c())).expect("verified");
        assert!(r.fenced && r.resident && r.fence_done.is_some());
        // The fence named the wrong hart: CPU 1's index taken for a hart id names hart 1 — the
        // requester's own — and any other hart is not the target's.
        let wrong = Op {
            fence_harts: 0b10,
            ..s_by_c()
        };
        assert_eq!(repl_of(op(wrong)), Err("requester_fenced_itself"));
        let wrong = Op {
            fence_harts: 0b100,
            ..s_by_c()
        };
        assert_eq!(repl_of(op(wrong)), Err("fence_harts_not_the_targets"));
        // The fence named other CPUs than the decision did (a substituted target set).
        let other = Op {
            fence_cpus: 0b01,
            ..s_by_c()
        };
        assert_eq!(repl_of(op(other)), Err("fence_targets_not_the_decision"));
        // The requester's own hart in the request.
        let own = Op {
            fence_harts: 0b11,
            ..s_by_c()
        };
        assert_eq!(repl_of(op(own)), Err("requester_fenced_itself"));
        // The decision named S's CPU but the fence was omitted altogether: refused — a named CPU
        // missing its fence is a FAILURE, never an ineligible attempt.
        let mut v = op(s_by_c());
        v.drain(4..6);
        assert_eq!(repl_of(v), Err("fence_not_requested_for_the_targets"));
        // The completion withheld (firmware error): refused, never settled as complete.
        let mut v = op(s_by_c());
        v[5].2[4] = (-1i64) as u64;
        assert_eq!(repl_of(v), Err("fence_failed"));
        // The completion missing, duplicated, or substituted by another request's generation.
        let mut v = op(s_by_c());
        v.remove(5);
        assert_eq!(repl_of(v), Err("fence_completion_missing"));
        let mut v = op(s_by_c());
        v.insert(6, v[5]);
        assert_eq!(repl_of(v), Err("fence_duplicate_completion"));
        let mut v = op(s_by_c());
        v[5].2[3] += 1;
        assert_eq!(repl_of(v), Err("fence_stale_completion"));
        // Settlement before the fence completed (premature release).
        let mut v = op(s_by_c());
        let settled = v.remove(7);
        v.insert(4, settled);
        assert!(repl_of(v).is_err());
    }

    /// SMP3-EVIDENCE1 §1 A — the dispatch gap: CPU 1's `current` slot was EMPTY at the decision
    /// while S's root stayed loaded there. Production names CPU 1 by residency; the fence and its
    /// completion are owed and verified. The delivered verifier derived "resident" from `current`
    /// and refused this as inconsistent.
    #[test]
    fn a_loaded_root_with_no_current_task_is_fenced_and_owed_its_completion() {
        let gap = Op {
            placement: 0,
            cur: (9301, 0),
            ..s_by_c()
        };
        let r = repl_of(op(gap)).expect("verified");
        assert!(r.fenced && !r.resident && r.fence_done.is_some());
        // Coverage is separate: fenced and completed, but S was not current — uncredited.
        let a = p3a(p3(9300, op(gap), 10, 11)).expect("graded");
        assert_eq!((a.eligible, a.reason), (false, "not_current"));
        // The empty slot does not discharge the obligation: no fence fails...
        let mut v = op(gap);
        v.drain(4..6);
        assert_eq!(repl_of(v), Err("fence_not_requested_for_the_targets"));
        // ... a target set that drops the loaded-root CPU is not the policy...
        let dropped = Op {
            mask: 0,
            fence: false,
            ..gap
        };
        assert_eq!(repl_of(op(dropped)), Err("shoot_targets_not_the_policy"));
        // ... and an empty CPU contributes no placement.
        let placed = Op {
            placement: 0b10,
            ..gap
        };
        assert_eq!(repl_of(op(placed)), Err("shoot_placement_inconsistent"));
    }

    /// SMP3-EVIDENCE1 §1 A — another task of the SAME address space current on the target's CPU:
    /// named (by placement and residency), fenced, and uncredited for S.
    #[test]
    fn another_task_of_the_address_space_is_fenced_and_earns_no_coverage_for_the_target() {
        let other = Op {
            cur: (9301, 9399),
            ..s_by_c()
        };
        let r = repl_of(op(other)).expect("verified");
        assert!(r.fenced && !r.resident);
        let a = p3a(p3(9300, op(other), 10, 11)).expect("graded");
        assert_eq!((a.eligible, a.reason), (false, "not_current"));
        // Dropping it from the target set is not the policy, whatever `current` said.
        let dropped = Op {
            mask: 0,
            fence: false,
            ..other
        };
        assert_eq!(repl_of(op(dropped)), Err("shoot_targets_not_the_policy"));
    }

    #[test]
    fn the_decision_must_be_the_policy_over_its_own_recorded_inputs() {
        // Placement only (a root not yet recorded loaded), residency only (the gap), and both.
        let placement_only = Op {
            loaded: 0,
            ..s_by_c()
        };
        assert!(repl_of(op(placement_only)).is_ok_and(|r| r.fenced && r.resident));
        assert!(repl_of(op(s_by_c())).is_ok());
        // A CPU genuinely gone: nothing loaded, another address space current — excluded.
        let r = repl_of(op(s_left())).expect("verified");
        assert!(!r.fenced && !r.resident && r.fence_done.is_none());
        // A named CPU neither contribution names.
        let extra = Op {
            mask: 0b10,
            fence: true,
            ..s_left()
        };
        assert_eq!(repl_of(op(extra)), Err("shoot_targets_not_the_policy"));
        // Contributions outside the candidates (wake-only, or offline).
        let wake_only = Op {
            wake_only: 0b10,
            ..s_by_c()
        };
        assert_eq!(
            repl_of(op(wake_only)),
            Err("shoot_inputs_outside_candidates")
        );
        let offline = Op {
            online: 0b01,
            ..s_by_c()
        };
        assert_eq!(repl_of(op(offline)), Err("shoot_inputs_outside_candidates"));
        // The requester named itself.
        let own = Op {
            mask: 0b11,
            loaded: 0b11,
            ..s_by_c()
        };
        assert_eq!(repl_of(op(own)), Err("shoot_targets_named_requester"));
        // A known task of ANOTHER address space contributing placement, or S's own CPU with S
        // current contributing none.
        let wrong = Op {
            cur: (9301, 9302),
            ..s_by_c()
        };
        assert_eq!(repl_of(op(wrong)), Err("shoot_placement_inconsistent"));
        let omitted = Op {
            placement: 0,
            ..s_by_c()
        };
        assert_eq!(repl_of(op(omitted)), Err("shoot_placement_inconsistent"));
        // The inputs missing, unrecorded, or for another mapping.
        let mut v = op(s_by_c());
        v.remove(2);
        assert_eq!(repl_of(v), Err("shoot_inputs_missing"));
        let mut v = op(s_by_c());
        v[2].2[4] = u64::MAX;
        assert_eq!(repl_of(v), Err("shoot_inputs_unrecorded"));
        let mut v = op(s_by_c());
        v[2].2[1] = W + 0x1000;
        assert_eq!(repl_of(v), Err("shoot_inputs_missing"));
        // The decision recorded by another CPU than the operation's requester is not its decision.
        let mut v = op(s_by_c());
        v[2].1 = 1;
        v[3].1 = 1;
        assert_eq!(repl_of(v), Err("shoot_targets_missing"));
    }

    /// One whole serial attempt r = 1 (target S on CPU 1, requester C on CPU 0).
    fn p3(ready_cur: u64, op: Vec<R>, c0: u64, c1: u64) -> Vec<R> {
        let mut v = vec![
            (Kind::User, 1, [9300, 11, code("S_PRE"), 1, 0]),
            (Kind::Residency, 1, [9300, c0, code("S_PRE"), 1, 0]),
            (Kind::User, 0, [9301, 12, code("C_REQ"), 1, 0]),
            (
                Kind::Ready,
                0,
                [1 | (PHASE_P3 << 8) | (1 << 16), ready_cur, c0, 1, 5],
            ),
        ];
        v.extend(op);
        v.push((Kind::User, 1, [9300, 11, code("S_OBSERVED"), 1, 0]));
        v.push((Kind::Residency, 1, [9300, c1, code("S_OBSERVED"), 1, 0]));
        v
    }

    fn p3a(v: Vec<R>) -> Result<P3Attempt, &'static str> {
        let ro = roles();
        p3_attempt(
            &seqd(v),
            &ro,
            1,
            ro.s,
            ro.c,
            ("S_PRE", "C_REQ", "S_OBSERVED"),
        )
    }

    #[test]
    fn a_serial_attempt_is_credited_only_with_residency_over_the_whole_interval() {
        let a = p3a(p3(9300, op(s_by_c()), 10, 11)).expect("graded");
        assert!(a.eligible && a.reason == "none" && a.generation == 1);
        // Resident at the computation and fenced, but the hart entered the kernel in between.
        let a = p3a(p3(9300, op(s_by_c()), 10, 14)).expect("graded");
        assert!(!a.eligible && a.reason == "interfered");
        // Readiness never saw the target current.
        let a = p3a(p3(2, op(s_by_c()), 10, 14)).expect("graded");
        assert!(!a.eligible && a.reason == "not_ready");
        // The counts went nowhere: not history.
        assert_eq!(
            p3a(p3(9300, op(s_by_c()), 10, 10)),
            Err("tlb_residency_count_not_monotone")
        );
        // An invalid operation fails before coverage is considered.
        let mut v = op(s_by_c());
        v.drain(4..6);
        assert_eq!(
            p3a(p3(9300, v, 10, 11)),
            Err("fence_not_requested_for_the_targets")
        );
    }

    #[test]
    fn an_excluded_target_is_uncredited_only_with_positive_activation_evidence() {
        // Excluded at the decision, and its hart re-activated S's address space (the satp write
        // whose sfence retires every stale translation) after the displacement, before OBSERVED.
        let mut o = op(s_left());
        o.insert(
            4,
            (Kind::Activation, 1, [11, 0x8000_b000_0000_0001, 0, 0, 0]),
        );
        let a = p3a(p3(9300, o, 10, 15)).expect("graded");
        assert!(!a.eligible && a.reason == "off_cpu");
        // ... without that activation: a FAILURE, not an uncredited attempt.
        assert_eq!(
            p3a(p3(9300, op(s_left()), 10, 15)),
            Err("off_cpu_target_not_reactivated")
        );
        // ... an activation of ANOTHER address space is not the target's.
        let mut o = op(s_left());
        o.insert(
            4,
            (Kind::Activation, 1, [2, 0x8000_2000_0000_0001, 0, 0, 0]),
        );
        assert_eq!(
            p3a(p3(9300, o, 10, 15)),
            Err("off_cpu_target_not_reactivated")
        );
        // ... nor is an activation BEFORE the displacement (the new PTE was not yet written).
        let mut o = op(s_left());
        o.insert(
            1,
            (Kind::Activation, 1, [11, 0x8000_b000_0000_0001, 0, 0, 0]),
        );
        assert_eq!(
            p3a(p3(9300, o, 10, 15)),
            Err("off_cpu_target_not_reactivated")
        );
        // ... nor is one on the requester's hart.
        let mut o = op(s_left());
        o.insert(
            4,
            (Kind::Activation, 0, [11, 0x8000_b000_0000_0001, 0, 0, 0]),
        );
        assert_eq!(
            p3a(p3(9300, o, 10, 15)),
            Err("off_cpu_target_not_reactivated")
        );
    }

    /// A displaced page, its decision (inputs and targets), the acknowledgement and the settlement:
    /// indices 0 displaced, 1 inputs, 2 targets, 3 acknowledgement, 4 settlement.
    fn v_premature(o: Op) -> Vec<R> {
        let mut v = op(o);
        v.retain(|(k, _, _)| {
            matches!(
                k,
                Kind::VmDisplaced
                    | Kind::ShootInputs
                    | Kind::ShootTargets
                    | Kind::VmShootdown
                    | Kind::VmSettled
            )
        });
        v
    }

    fn settled(v: Vec<R>) -> (usize, usize) {
        let verdict = verify(&seqd(v), &roles(), false);
        assert!(verdict.failure.is_some());
        (verdict.settled_after_completion, verdict.settled_local_only)
    }

    #[test]
    fn the_premature_release_check_is_global() {
        // The global check, read off its own counts (the verdict's reported failure is the FIRST
        // one, which for a record with no P1 phase is the missing call): a settlement with no
        // completed fence for a decision that named a remote CPU is not counted...
        assert_eq!(settled(v_premature(s_by_c())), (0, 0));
        let fenced = |cpus: u64, harts: u64, err: u64| {
            let mut v = v_premature(s_by_c());
            v.insert(3, (Kind::FenceDone, 0, [11, W, harts, 9, err]));
            v.insert(3, (Kind::FenceRequest, 0, [11, W, harts, 9, cpus]));
            v
        };
        // ... the same settlement after a successful fence for exactly that CPU and its hart is...
        assert_eq!(settled(fenced(0b10, 0b01, 0)), (1, 0));
        // ... a FAILED fence never is, nor one for other CPUs or other harts...
        assert_eq!(settled(fenced(0b10, 0b01, (-1i64) as u64)), (0, 0));
        assert_eq!(settled(fenced(0b11, 0b01, 0)), (0, 0));
        assert_eq!(settled(fenced(0b10, 0b10, 0)), (0, 0));
        // ... and a decision that named no remote CPU needs none (the local fence and every other
        // hart's next activation are the whole shootdown).
        assert_eq!(settled(v_premature(s_left())), (0, 1));
        // A decision that is not the policy over its inputs is never counted.
        let short = Op {
            mask: 0,
            fence: false,
            ..s_by_c()
        };
        assert_eq!(settled(v_premature(short)), (0, 0));
        // A settlement with no recorded computation at all is never counted.
        let mut none = v_premature(s_left());
        none.drain(1..3);
        assert_eq!(settled(none), (0, 0));
    }

    #[test]
    fn every_attempt_is_retained_and_all_ineligible_attempts_fail_coverage() {
        // An empty record grades nothing: every attempt fails on missing evidence, and the
        // coverage obligations fail too — ineligibility can never stand in for coverage.
        let (v, att) = verify_attempts(&[], &roles(), false);
        assert!(v.failure.is_some());
        assert_eq!(
            att.iter().filter(|a| a.phase == 2).count(),
            2 * P2_ATTEMPTS as usize
        );
        assert_eq!(
            att.iter().filter(|a| a.phase == 3).count(),
            TLB_ROUNDS as usize
        );
        assert!(att.iter().all(|a| a.outcome == Outcome::Failed));
        assert_eq!(
            v.p2_credited_s + v.p2_credited_c + v.tlb_credited_s + v.tlb_credited_c,
            0
        );
    }

    #[test]
    fn a_parked_wake_is_classified_by_what_actually_ran() {
        let ro = roles();
        // C calls; IPI to CPU 1 taken at idle; idle dispatch of S driven by the IPI.
        let r = seqd(vec![
            (Kind::User, 0, [9301, 12, code("C_P1_CALL"), 1, 0]),
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
            (Kind::IdleDispatch, 1, [9300, 1, 0, 0, 0]),
        ]);
        assert_eq!(parked_wake(&r, 0, ro.c, 1, ro.s), Ok((5, ParkedRoute::Ipi)));
        // The IPI's idle advance selected another runnable task first: preceded, not credited.
        let mut s = r.clone();
        s[5].f[0] = 9302;
        assert_eq!(
            parked_wake(&s, 0, ro.c, 1, ro.s),
            Ok((5, ParkedRoute::Preceded))
        );
        // ... but an idle advance that dispatched nothing is a substitution.
        let mut z = r.clone();
        z[5].f[0] = 0;
        assert_eq!(
            parked_wake(&z, 0, ro.c, 1, ro.s),
            Err("ipi_dispatch_substituted")
        );
        // The dispatch was the timer's, not the IPI's, after an idle arrival: substitution.
        let mut t = r.clone();
        t[5].f[1] = 0;
        assert_eq!(
            parked_wake(&t, 0, ro.c, 1, ro.s),
            Err("ipi_dispatch_substituted")
        );
        // Never consumed (clearing omitted and the publication lost, or notification suppressed).
        let u = seqd(vec![
            (Kind::User, 0, [9301, 12, code("C_P1_CALL"), 1, 0]),
            pubrec(0, 1, 0, 1),
        ]);
        assert_eq!(parked_wake(&u, 0, ro.c, 1, ro.s), Err("ipi_not_consumed"));
    }

    /// SMP3-EVIDENCE1 §3 — parked wakes attributed by group: a delayed publication record is still
    /// this wake's; a merged wake is counted apart; a consumption recorded before the wake's own
    /// call step cannot be its consumption.
    #[test]
    fn a_parked_wake_is_attributed_by_group() {
        let ro = roles();
        let r = seqd(vec![
            (Kind::User, 0, [9301, 12, code("C_P1_CALL"), 1, 0]),
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            (Kind::IdleDispatch, 1, [9300, 1, 0, 0, 0]),
        ]);
        assert_eq!(parked_wake(&r, 0, ro.c, 1, ro.s), Ok((5, ParkedRoute::Ipi)));
        assert!(check_ipi_population(&r).is_ok());
        let m = seqd(vec![
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            (Kind::User, 0, [9301, 12, code("C_P1_CALL"), 1, 0]),
            pubrec(0, 1, 1, 1),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
            (Kind::IdleDispatch, 1, [9300, 1, 0, 0, 0]),
        ]);
        assert_eq!(
            parked_wake(&m, 2, ro.c, 1, ro.s),
            Ok((2, ParkedRoute::Merged))
        );
        assert!(check_ipi_population(&m).is_ok());
        let early = seqd(vec![
            arr(1, 0b1, IDLE),
            cons(1, 0, 1),
            (Kind::User, 0, [9301, 12, code("C_P1_CALL"), 1, 0]),
            pubrec(0, 1, 0, 1),
            reqrec(0, 1, 0),
            (Kind::IdleDispatch, 1, [9300, 1, 0, 0, 0]),
        ]);
        assert_eq!(
            parked_wake(&early, 2, ro.c, 1, ro.s),
            Err("ipi_attribution_inconsistent")
        );
    }

    #[test]
    fn a_step_message_maps_to_its_code() {
        assert_eq!(step_code("SMP3 S_PRE"), Some(9));
        assert_eq!(
            step_code("SMP3 C_FAIL_STALE_AFTER_INVALIDATION"),
            Some(STEP_FAIL + 1)
        );
        assert_eq!(step_code("SMP3 WHAT"), Some(STEP_FAIL));
        assert_eq!(step_code("SMP2 S_PRE"), None);
        assert_eq!(step_name(9), "S_PRE");
    }

    #[test]
    fn the_recorder_seals_what_it_published() {
        // The recorder is process-global; this is its only test, and it arms it.
        arm();
        let g = next_generation();
        push(Kind::User, 1, [1, 2, 3, 4, 5]);
        push(Kind::IdleDispatch, 0, [9, 1, 0, 0, 0]);
        let mut out = [None; SLOTS];
        let (n, overflow) = seal(&mut out, 10).expect("sealed");
        assert!(!overflow && n >= 2 && g >= 1);
        assert_eq!(out[n - 1].map(|r| r.kind), Some(Kind::IdleDispatch));
    }
}
