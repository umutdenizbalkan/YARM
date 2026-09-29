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
pub const SLOTS: usize = 1024;

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
    /// empty arrival), `f[1]` origin | window << 8, `f[2]` `sepc`, `f[3]` the task current when it
    /// was taken (0 = none), `f[4]` the interrupted `sstatus`.
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
pub const P2_ROUNDS: u64 = 4;
/// Serial remote-invalidation rounds (odd: S is the target; even: C is), and how many of each
/// direction must be CREDITED — the target hart took no supervisor entry inside its window.
pub const TLB_ROUNDS: u64 = 8;
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
    pub p2_user: usize,
    /// P2 attempts whose arrival found the target displaced (not credited).
    pub p2_displaced: usize,
    /// Serial rounds whose whole chain verified.
    pub tlb_rounds: usize,
    /// ... of which the target hart took no supervisor entry in its window, per target.
    pub tlb_credited_s: usize,
    pub tlb_credited_c: usize,
    /// ... of which the target took other entries in its window (not credited).
    pub tlb_interfered: usize,
    pub mutual_rounds: usize,
    pub mutual_overlapped: usize,
    /// Contended spin-lock acquisitions inside the mutual rounds' operation windows.
    pub mutual_contended: u64,
    pub settled_after_completion: usize,
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
    if origin != u64::MAX && a.f[1] & 0xff != origin {
        return Err(if origin == ORIGIN_IDLE {
            "ipi_target_not_parked"
        } else {
            "ipi_target_not_in_user"
        });
    }
    if window != WINDOW_NONE && (a.f[1] >> 8) != window {
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
    if a.f[1] & 0xff == ORIGIN_IDLE {
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
        Some(d) if a.f[3] == woken.tid => Ok((d, ParkedRoute::TimerFirst)),
        Some(_) => Err("ipi_arrived_in_another_task"),
        None => Ok((after, ParkedRoute::Busy)),
    }
}

/// Where a wake aimed at a RESIDENT user-mode target actually landed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResidentRoute {
    /// Taken from U-mode in the target, with `sepc` inside its register-checked window.
    InWindow,
    /// The target was not resident when the IPI arrived: another task was (CPU 0's tick may
    /// switch in the supervisor), or the hart was at its idle boundary with the displaced target
    /// queued. Counted apart; the target's own window check is still required afterwards.
    Displaced,
}

/// A wake by `from` of the hart `target` is expected to be resident on, after step `after`,
/// classified. An arrival IN the target from U-mode must be inside `window`. Pure.
pub fn resident_wake(
    recs: &[Rec],
    after: usize,
    from: Role,
    target: Role,
    window: u64,
) -> Result<(usize, ResidentRoute), &'static str> {
    let arrived = wake_chain(recs, after, from, target.cpu, u64::MAX, WINDOW_NONE)?;
    let a = recs[arrived];
    if a.f[1] & 0xff == ORIGIN_USER && a.f[3] == target.tid {
        if (a.f[1] >> 8) != window {
            return Err("ipi_sepc_outside_window");
        }
        return Ok((arrived, ResidentRoute::InWindow));
    }
    Ok((arrived, ResidentRoute::Displaced))
}

/// One production replacement of `target`'s W by `req`, verified end to end, starting its search
/// at `from`: the operation's own begin .. completion, and inside it the displacement, the fence
/// request naming the target's hart, that request's successful completion, the acknowledged
/// shootdown and the settlement — in that order. Returns `(begin, end, fence_done)`. Pure.
pub fn replacement(
    recs: &[Rec],
    roles: &Roles,
    from: usize,
    req: Role,
    target: Role,
) -> Result<(usize, usize, usize), &'static str> {
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
    let hart_bit = 1u64.checked_shl(target.hart as u32).unwrap_or(0);
    let cpu_bit = 1u64 << (target.cpu & 63);
    let fence = inside(Kind::FenceRequest, disp, &|x| {
        x.f[2] & hart_bit != 0 && x.f[4] & cpu_bit != 0
    })
    .ok_or("fence_not_requested_for_the_resident_target")?;
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
    let sd = inside(Kind::VmShootdown, done, &|_| true).ok_or("shootdown_outside_operation")?;
    if recs[sd].f[2] != 1 {
        return Err("shootdown_not_acknowledged");
    }
    let old = recs[disp].f[2];
    inside(Kind::VmSettled, sd, &|x| x.f[2] == old).ok_or("settlement_outside_operation")?;
    Ok((begin, end, done))
}

