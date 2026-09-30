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
    /// publication and BEFORE the firmware call.
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
}

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
pub const MUT_ROUNDS: u64 = 4;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpiRefusal {
    /// A consumed source that no pending publication from that source to that CPU explains.
    Unexplained { arrival: u32 },
    /// A publication recorded as fresh while one from the same source was still pending, or as
    /// merged when none was: the mailbox accounting is inconsistent.
    MergeInconsistent { publication: u32 },
    /// A publication no later arrival consumed.
    Undelivered { publication: u32 },
    /// A CPU consumed more supervisor software interrupts than the firmware was successfully asked
    /// to raise on it.
    ArrivalsExceedRequests { cpu: u8 },
    /// A firmware request failed.
    FirmwareRefused { request: u32 },
}

/// Pair every publication with the arrival that consumed it, and bound arrivals by requests. Pure.
/// Returns `(arrivals, consumed_publications, empty_arrivals, merged_publications)`.
pub fn check_ipi_population(recs: &[Rec]) -> Result<(usize, usize, usize, usize), IpiRefusal> {
    // pending[src][dst] = index of the fresh publication not yet consumed.
    let mut pending = [[None::<usize>; 64]; 64];
    let mut arrivals = 0usize;
    let mut consumed = 0usize;
    let mut empty = 0usize;
    let mut merged = 0usize;
    let mut requests = [0usize; 64];
    let mut taken = [0usize; 64];
    for (i, r) in recs.iter().enumerate() {
        match r.kind {
            Kind::IpiPublished if r.f[2] == PUB_WAKE || r.f[2] == PUB_KICK => {
                let (src, dst) = (r.cpu as usize & 63, r.f[0] as usize & 63);
                let was = pending[src][dst];
                match (r.f[3], was) {
                    (0, None) => pending[src][dst] = Some(i),
                    (1, Some(_)) => merged += 1,
                    _ => return Err(IpiRefusal::MergeInconsistent { publication: r.seq }),
                }
            }
            Kind::IpiRequested => {
                if r.f[3] != 0 {
                    return Err(IpiRefusal::FirmwareRefused { request: r.seq });
                }
                requests[r.f[0] as usize & 63] += 1;
            }
            Kind::IpiArrived | Kind::ParkReleased => {
                let dst = r.cpu as usize & 63;
                taken[dst] += 1;
                if r.kind == Kind::ParkReleased {
                    continue;
                }
                arrivals += 1;
                if r.f[0] == 0 {
                    empty += 1;
                }
                for src in 0..64 {
                    if r.f[0] & (1u64 << src) == 0 {
                        continue;
                    }
                    if pending[src][dst].take().is_none() {
                        return Err(IpiRefusal::Unexplained { arrival: r.seq });
                    }
                    consumed += 1;
                }
            }
            _ => {}
        }
    }
    for row in &pending {
        if let Some(i) = row.iter().flatten().next() {
            return Err(IpiRefusal::Undelivered {
                publication: recs[*i].seq,
            });
        }
    }
    for cpu in 0..64 {
        if taken[cpu] > requests[cpu] {
            return Err(IpiRefusal::ArrivalsExceedRequests { cpu: cpu as u8 });
        }
    }
    Ok((arrivals, consumed, empty, merged))
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

/// A wake from `from` to `to_cpu`, published after `after`, and the arrival that consumed it.
fn wake_chain(
    recs: &[Rec],
    after: usize,
    from: Role,
    to_cpu: u8,
    origin: u64,
    window: u64,
) -> Result<usize, &'static str> {
    let published = find_from(recs, after, |r| {
        r.kind == Kind::IpiPublished
            && r.cpu == from.cpu
            && r.f[0] == u64::from(to_cpu)
            && r.f[2] == PUB_WAKE
    })
    .ok_or("ipi_not_published")?;
    let bit = 1u64 << (from.cpu & 63);
    let arrived = find_from(recs, published + 1, |r| {
        r.kind == Kind::IpiArrived && r.cpu == to_cpu && r.f[0] & bit != 0
    })
    .ok_or("ipi_not_consumed")?;
    let a = recs[arrived];
    if origin != u64::MAX && arr_origin(&a) != origin {
        return Err(if origin == ORIGIN_IDLE {
            "ipi_target_not_parked"
        } else {
            "ipi_target_not_in_user"
        });
    }
    if window != WINDOW_NONE && arr_window(&a) != window {
        return Err("ipi_sepc_outside_window");
    }
    Ok(arrived)
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
}