/// What one fully verified mutual round showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MutualRound {
    /// Both production operations entered before either completed.
    pub overlapped: bool,
    /// Contended spin-lock acquisitions from the first entry to the last exit.
    pub contended: u64,
}

/// Mutual round `m`: S's NR 3 on S's hart replaces C's W while C's NR 3 on C's hart replaces S's.
pub fn mutual_round(recs: &[Rec], roles: &Roles, m: u64) -> Result<MutualRound, &'static str> {
    let (s, c) = (roles.s, roles.c);
    let sn = user(recs, 0, s, "S_MUT_NR3", Some(m)).ok_or("mut_request_missing")?;
    let cn = user(recs, 0, c, "C_MUT_NR3", Some(m)).ok_or("mut_request_missing")?;
    let (sb, se, _) = replacement(recs, roles, sn, s, c).map_err(|_| "mut_s_operation_invalid")?;
    let (cb, ce, _) = replacement(recs, roles, cn, c, s).map_err(|_| "mut_c_operation_invalid")?;
    let so = user(recs, 0, s, "S_MUT_OK", Some(m)).ok_or("mut_observation_missing")?;
    let co = user(recs, 0, c, "C_MUT_OK", Some(m)).ok_or("mut_observation_missing")?;
    if so < ce || co < se {
        return Err("mut_observed_before_operation_completed");
    }
    let (first, last) = (sb.min(cb), se.max(ce));
    let count_at = |i: usize, phase: u64| {
        find_from(recs, i, |x| x.kind == Kind::Contention && x.f[1] == phase).map(|j| recs[j].f[0])
    };
    let contended = match (count_at(first, 0), count_at(last, 1)) {
        (Some(a), Some(b)) => b.saturating_sub(a),
        _ => 0,
    };
    Ok(MutualRound {
        overlapped: ops_overlap((sb, se), (cb, ce)),
        contended,
    })
}

/// Grade a sealed record. Pure; the kernel runs it at the dump and the grader re-derives it.
pub fn verify(recs: &[Rec], roles: &Roles, overflowed: bool) -> Verdict {
    let mut v = Verdict {
        records: recs.len(),
        ..Verdict::default()
    };
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
        if r.kind == Kind::IpiArrived && r.f[1] & 0xff == ORIGIN_USER {
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

    // P2 — user-mode targets inside the checked windows, P2_ROUNDS attempts per direction. Every
    // attempt's window check must pass; an arrival that found another task resident is counted
    // apart; each direction needs at least one arrival in the resident target's window.
    let mut resident = [0usize; 2];
    for k in 1..=P2_ROUNDS {
        for (dir, (waker, call_name, target, ok_name, window)) in [
            (c, "C_P2A_CALL", s, "S_WIN_A_OK", WINDOW_S_A),
            (s, "S_P2B_CALL", c, "C_WIN_B_OK", WINDOW_C_B),
        ]
        .into_iter()
        .enumerate()
        {
            let attempt = (|| -> Result<(usize, ResidentRoute), &'static str> {
                let call = user(recs, at, waker, call_name, Some(k)).ok_or("p2_call_missing")?;
                let (arr, route) = resident_wake(recs, call, waker, target, window)?;
                let ok =
                    user(recs, arr, target, ok_name, Some(k)).ok_or("p2_window_check_missing")?;
                if recs[ok].cpu != target.cpu {
                    return Err("p2_window_checked_elsewhere");
                }
                Ok((ok, route))
            })();
            match attempt {
                Ok((end, route)) => {
                    match route {
                        ResidentRoute::InWindow => {
                            v.p2_user += 1;
                            resident[dir] += 1;
                        }
                        ResidentRoute::Displaced => v.p2_displaced += 1,
                    }
                    if dir == 1 {
                        at = end;
                    }
                }
                Err(why) => v.fail(why, k as u32),
            }
        }
    }
    if resident[0] == 0 || resident[1] == 0 {
        v.fail("p2_no_arrival_in_a_resident_window", 0);
    }

    // P3 — serial remote-invalidation rounds.
    for r in 1..=TLB_ROUNDS {
        let (t, q, tp, tq) = if r % 2 == 1 {
            (s, c, "S", "C")
        } else {
            (c, s, "C", "S")
        };
        let pre_name = if tp == "S" { "S_PRE" } else { "C_PRE" };
        let obs_name = if tp == "S" {
            "S_OBSERVED"
        } else {
            "C_OBSERVED"
        };
        let req_name = if tq == "S" { "S_REQ" } else { "C_REQ" };
        let round = (|| -> Result<bool, &'static str> {
            let pre = user(recs, 0, t, pre_name, Some(r)).ok_or("tlb_pre_missing")?;
            let req = user(recs, pre, q, req_name, Some(r)).ok_or("tlb_request_step_missing")?;
            let (_, end, done) = replacement(recs, roles, req, q, t)?;
            let observed =
                user(recs, pre, t, obs_name, Some(r)).ok_or("tlb_observation_missing")?;
            if observed < end || observed < done {
                return Err("tlb_observed_before_completion");
            }
            let p = recs[pre];
            if recs[observed].cpu != p.cpu || p.cpu != t.cpu || recs[req].cpu != q.cpu {
                return Err("tlb_target_not_resident_remote");
            }
            // Residency: the target hart's supervisor entries between its PRE and OBSERVED steps.
            let (_, c0) = residency(recs, t, pre_name, r).ok_or("tlb_residency_missing")?;
            let (_, c1) = residency(recs, t, obs_name, r).ok_or("tlb_residency_missing")?;
            if c1 <= c0 {
                return Err("tlb_residency_count_not_monotone");
            }
            Ok(c1 == c0 + 1)
        })();
        match round {
            Ok(credited) => {
                v.tlb_rounds += 1;
                if !credited {
                    v.tlb_interfered += 1;
                } else if t.tid == s.tid {
                    v.tlb_credited_s += 1;
                } else {
                    v.tlb_credited_c += 1;
                }
            }
            Err(why) => v.fail(why, r as u32),
        }
    }
    if v.tlb_credited_s < TLB_MIN_CREDITED_PER_DIRECTION
        || v.tlb_credited_c < TLB_MIN_CREDITED_PER_DIRECTION
    {
        v.fail("tlb_too_few_credited_rounds_per_direction", 0);
    }

    // P4 — mutual rounds, graded on the production operations themselves.
    for m in 1..=MUT_ROUNDS {
        match mutual_round(recs, roles, m) {
            Ok(round) => {
                v.mutual_rounds += 1;
                v.mutual_overlapped += usize::from(round.overlapped);
                v.mutual_contended += round.contended;
                if !round.overlapped {
                    v.fail("mut_operations_serialized", m as u32);
                }
            }
            Err(why) => v.fail(why, m as u32),
        }
    }

    // Reclaim only after the firmware completed the fence, exactly once, per displaced page.
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
        let completed = recs[i..settled[0]].iter().any(|x| {
            x.kind == Kind::FenceDone
                && x.cpu == d.cpu
                && x.f[0] == d.f[0]
                && x.f[1] == d.f[1]
                && x.f[4] == 0
        });
        if completed {
            v.settled_after_completion += 1;
        } else {
            v.fail("settled_before_fence_completion", d.seq);
        }
    }
    if v.tlb_rounds + v.mutual_rounds * 2 > v.settled_after_completion {
        v.fail("displaced_pages_unsettled", 0);
    }
    if user(recs, 0, s, "S_DONE", None).is_none() || user(recs, 0, c, "C_DONE", None).is_none() {
        v.fail("witness_not_complete", 0);
    }
    v
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

    #[test]
    fn a_resident_wake_is_credited_only_inside_the_resident_targets_window() {
        let ro = roles();
        let win = |tid: u64, origin: u64, window: u64| {
            (
                Kind::IpiArrived,
                1u8,
                [0b1, origin | (window << 8), 0x400000, tid, 0],
            )
        };
        let base = |a| seqd(vec![pubrec(0, 1, 0), reqrec(0, 1, 0), a]);
        assert_eq!(
            resident_wake(
                &base(win(9300, USER, WINDOW_S_A)),
                0,
                ro.c,
                ro.s,
                WINDOW_S_A
            ),
            Ok((2, ResidentRoute::InWindow))
        );
        // In the target but outside its window: a failure, never a credit.
        assert_eq!(
            resident_wake(
                &base(win(9300, USER, WINDOW_NONE)),
                0,
                ro.c,
                ro.s,
                WINDOW_S_A
            ),
            Err("ipi_sepc_outside_window")
        );
        // Another task was resident, or the hart was idle: displaced, not credited.
        assert_eq!(
            resident_wake(&base(win(2, USER, WINDOW_NONE)), 0, ro.c, ro.s, WINDOW_S_A),
            Ok((2, ResidentRoute::Displaced))
        );
        assert_eq!(
            resident_wake(&base(win(0, IDLE, WINDOW_NONE)), 0, ro.c, ro.s, WINDOW_S_A),
            Ok((2, ResidentRoute::Displaced))
        );
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

    /// One whole replacement of S's W by C, as the owners record it.
    fn repl(
        req_cpu: u8,
        req_tid: u64,
        asid: u64,
        hart_mask: u64,
        cpu_mask: u64,
        generation: u64,
    ) -> Vec<(Kind, u8, [u64; 5])> {
        vec![
            (
                Kind::VmOpBegin,
                req_cpu,
                [asid, W, 0x1000, generation, req_tid],
            ),
            (Kind::VmDisplaced, req_cpu, [asid, W, 0xAAA000, 0, 0]),
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

    #[test]
    fn a_replacement_needs_the_fence_to_the_resident_hart_completed_before_settlement() {
        let ro = roles();
        // C (cpu 0) replaces S's W; S is on cpu 1 = hart 0.
        let ok = seqd(repl(0, 9301, 11, 0b01, 0b10, 1));
        assert!(replacement(&ok, &ro, 0, ro.c, ro.s).is_ok());
        // The fence named the wrong hart (a CPU index taken for a hart id): refused.
        let wrong = seqd(repl(0, 9301, 11, 0b10, 0b10, 1));
        assert_eq!(
            replacement(&wrong, &ro, 0, ro.c, ro.s),
            Err("fence_not_requested_for_the_resident_target")
        );
        // The fence omitted altogether.
        let mut v = repl(0, 9301, 11, 0b01, 0b10, 1);
        v.remove(3);
        v.remove(2);
        assert_eq!(
            replacement(&seqd(v), &ro, 0, ro.c, ro.s),
            Err("fence_not_requested_for_the_resident_target")
        );
        // The completion withheld (firmware error): refused, never settled as complete.
        let mut v = repl(0, 9301, 11, 0b01, 0b10, 1);
        v[3].2[4] = (-1i64) as u64;
        assert_eq!(
            replacement(&seqd(v), &ro, 0, ro.c, ro.s),
            Err("fence_failed")
        );
        // Settlement before the fence completed (premature release).
        let mut v = repl(0, 9301, 11, 0b01, 0b10, 1);
        let settled = v.remove(5);
        v.insert(2, settled);
        assert!(replacement(&seqd(v), &ro, 0, ro.c, ro.s).is_err());
    }

    #[test]
    fn the_premature_release_check_is_global() {
        let ro = roles();
        let mut v = repl(0, 9301, 11, 0b01, 0b10, 1);
        let settled = v.remove(5);
        v.insert(2, settled);
        let verdict = verify(&seqd(v), &ro, false);
        assert!(verdict.failure.is_some());
        // The global check itself, read off its own count (the verdict's reported failure is the
        // FIRST one, which for a record with no P1 phase is the missing call): a settlement with no
        // completed fence before it is not counted, the same settlement after one is.
        let r = seqd(v_premature());
        let verdict = verify(&r, &ro, false);
        assert!(verdict.failure.is_some());
        assert_eq!(verdict.settled_after_completion, 0);
        let mut ok = v_premature();
        ok.insert(1, (Kind::FenceDone, 0, [11, W, 0, 0, 0]));
        assert_eq!(verify(&seqd(ok), &ro, false).settled_after_completion, 1);
    }

    fn v_premature() -> Vec<(Kind, u8, [u64; 5])> {
        vec![
            (Kind::VmDisplaced, 0, [11, W, 0xAAA000, 0, 0]),
            (Kind::VmSettled, 0, [11, W, 0xAAA000, 0, 0]),
        ]
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