/// A wake of the blocked `woken` on `to_cpu` by `from`, after step `after`, classified. Pure.
pub fn parked_wake(
    recs: &[Rec],
    after: usize,
    from: Role,
    to_cpu: u8,
    woken: Role,
) -> Result<(usize, ParkedRoute), &'static str> {
    let arrived = wake_chain(recs, after, from, to_cpu, u64::MAX, WINDOW_NONE)?;
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
    let published = find_from(recs, rd, |r| {
        r.kind == Kind::IpiPublished
            && r.cpu == waker.cpu
            && r.f[0] == u64::from(target.cpu)
            && r.f[2] == PUB_WAKE
    })
    .filter(|&p| p < sent_i)
    .ok_or("ipi_not_published")?;
    let arrived = wake_chain(recs, published, waker, target.cpu, u64::MAX, WINDOW_NONE)?;
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
    /// The shootdown's own target computation.
    pub shoot: usize,
    /// The completed firmware fence, when the target was resident at the computation.
    pub fence_done: Option<usize>,
    /// Whether the computation saw `target` current on its hart (and so had to fence it).
    pub resident: bool,
}

/// One production replacement of `target`'s W by `req`, verified end to end, starting its search
/// at `from`: the operation's begin .. completion (`Ok`, W), and inside it, in this order, the
/// displacement, the shootdown's target computation, the fence request naming the target's HART
/// and its successful completion — required exactly when that computation saw the target current,
/// which it must have recorded consistently — the acknowledged shootdown and the settlement. An
/// off-CPU target needs no fence here; the caller requires its activation evidence. Pure.
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
    let s = recs[shoot];
    let mask = s.f[2];
    if mask & (1u64 << (req.cpu & 63)) != 0 {
        return Err("shoot_targets_named_requester");
    }
    let current = match target.cpu {
        0 => s.f[3],
        1 => s.f[4],
        _ => return Err("shoot_targets_cpu_unrecorded"),
    };
    let resident = current == target.tid;
    if ((mask >> (target.cpu & 63)) & 1 == 1) != resident {
        return Err("shoot_targets_inconsistent");
    }
    let fence_done = if resident {
        let hart_bit = 1u64.checked_shl(target.hart as u32).unwrap_or(0);
        let cpu_bit = 1u64 << (target.cpu & 63);
        let fence = inside(Kind::FenceRequest, shoot, &|x| {
            x.f[2] & hart_bit != 0 && x.f[4] & cpu_bit != 0
        })
        .ok_or("fence_not_requested_for_the_resident_target")?;
        if recs[fence].f[2] & 1u64.checked_shl(req.hart as u32).unwrap_or(0) != 0 {
            return Err("requester_fenced_itself");
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
    if !rep.resident && reactivated(recs, t, rep.displaced, observed).is_none() {
        return Err("off_cpu_target_not_reactivated");
    }
    let reason = if !ready_met(&recs[rd]) || recs[rd].f[1] != t.tid {
        "not_ready"
    } else if !rep.resident {
        "off_cpu"
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
    if !s_op.resident && reactivated(recs, c, s_op.displaced, co).is_none() {
        return Err("off_cpu_target_not_reactivated");
    }
    if !c_op.resident && reactivated(recs, s, c_op.displaced, so).is_none() {
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
        Ok((a, c, e, m)) => {
            v.ipi_arrivals = a;
            v.ipi_consumed = c;
            v.ipi_empty = e;
            v.ipi_merged = m;
        }
        Err(IpiRefusal::Unexplained { arrival }) => v.fail("ipi_unexplained_arrival", arrival),
        Err(IpiRefusal::MergeInconsistent { publication }) => {
            v.fail("ipi_merge_inconsistent", publication)
        }
        Err(IpiRefusal::Undelivered { publication }) => v.fail("ipi_undelivered", publication),
        Err(IpiRefusal::ArrivalsExceedRequests { cpu }) => {
            v.fail("ipi_arrivals_exceed_requests", u32::from(cpu))
        }
        Err(IpiRefusal::FirmwareRefused { request }) => v.fail("ipi_firmware_refused", request),
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
                    v.mutual_off_cpu += usize::from(!op.resident);
                    att.push(Attempt {
                        phase: 4,
                        n: m as u8,
                        target_cpu: target.cpu,
                        generation: recs[op.begin].f[3],
                        eligible: true,
                        reason: if op.resident { "resident" } else { "off_cpu" },
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

    // Reclaim only after the shootdown the computation owed completed, exactly once, per displaced
    // page: the target computation, then — when it named any remote CPU — a SUCCESSFUL firmware
    // fence covering every named CPU's hart, then the acknowledgement, then the one settlement.
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
        let mut need = 0u64;
        for cpu in 0..64u8 {
            if mask & (1u64 << cpu) == 0 {
                continue;
            }
            let hart = if cpu == s.cpu {
                s.hart
            } else if cpu == c.cpu {
                c.hart
            } else {
                u64::MAX
            };
            need |= 1u64.checked_shl(hart as u32).unwrap_or(u64::MAX);
        }
        let fenced = (shoot + 1..ack).any(|j| {
            same(&recs[j], Kind::FenceDone) && recs[j].f[4] == 0 && recs[j].f[2] & need == need
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

    fn seqd(v: Vec<(Kind, u8, [u64; 5])>) -> Vec<Rec> {
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

    fn pubrec(from: u8, to: u64, merged: u64) -> (Kind, u8, [u64; 5]) {
        (Kind::IpiPublished, from, [to, 0, PUB_WAKE, merged, 0])
    }
    fn reqrec(from: u8, to: u64, err: u64) -> (Kind, u8, [u64; 5]) {
        (Kind::IpiRequested, from, [to, 0, PUB_WAKE, err, 0])
    }
    fn arr(cpu: u8, sources: u64, origin: u64) -> (Kind, u8, [u64; 5]) {
        (Kind::IpiArrived, cpu, [sources, origin, 0, 0, 0])
    }

    /// One P2 attempt, direction A (C wakes H1 on CPU 1 while S spins in window A), as the owners
    /// record it: C's call step, C's readiness (S current, CPU 1 at `entries` 40), the publication
    /// and request, the arrival, C's post-send step, S's window check.
    fn p2(ready_cur: u64, arrival: (u64, u64, u64, u64)) -> Vec<(Kind, u8, [u64; 5])> {
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
            pubrec(0, 1, 0),
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
            (Kind::User, 0, [9301, 12, code("C_P2A_SENT"), 1, 0]),
            (Kind::User, 1, [9300, 11, code("S_WIN_A_OK"), 1, 0]),
        ]
    }
    const S_INC: u64 = 9300 | (11 << 32);

    fn p2a(v: Vec<(Kind, u8, [u64; 5])>) -> Result<P2Attempt, &'static str> {
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
        // No wake: drop the publication, the request and the arrival.
        v.retain(|(kind, _, _)| {
            !matches!(
                kind,
                Kind::IpiPublished | Kind::IpiRequested | Kind::IpiArrived
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
        late.insert(5, publ);
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
        let mut w = p3(9300, repl(0, 9301, 11, 0b01, 0b10, 1), 10, 11);
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
        v.remove(4);
        assert_eq!(p2a(v), Err("ipi_not_consumed"));
        // No window check after the arrival (a corrupted context reports a FAIL step instead).
        let mut v = p2(9300, (USER, WINDOW_S_A, 41, S_INC));
        v.pop();
        assert_eq!(p2a(v), Err("p2_window_check_missing"));
        // The waker's post-send step missing.
        let mut v = p2(9300, (USER, WINDOW_S_A, 41, S_INC));
        v.remove(5);
        assert_eq!(p2a(v), Err("p2_sent_missing"));
    }

    #[test]
    fn a_publication_is_consumed_exactly_once() {
        let r = seqd(vec![pubrec(0, 1, 0), reqrec(0, 1, 0), arr(1, 0b1, IDLE)]);
        assert_eq!(check_ipi_population(&r), Ok((1, 1, 0, 0)));
    }

    #[test]
    fn requests_merging_into_one_arrival_are_not_loss() {
        // Two publications (the second merged), two requests, one arrival consuming both.
        let r = seqd(vec![
            pubrec(0, 1, 0),
            reqrec(0, 1, 0),
            pubrec(0, 1, 1),
            reqrec(0, 1, 0),
            arr(1, 0b1, USER),
        ]);
        assert_eq!(check_ipi_population(&r), Ok((1, 1, 0, 1)));
        // ... and an empty arrival later (the second SSIP after the consumption) is not duplication.
        let mut v = r.clone();
        v.push(Rec {
            seq: 5,
            kind: Kind::IpiArrived,
            cpu: 1,
            f: [0, USER, 0, 0, 0],
        });
        assert_eq!(check_ipi_population(&v), Ok((2, 1, 1, 1)));
    }

    #[test]
    fn an_unexplained_arrival_fails() {
        let r = seqd(vec![reqrec(0, 1, 0), arr(1, 0b1, IDLE)]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::Unexplained { arrival: 1 })
        );
        // A consumption of the WRONG source is unexplained too.
        let r = seqd(vec![pubrec(0, 1, 0), reqrec(0, 1, 0), arr(1, 0b10, IDLE)]);
        assert!(matches!(
            check_ipi_population(&r),
            Err(IpiRefusal::Unexplained { .. })
        ));
    }

    #[test]
    fn a_suppressed_notification_is_undelivered() {
        // Published and never consumed (the firmware call was suppressed).
        let r = seqd(vec![pubrec(0, 1, 0)]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::Undelivered { publication: 0 })
        );
    }

    #[test]
    fn more_arrivals_than_requests_fail() {
        // An SSIP never cleared re-traps: the second arrival has no request behind it.
        let r = seqd(vec![
            pubrec(0, 1, 0),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            arr(1, 0, IDLE),
        ]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::ArrivalsExceedRequests { cpu: 1 })
        );
    }

    #[test]
    fn inconsistent_merge_accounting_fails() {
        let r = seqd(vec![pubrec(0, 1, 1)]);
        assert!(matches!(
            check_ipi_population(&r),
            Err(IpiRefusal::MergeInconsistent { .. })
        ));
        let r = seqd(vec![pubrec(0, 1, 0), pubrec(0, 1, 0)]);
        assert!(matches!(
            check_ipi_population(&r),
            Err(IpiRefusal::MergeInconsistent { .. })
        ));
    }

    #[test]
    fn a_failed_firmware_request_fails() {
        let r = seqd(vec![pubrec(0, 1, 0), reqrec(0, 1, (-2i64) as u64)]);
        assert_eq!(
            check_ipi_population(&r),
            Err(IpiRefusal::FirmwareRefused { request: 1 })
        );
    }

    fn fence(cpu: u8, generation: u64, err: u64) -> Vec<(Kind, u8, [u64; 5])> {
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

    /// One whole replacement of S's W by C, as the owners record it, the shootdown's target
    /// computation seeing CPU 0 running `cur0` and CPU 1 running `cur1`.
    fn repl_seen(
        req_cpu: u8,
        req_tid: u64,
        asid: u64,
        hart_mask: u64,
        cpu_mask: u64,
        generation: u64,
        cur: (u64, u64),
    ) -> Vec<(Kind, u8, [u64; 5])> {
        vec![
            (
                Kind::VmOpBegin,
                req_cpu,
                [asid, W, 0x1000, generation, req_tid],
            ),
            (Kind::VmDisplaced, req_cpu, [asid, W, 0xAAA000, 0, 0]),
            (
                Kind::ShootTargets,
                req_cpu,
                [asid, W, cpu_mask, cur.0, cur.1],
            ),
            (
                Kind::FenceRequest,
                req_cpu,
                [asid, W, hart_mask, generation + 100, cpu_mask],
            ),
            (
                Kind::FenceDone,
                req_cpu,
                [asid, W, hart_mask, generation + 100, 0],
            ),
            (Kind::VmShootdown, req_cpu, [asid, W, 1, 0, 0]),
            (Kind::VmSettled, req_cpu, [asid, W, 0xAAA000, 0, 0]),
            (Kind::VmOpEnd, req_cpu, [asid, W, generation, 0, W]),
        ]
    }

    /// ... with S resident on CPU 1 and C (the requester) on CPU 0.
    fn repl(
        req_cpu: u8,
        req_tid: u64,
        asid: u64,
        hart_mask: u64,
        cpu_mask: u64,
        generation: u64,
    ) -> Vec<(Kind, u8, [u64; 5])> {
        repl_seen(
            req_cpu,
            req_tid,
            asid,
            hart_mask,
            cpu_mask,
            generation,
            (9301, 9300),
        )
    }

    /// ... with S OFF CPU 1 (another task current there): no remote target, no fence.
    fn repl_off_cpu(generation: u64) -> Vec<(Kind, u8, [u64; 5])> {
        let mut v = repl_seen(0, 9301, 11, 0, 0, generation, (9301, 2));
        v.remove(4);
        v.remove(3);
        v
    }

    #[test]
    fn a_replacement_needs_the_fence_to_the_resident_hart_completed_before_settlement() {
        let ro = roles();
        // C (cpu 0) replaces S's W; S is on cpu 1 = hart 0.
        let ok = seqd(repl(0, 9301, 11, 0b01, 0b10, 1));
        let r = replacement(&ok, &ro, 0, ro.c, ro.s).expect("verified");
        assert!(r.resident && r.fence_done.is_some());
        // The fence named the wrong hart (a CPU index taken for a hart id): refused.
        let wrong = seqd(repl(0, 9301, 11, 0b10, 0b10, 1));
        assert_eq!(
            replacement(&wrong, &ro, 0, ro.c, ro.s),
            Err("fence_not_requested_for_the_resident_target")
        );
        // The computation saw S resident but the fence was omitted altogether: refused — a
        // resident target missing its fence is a FAILURE, never an ineligible attempt.
        let mut v = repl(0, 9301, 11, 0b01, 0b10, 1);
        v.remove(4);
        v.remove(3);
        assert_eq!(
            replacement(&seqd(v), &ro, 0, ro.c, ro.s),
            Err("fence_not_requested_for_the_resident_target")
        );
        // The completion withheld (firmware error): refused, never settled as complete.
        let mut v = repl(0, 9301, 11, 0b01, 0b10, 1);
        v[4].2[4] = (-1i64) as u64;
        assert_eq!(
            replacement(&seqd(v), &ro, 0, ro.c, ro.s),
            Err("fence_failed")
        );
        // Settlement before the fence completed (premature release).
        let mut v = repl(0, 9301, 11, 0b01, 0b10, 1);
        let settled = v.remove(6);
        v.insert(3, settled);
        assert!(replacement(&seqd(v), &ro, 0, ro.c, ro.s).is_err());
    }

    #[test]
    fn the_shootdown_computation_decides_residency_and_must_be_recorded_consistently() {
        let ro = roles();
        // S off CPU 1 at the computation: no remote target, no fence, a verified operation.
        let r = replacement(&seqd(repl_off_cpu(1)), &ro, 0, ro.c, ro.s).expect("verified");
        assert!(!r.resident && r.fence_done.is_none());
        // No record of the computation at all: missing evidence fails.
        let mut v = repl(0, 9301, 11, 0b01, 0b10, 1);
        v.remove(2);
        assert_eq!(
            replacement(&seqd(v), &ro, 0, ro.c, ro.s),
            Err("shoot_targets_missing")
        );
        // A snapshot that says S was current but a mask that omits CPU 1 (or the reverse): the
        // record contradicts itself.
        assert_eq!(
            replacement(
                &seqd(repl_seen(0, 9301, 11, 0b01, 0, 1, (9301, 9300))),
                &ro,
                0,
                ro.c,
                ro.s
            ),
            Err("shoot_targets_inconsistent")
        );
        assert_eq!(
            replacement(
                &seqd(repl_seen(0, 9301, 11, 0b01, 0b10, 1, (9301, 2))),
                &ro,
                0,
                ro.c,
                ro.s
            ),
            Err("shoot_targets_inconsistent")
        );
        // A substituted identity: the computation names another tid as S.
        let mut v = repl_off_cpu(1);
        v[2].2[4] = 9300 + 7;
        assert!(
            replacement(&seqd(v), &ro, 0, ro.c, ro.s)
                .map(|r| !r.resident)
                .unwrap_or(true),
            "a substituted tid is never taken as the target"
        );
        // The requester named itself.
        assert_eq!(
            replacement(
                &seqd(repl_seen(0, 9301, 11, 0b11, 0b11, 1, (9301, 9300))),
                &ro,
                0,
                ro.c,
                ro.s
            ),
            Err("shoot_targets_named_requester")
        );
    }

    /// One whole serial attempt r = 1 (target S on CPU 1, requester C on CPU 0).
    fn p3(
        ready_cur: u64,
        op: Vec<(Kind, u8, [u64; 5])>,
        c0: u64,
        c1: u64,
    ) -> Vec<(Kind, u8, [u64; 5])> {
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

    fn p3a(v: Vec<(Kind, u8, [u64; 5])>) -> Result<P3Attempt, &'static str> {
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
        let a = p3a(p3(9300, repl(0, 9301, 11, 0b01, 0b10, 1), 10, 11)).expect("graded");
        assert!(a.eligible && a.reason == "none" && a.generation == 1);
        // Resident at the computation and fenced, but the hart entered the kernel in between.
        let a = p3a(p3(9300, repl(0, 9301, 11, 0b01, 0b10, 1), 10, 14)).expect("graded");
        assert!(!a.eligible && a.reason == "interfered");
        // Readiness never saw the target current.
        let a = p3a(p3(2, repl(0, 9301, 11, 0b01, 0b10, 1), 10, 14)).expect("graded");
        assert!(!a.eligible && a.reason == "not_ready");
        // The counts went nowhere: not history.
        assert_eq!(
            p3a(p3(9300, repl(0, 9301, 11, 0b01, 0b10, 1), 10, 10)),
            Err("tlb_residency_count_not_monotone")
        );
    }

    #[test]
    fn an_off_cpu_target_is_uncredited_only_with_positive_activation_evidence() {
        // Off CPU at the computation, and its hart re-activated S's address space (the satp write
        // whose sfence retires every stale translation) after the displacement, before OBSERVED.
        let mut op = repl_off_cpu(1);
        op.insert(
            4,
            (Kind::Activation, 1, [11, 0x8000_b000_0000_0001, 0, 0, 0]),
        );
        let a = p3a(p3(9300, op, 10, 15)).expect("graded");
        assert!(!a.eligible && a.reason == "off_cpu");
        // ... without that activation: a FAILURE, not an uncredited attempt.
        assert_eq!(
            p3a(p3(9300, repl_off_cpu(1), 10, 15)),
            Err("off_cpu_target_not_reactivated")
        );
        // ... an activation of ANOTHER address space is not the target's.
        let mut op = repl_off_cpu(1);
        op.insert(
            4,
            (Kind::Activation, 1, [2, 0x8000_2000_0000_0001, 0, 0, 0]),
        );
        assert_eq!(
            p3a(p3(9300, op, 10, 15)),
            Err("off_cpu_target_not_reactivated")
        );
        // ... nor is an activation BEFORE the displacement (the new PTE was not yet written).
        let mut op = repl_off_cpu(1);
        op.insert(
            1,
            (Kind::Activation, 1, [11, 0x8000_b000_0000_0001, 0, 0, 0]),
        );
        assert_eq!(
            p3a(p3(9300, op, 10, 15)),
            Err("off_cpu_target_not_reactivated")
        );
        // ... nor is one on the requester's hart.
        let mut op = repl_off_cpu(1);
        op.insert(
            4,
            (Kind::Activation, 0, [11, 0x8000_b000_0000_0001, 0, 0, 0]),
        );
        assert_eq!(
            p3a(p3(9300, op, 10, 15)),
            Err("off_cpu_target_not_reactivated")
        );
    }

    #[test]
    fn the_premature_release_check_is_global() {
        let ro = roles();
        let mut v = repl(0, 9301, 11, 0b01, 0b10, 1);
        let settled = v.remove(6);
        v.insert(3, settled);
        let verdict = verify(&seqd(v), &ro, false);
        assert!(verdict.failure.is_some());
        // The global check itself, read off its own counts (the verdict's reported failure is the
        // FIRST one, which for a record with no P1 phase is the missing call): a settlement with
        // no completed fence for a computation that named a remote CPU is not counted...
        let verdict = verify(&seqd(v_premature(0b10)), &ro, false);
        assert!(verdict.failure.is_some());
        assert_eq!(
            verdict.settled_after_completion + verdict.settled_local_only,
            0
        );
        // ... the same settlement after a successful fence covering that CPU's hart is...
        let mut ok = v_premature(0b10);
        ok.insert(2, (Kind::FenceDone, 0, [11, W, 0b01, 9, 0]));
        assert_eq!(verify(&seqd(ok), &ro, false).settled_after_completion, 1);
        // ... a FAILED fence never is...
        let mut bad = v_premature(0b10);
        bad.insert(2, (Kind::FenceDone, 0, [11, W, 0b01, 9, (-1i64) as u64]));
        assert_eq!(verify(&seqd(bad), &ro, false).settled_after_completion, 0);
        // ... and a computation that named no remote CPU needs none (the local fence and every
        // other hart's next activation are the whole shootdown).
        assert_eq!(
            verify(&seqd(v_premature(0)), &ro, false).settled_local_only,
            1
        );
        // A settlement with no recorded computation at all is never counted.
        let mut none = v_premature(0);
        none.remove(1);
        let verdict = verify(&seqd(none), &ro, false);
        assert_eq!(
            verdict.settled_after_completion + verdict.settled_local_only,
            0
        );
    }

    fn v_premature(mask: u64) -> Vec<(Kind, u8, [u64; 5])> {
        vec![
            (Kind::VmDisplaced, 0, [11, W, 0xAAA000, 0, 0]),
            (Kind::ShootTargets, 0, [11, W, mask, 9301, 9300]),
            (Kind::VmShootdown, 0, [11, W, 1, 0, 0]),
            (Kind::VmSettled, 0, [11, W, 0xAAA000, 0, 0]),
        ]
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
            pubrec(0, 1, 0),
            reqrec(0, 1, 0),
            arr(1, 0b1, IDLE),
            (Kind::IdleDispatch, 1, [9300, 1, 0, 0, 0]),
        ]);
        assert_eq!(parked_wake(&r, 0, ro.c, 1, ro.s), Ok((4, ParkedRoute::Ipi)));
        // The IPI's idle advance selected another runnable task first: preceded, not credited.
        let mut s = r.clone();
        s[4].f[0] = 9302;
        assert_eq!(
            parked_wake(&s, 0, ro.c, 1, ro.s),
            Ok((4, ParkedRoute::Preceded))
        );
        // ... but an idle advance that dispatched nothing is a substitution.
        let mut z = r.clone();
        z[4].f[0] = 0;
        assert_eq!(
            parked_wake(&z, 0, ro.c, 1, ro.s),
            Err("ipi_dispatch_substituted")
        );
        // The dispatch was the timer's, not the IPI's, after an idle arrival: substitution.
        let mut t = r.clone();
        t[4].f[1] = 0;
        assert_eq!(
            parked_wake(&t, 0, ro.c, 1, ro.s),
            Err("ipi_dispatch_substituted")
        );
        // Never consumed (clearing omitted and the publication lost, or notification suppressed).
        let u = seqd(vec![
            (Kind::User, 0, [9301, 12, code("C_P1_CALL"), 1, 0]),
            pubrec(0, 1, 0),
        ]);
        assert_eq!(parked_wake(&u, 0, ro.c, 1, ro.s), Err("ipi_not_consumed"));
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
