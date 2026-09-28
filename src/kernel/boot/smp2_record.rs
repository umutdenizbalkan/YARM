// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP2 — the two-CPU witness's bounded transaction record, and the pure verifier that
//! grades it.
//!
//! # The record
//!
//! Each owner appends ONE record at its own commit point: the SGI owner when it has decided to
//! write `GICD_SGIR` (before the write), the vector entry when its claim returned a reschedule
//! SGI, the vector tail after the one `GICC_EOIR` write, the bridge's idle advance after the
//! resume succeeded, the page-table owner around its break-before-make invalidation, the VM
//! transaction at displacement / shootdown / settlement, and the DebugLog route for each user
//! step. A slot is claimed by one `fetch_add` on the sequence counter and published by a
//! `Release` store of its state after every field is written, so the claim order is a total order
//! consistent with every happens-before edge between owners, and a reader that sees a slot
//! complete sees all of its fields. Nothing here decides anything; every owner does its work
//! whether or not a record is taken.
//!
//! # What is graded, and what is not
//!
//! Only CAUSAL edges. A receiver may observe an event before its sender gets round to recording
//! the fact that it caused it — the SGI arrival can precede a record taken after the `GICD_SGIR`
//! write — so every "before" the verifier requires is one the sender records BEFORE the action
//! that the receiver can observe. A missing, duplicated or substituted step fails; a step that
//! merely happened later than it could have does not.
//!
//! The dump is sealed: it is taken only once the witness reports completion from both CPUs, and
//! only after every claimed slot has been published, so no record can be lost or half-read.

/// Record capacity. Bounded: a witness that overflows it fails rather than truncating quietly.
pub const SLOTS: usize = 768;

/// What a record says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// `cpu` decided to raise the reschedule SGI at `f[0]` (`f[1]` = `GICD_SGIR`, `f[2]` = 1
    /// for the AP's own start-up kick). Recorded BEFORE the controller write.
    SgiSent = 1,
    /// `cpu`'s claim returned the SGI: `f[0]` = full `GICC_IAR` token, `f[1]` = source CPU,
    /// `f[2]` = origin (0 EL0, 1 idle boundary, 2 other EL1) | window << 8, `f[3]` = `ELR_EL1`,
    /// `f[4]` = the task current on `cpu` when it was taken (0 = none).
    SgiArrived = 2,
    /// `cpu` wrote `GICC_EOIR` with token `f[0]`.
    SgiCompleted = 3,
    /// `cpu`'s idle advance resumed task `f[0]`; `f[1]` = 1 when a reschedule SGI drove it, 0
    /// when the periodic timer's idle advance did.
    IdleDispatch = 4,
    /// `cpu` began replacing a present leaf: `f[0]` asid, `f[1]` va, `f[2]` old PA, `f[3]` new
    /// PA, `f[4]` request generation. Recorded with the page-table lock held, before the break.
    InvalBegin = 5,
    /// The broadcast invalidation for that exact request has completed (`dsb ish` returned),
    /// same fields. This is the acknowledgement: nothing else acknowledges an AArch64 TLBI.
    InvalDone = 6,
    /// The VM transaction displaced `f[2]` at (`f[0]`, `f[1]`), still pinned.
    VmDisplaced = 7,
    /// Its shootdown owner answered `f[2]` (1 = acknowledged).
    VmShootdown = 8,
    /// The displaced backing `f[2]` was released for reclaim.
    VmSettled = 9,
    /// A user step: `f[0]` tid, `f[1]` asid, `f[2]` step code, `f[3]` round, `f[4]` aux.
    User = 10,
    /// The residency probe of asid `f[0]` was re-pointed at frame `f[1]` (PA `f[2]`) for round
    /// `f[3]`, WITHOUT any invalidation.
    RRepoint = 11,
}

impl Kind {
    pub fn from_u8(v: u8) -> Option<Kind> {
        Some(match v {
            1 => Kind::SgiSent,
            2 => Kind::SgiArrived,
            3 => Kind::SgiCompleted,
            4 => Kind::IdleDispatch,
            5 => Kind::InvalBegin,
            6 => Kind::InvalDone,
            7 => Kind::VmDisplaced,
            8 => Kind::VmShootdown,
            9 => Kind::VmSettled,
            10 => Kind::User,
            11 => Kind::RRepoint,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::SgiSent => "sgi_sent",
            Kind::SgiArrived => "sgi_arrived",
            Kind::SgiCompleted => "sgi_completed",
            Kind::IdleDispatch => "idle_dispatch",
            Kind::InvalBegin => "inval_begin",
            Kind::InvalDone => "inval_done",
            Kind::VmDisplaced => "vm_displaced",
            Kind::VmShootdown => "vm_shootdown",
            Kind::VmSettled => "vm_settled",
            Kind::User => "user",
            Kind::RRepoint => "r_repoint",
        }
    }
}

/// Arrival origins (low byte of `SgiArrived.f[2]`).
pub const ORIGIN_USER: u64 = 0;
pub const ORIGIN_IDLE: u64 = 1;
pub const ORIGIN_KERNEL: u64 = 2;

/// Windows (second byte of `SgiArrived.f[2]`): where in the witness programs `ELR_EL1` was.
pub const WINDOW_NONE: u64 = 0;
pub const WINDOW_S_A: u64 = 1;
pub const WINDOW_C_B: u64 = 2;
pub const WINDOW_S_TLB: u64 = 3;
pub const WINDOW_C_TLB: u64 = 4;

/// One record as the verifier sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rec {
    pub seq: u32,
    pub kind: Kind,
    pub cpu: u8,
    pub f: [u64; 5],
}

/// The user steps, by the exact message after `SMP2 `. Failure steps are >= `STEP_FAIL`.
pub const STEP_FAIL: u64 = 100;
pub const STEPS: &[(&str, u64)] = &[
    ("S_ENTERED", 1),
    ("S_P1_RESUMED", 2),
    ("S_P1_REPLY", 3),
    ("S_WIN_A_OK", 4),
    ("S_P2B_CALL", 5),
    ("S_P2B_REPLY_OK", 6),
    ("S_ARM_R", 7),
    ("S_REQ_DONE", 8),
    ("S_PRIMED", 9),
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
    ("C_ARM_R", 27),
    ("C_REQ_DONE", 28),
    ("C_PRIMED", 29),
    ("C_OBSERVED", 30),
    ("C_MUT_NR3", 31),
    ("C_MUT_OK", 32),
    ("C_DONE", 33),
    ("H_ENTERED", 41),
    ("H_SERVED", 42),
];

/// The step code for a witness message (`SMP2 <step>`), `STEP_FAIL + n` for a failure step and
/// for anything unrecognised — an unknown step is never silently accepted.
pub fn step_code(msg: &str) -> Option<u64> {
    let step = msg.strip_prefix("SMP2 ")?.trim_end();
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

/// Parked-target rounds per direction, and the SGI-driven dispatches CPU 0 must show of them.
pub const P1_ROUNDS: u64 = 4;
pub const P1_MIN_SGI_TO_C: usize = 2;

/// The four witness tasks: `(tid, asid, home cpu)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Role {
    pub tid: u64,
    pub asid: u64,
    pub cpu: u8,
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

/// Why an invalidation acknowledgement does not acknowledge a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalRefusal {
    /// No completion carries this request's identity and generation after its begin.
    Missing,
    /// A completion for the same mapping names a different generation — an older (or another
    /// CPU's) request's acknowledgement, which must never be credited to this one.
    StaleGeneration,
    /// More than one completion names this exact request.
    Duplicate,
}

/// The completion that acknowledges exactly the request begun at `recs[begin]`: same CPU, same
/// address space, same mapping (VA, old PA, new PA) and the same generation, after it. Pure.
///
/// A completion for the same mapping with a different generation is not skipped past: it is
/// reported, because the only way it can sit between this request's begin and its own
/// completion on the same CPU is if the generations were confused.
pub fn match_invalidation(recs: &[Rec], begin: usize) -> Result<usize, InvalRefusal> {
    let b = recs.get(begin).ok_or(InvalRefusal::Missing)?;
    if b.kind != Kind::InvalBegin {
        return Err(InvalRefusal::Missing);
    }
    let mut found: Option<usize> = None;
    for (i, r) in recs.iter().enumerate().skip(begin + 1) {
        if r.kind != Kind::InvalDone || r.cpu != b.cpu {
            continue;
        }
        let same_mapping =
            r.f[0] == b.f[0] && r.f[1] == b.f[1] && r.f[2] == b.f[2] && r.f[3] == b.f[3];
        if !same_mapping {
            continue;
        }
        if r.f[4] != b.f[4] {
            if found.is_none() {
                return Err(InvalRefusal::StaleGeneration);
            }
            continue;
        }
        if found.is_some() {
            return Err(InvalRefusal::Duplicate);
        }
        found = Some(i);
    }
    found.ok_or(InvalRefusal::Missing)
}

/// Why the SGI population does not balance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SgiRefusal {
    /// An arrival that no earlier send to that CPU from that source can explain.
    Unexplained { arrival: u32 },
    /// A send that no later arrival ever consumed.
    Undelivered { send: u32 },
    /// An arrival without exactly one completion of its own token before the next arrival.
    Completion { arrival: u32 },
}

/// Pair every send with the arrival that consumed it and every arrival with its one completion.
/// Sends from one source to one target that are still pending together are legitimately merged
/// by a GICv2 into one arrival; that is the only many-to-one allowed. Returns the number of
/// arrivals. Pure.
pub fn check_sgi_population(recs: &[Rec]) -> Result<usize, SgiRefusal> {
    let mut consumed = [false; SLOTS];
    let mut arrivals = 0usize;
    for (i, a) in recs.iter().enumerate() {
        if a.kind != Kind::SgiArrived {
            continue;
        }
        arrivals += 1;
        let mut explained = false;
        for (j, s) in recs.iter().enumerate().take(i) {
            if s.kind == Kind::SgiSent
                && s.f[0] == u64::from(a.cpu)
                && u64::from(s.cpu) == a.f[1]
                && !consumed.get(j).copied().unwrap_or(true)
            {
                consumed[j] = true;
                explained = true;
            }
        }
        if !explained {
            return Err(SgiRefusal::Unexplained { arrival: a.seq });
        }
        let next_arrival = recs
            .iter()
            .enumerate()
            .skip(i + 1)
            .find(|(_, r)| r.kind == Kind::SgiArrived && r.cpu == a.cpu)
            .map_or(recs.len(), |(k, _)| k);
        let completions = recs[i + 1..next_arrival]
            .iter()
            .filter(|r| r.kind == Kind::SgiCompleted && r.cpu == a.cpu && r.f[0] == a.f[0])
            .count();
        if completions != 1 {
            return Err(SgiRefusal::Completion { arrival: a.seq });
        }
    }
    for (j, s) in recs.iter().enumerate() {
        if s.kind == Kind::SgiSent && !consumed.get(j).copied().unwrap_or(true) {
            return Err(SgiRefusal::Undelivered { send: s.seq });
        }
    }
    Ok(arrivals)
}

/// The verdict: every check, named, with the first failure's reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    pub records: usize,
    pub sgi_arrivals: usize,
    /// Parked-target wakes accounted for (every round, both directions).
    pub p1_parked: usize,
    /// ... of which the SGI drove the dispatch at the target's idle boundary, per direction.
    pub p1_sgi_to_s: usize,
    pub p1_sgi_to_c: usize,
    /// ... of which CPU 0's periodic idle advance had already resumed the woken task when the
    /// SGI arrived (it then arrived in that task and returned to it).
    pub p1_timer_first: usize,
    pub p2_el0: usize,
    pub tlb_rounds: usize,
    pub mutual_rounds: usize,
    pub mutual_overlapped: usize,
    pub settled_after_ack: usize,
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

/// A wake from `from` to `to_cpu`, sent after `after`: the send and its arrival (origin as
/// required, `u64::MAX` for any; window when named). Returns the arrival's index.
fn wake_chain(
    recs: &[Rec],
    after: usize,
    from: Role,
    to_cpu: u8,
    origin: u64,
    window: u64,
) -> Result<usize, &'static str> {
    let sent = find_from(recs, after, |r| {
        r.kind == Kind::SgiSent && r.cpu == from.cpu && r.f[0] == u64::from(to_cpu) && r.f[2] == 0
    })
    .ok_or("sgi_not_sent")?;
    let arrived = find_from(recs, sent + 1, |r| {
        r.kind == Kind::SgiArrived && r.cpu == to_cpu && r.f[1] == u64::from(from.cpu)
    })
    .ok_or("sgi_not_arrived")?;
    let a = recs[arrived];
    if origin != u64::MAX && a.f[2] & 0xff != origin {
        return Err(if origin == ORIGIN_IDLE {
            "sgi_target_not_parked"
        } else {
            "sgi_target_not_in_el0"
        });
    }
    if window != WINDOW_NONE && (a.f[2] >> 8) != window {
        return Err("sgi_elr_outside_window");
    }
    Ok(arrived)
}

/// How a parked target was resumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParkedRoute {
    /// The SGI was taken at the target's authenticated idle boundary and its idle advance
    /// resumed exactly the woken task.
    Sgi,
    /// The target CPU's periodic idle advance resumed the woken task first; the SGI then arrived
    /// in that same task and returned to it. Only CPU 0 has a timer.
    TimerFirst,
}

/// A wake of the parked `woken` on `to_cpu` by `from`, after step `after`: the send, its one
/// arrival, and the dispatch that resumed `woken`, classified. Every other shape fails — a
/// dispatch of another task, a dispatch by neither route, an arrival in another task. Returns
/// the dispatch's index and route. Pure.
pub fn parked_wake(
    recs: &[Rec],
    after: usize,
    from: Role,
    to_cpu: u8,
    woken: Role,
) -> Result<(usize, ParkedRoute), &'static str> {
    let arrived = wake_chain(recs, after, from, to_cpu, u64::MAX, WINDOW_NONE)?;
    let a = recs[arrived];
    if a.f[2] & 0xff == ORIGIN_IDLE {
        let d = find_from(recs, arrived + 1, |r| {
            r.cpu == to_cpu && (r.kind == Kind::IdleDispatch || r.kind == Kind::SgiArrived)
        })
        .ok_or("sgi_dispatch_missing")?;
        let dr = recs[d];
        if dr.kind != Kind::IdleDispatch || dr.f[1] != 1 || dr.f[0] != woken.tid {
            return Err("sgi_dispatch_substituted");
        }
        return Ok((d, ParkedRoute::Sgi));
    }
    let d = recs[after..arrived]
        .iter()
        .position(|r| {
            r.kind == Kind::IdleDispatch && r.cpu == to_cpu && r.f[1] == 0 && r.f[0] == woken.tid
        })
        .map(|i| after + i)
        .ok_or("sgi_target_not_parked")?;
    if a.f[4] != woken.tid {
        return Err("sgi_arrived_in_another_task");
    }
    Ok((d, ParkedRoute::TimerFirst))
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
    match check_sgi_population(recs) {
        Ok(n) => v.sgi_arrivals = n,
        Err(SgiRefusal::Unexplained { arrival }) => v.fail("sgi_unexplained_arrival", arrival),
        Err(SgiRefusal::Undelivered { send }) => v.fail("sgi_undelivered", send),
        Err(SgiRefusal::Completion { arrival }) => {
            v.fail("sgi_completion_not_exactly_once", arrival)
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
                for (route, sgi) in [(to_s, &mut v.p1_sgi_to_s), (to_c, &mut v.p1_sgi_to_c)] {
                    match route {
                        ParkedRoute::Sgi => *sgi += 1,
                        ParkedRoute::TimerFirst => v.p1_timer_first += 1,
                    }
                }
                at = end;
            }
            Err(why) => v.fail(why, k as u32),
        }
    }
    // CPU 1 has no timer: every wake of S must be the SGI's. On CPU 0 the timer may win a race it
    // is entitled to win, but not the whole phase.
    if v.p1_sgi_to_s < P1_ROUNDS as usize || v.p1_sgi_to_c < P1_MIN_SGI_TO_C {
        v.fail("p1_too_few_sgi_driven_parked_dispatches", 0);
    }

    // P2 — EL0 targets inside the checked windows.
    let p2a = (|| -> Result<usize, &'static str> {
        let call = user(recs, at, c, "C_P2A_CALL", None).ok_or("p2a_call_missing")?;
        let arr = wake_chain(recs, call, c, s.cpu, ORIGIN_USER, WINDOW_S_A)?;
        if recs[arr].f[4] != s.tid {
            return Err("p2a_arrival_not_in_server");
        }
        user(recs, arr, s, "S_WIN_A_OK", None).ok_or("p2a_window_check_missing")
    })();
    match p2a {
        Ok(end) => {
            v.p2_el0 += 1;
            at = end;
        }
        Err(why) => v.fail(why, 0),
    }
    let p2b = (|| -> Result<usize, &'static str> {
        let call = user(recs, at, s, "S_P2B_CALL", None).ok_or("p2b_call_missing")?;
        let arr = wake_chain(recs, call, s, c.cpu, ORIGIN_USER, WINDOW_C_B)?;
        if recs[arr].f[4] != c.tid {
            return Err("p2b_arrival_not_in_client");
        }
        user(recs, arr, c, "C_WIN_B_OK", None).ok_or("p2b_window_check_missing")
    })();
    match p2b {
        Ok(_) => v.p2_el0 += 1,
        Err(why) => v.fail(why, 0),
    }

    // P3 — serial remote-invalidation rounds.
    for r in 1..=4u64 {
        let (t, q) = if r % 2 == 1 { (s, c) } else { (c, s) };
        let (tp, tq) = if r % 2 == 1 { ("S", "C") } else { ("C", "S") };
        let primed_name = if tp == "S" { "S_PRIMED" } else { "C_PRIMED" };
        let observed_name = if tp == "S" {
            "S_OBSERVED"
        } else {
            "C_OBSERVED"
        };
        let arm_name = if tq == "S" { "S_ARM_R" } else { "C_ARM_R" };
        let round = (|| -> Result<(), &'static str> {
            let primed = user(recs, 0, t, primed_name, Some(r)).ok_or("tlb_primed_missing")?;
            let arm = user(recs, primed, q, arm_name, Some(r)).ok_or("tlb_arm_missing")?;
            find_from(recs, arm, |x| {
                x.kind == Kind::RRepoint && x.f[0] == t.asid && x.f[3] == r
            })
            .ok_or("tlb_probe_not_repointed")?;
            let begin = find_from(recs, arm, |x| {
                x.kind == Kind::InvalBegin
                    && x.cpu == q.cpu
                    && x.f[0] == t.asid
                    && x.f[1] == roles.w_va
            })
            .ok_or("tlb_request_missing")?;
            let done = match match_invalidation(recs, begin) {
                Ok(d) => d,
                Err(InvalRefusal::StaleGeneration) => return Err("tlb_stale_acknowledgement"),
                Err(InvalRefusal::Duplicate) => return Err("tlb_duplicate_acknowledgement"),
                Err(InvalRefusal::Missing) => return Err("tlb_acknowledgement_missing"),
            };
            let observed =
                user(recs, primed, t, observed_name, Some(r)).ok_or("tlb_observation_missing")?;
            if observed < done {
                return Err("tlb_observed_before_acknowledgement");
            }
            let p = recs[primed];
            if recs[observed].cpu != p.cpu || p.cpu == q.cpu || recs[begin].f[0] != p.f[1] {
                return Err("tlb_target_not_resident_remote");
            }
            Ok(())
        })();
        match round {
            Ok(()) => v.tlb_rounds += 1,
            Err(why) => v.fail(why, r as u32),
        }
    }

    // P4 — mutual rounds.
    for m in 1..=4u64 {
        let round = (|| -> Result<bool, &'static str> {
            let sn = user(recs, 0, s, "S_MUT_NR3", Some(m)).ok_or("mut_request_missing")?;
            let cn = user(recs, 0, c, "C_MUT_NR3", Some(m)).ok_or("mut_request_missing")?;
            let mut dones = [0usize; 2];
            for (slot, (req, target)) in [(s, c), (c, s)].into_iter().enumerate() {
                let from = if req.tid == s.tid { sn } else { cn };
                let begin = find_from(recs, from, |x| {
                    x.kind == Kind::InvalBegin
                        && x.cpu == req.cpu
                        && x.f[0] == target.asid
                        && x.f[1] == roles.w_va
                })
                .ok_or("mut_invalidation_missing")?;
                dones[slot] =
                    match_invalidation(recs, begin).map_err(|_| "mut_acknowledgement_invalid")?;
            }
            user(recs, 0, s, "S_MUT_OK", Some(m)).ok_or("mut_observation_missing")?;
            user(recs, 0, c, "C_MUT_OK", Some(m)).ok_or("mut_observation_missing")?;
            Ok(sn.max(cn) < dones[0].min(dones[1]))
        })();
        match round {
            Ok(overlap) => {
                v.mutual_rounds += 1;
                v.mutual_overlapped += usize::from(overlap);
            }
            Err(why) => v.fail(why, m as u32),
        }
    }

    // Reclaim only after the acknowledgement, exactly once, for every displaced witness page.
    for (i, d) in recs.iter().enumerate() {
        if d.kind != Kind::VmDisplaced || d.f[1] != roles.w_va {
            continue;
        }
        let acked = recs[..i].iter().rposition(|x| {
            x.kind == Kind::InvalDone
                && x.cpu == d.cpu
                && x.f[0] == d.f[0]
                && x.f[1] == d.f[1]
                && x.f[2] == d.f[2]
        });
        let Some(acked) = acked else {
            v.fail("displaced_without_acknowledgement", d.seq);
            continue;
        };
        let _ = acked;
        let settled = recs[i + 1..]
            .iter()
            .filter(|x| {
                x.kind == Kind::VmSettled
                    && x.f[0] == d.f[0]
                    && x.f[1] == d.f[1]
                    && x.f[2] == d.f[2]
            })
            .count();
        if settled != 1 {
            v.fail("settlement_not_exactly_once_after_ack", d.seq);
        } else {
            v.settled_after_ack += 1;
        }
    }
    // No displaced backing is released before the invalidation that retired it completed.
    let premature = recs.iter().enumerate().any(|(i, x)| {
        x.kind == Kind::VmSettled
            && x.f[1] == roles.w_va
            && !recs[..i].iter().any(|y| {
                y.kind == Kind::InvalDone
                    && y.f[0] == x.f[0]
                    && y.f[1] == x.f[1]
                    && y.f[2] == x.f[2]
            })
    });
    if premature {
        v.fail("settled_before_acknowledgement", 0);
    }
    if v.tlb_rounds + v.mutual_rounds * 2 > v.settled_after_ack {
        v.fail("displaced_pages_unsettled", 0);
    }
    if user(recs, 0, s, "S_DONE", None).is_none() || user(recs, 0, c, "C_DONE", None).is_none() {
        v.fail("witness_not_complete", 0);
    }
    v
}

// ─────────────────────────────── the recorder ───────────────────────────────

#[cfg(any(test, all(feature = "aarch64-smp2-witness", target_arch = "aarch64")))]
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

    /// A fresh request generation for the page-table owner.
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

    /// The sealed record: every slot claimed so far, once each has been published. `None` when a
    /// claimed slot never completes within `spins` (it would be read half-written).
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

#[cfg(any(test, all(feature = "aarch64-smp2-witness", target_arch = "aarch64")))]
pub use recorder::{arm, armed, next_generation, push, seal};

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn rec(seq: u32, kind: Kind, cpu: u8, f: [u64; 5]) -> Rec {
        Rec { seq, kind, cpu, f }
    }

    fn seqd(v: Vec<(Kind, u8, [u64; 5])>) -> Vec<Rec> {
        v.into_iter()
            .enumerate()
            .map(|(i, (k, c, f))| rec(i as u32, k, c, f))
            .collect()
    }

    const W: u64 = 0x2008_0000;

    #[test]
    fn an_acknowledgement_is_credited_only_to_its_own_request() {
        let r = seqd(vec![
            (Kind::InvalBegin, 0, [5, W, 0x1000, 0x2000, 1]),
            (Kind::InvalDone, 0, [5, W, 0x1000, 0x2000, 1]),
        ]);
        assert_eq!(match_invalidation(&r, 0), Ok(1));
    }

    #[test]
    fn a_stale_acknowledgement_is_refused_not_skipped() {
        // An older request's completion (generation 1) found after generation 2 began.
        let r = seqd(vec![
            (Kind::InvalBegin, 0, [5, W, 0x1000, 0x2000, 2]),
            (Kind::InvalDone, 0, [5, W, 0x1000, 0x2000, 1]),
            (Kind::InvalDone, 0, [5, W, 0x1000, 0x2000, 2]),
        ]);
        assert_eq!(
            match_invalidation(&r, 0),
            Err(InvalRefusal::StaleGeneration)
        );
    }

    #[test]
    fn an_acknowledgement_for_another_mapping_or_cpu_does_not_count() {
        let r = seqd(vec![
            (Kind::InvalBegin, 0, [5, W, 0x1000, 0x2000, 3]),
            (Kind::InvalDone, 1, [5, W, 0x1000, 0x2000, 3]),
            (Kind::InvalDone, 0, [6, W, 0x1000, 0x2000, 3]),
            (Kind::InvalDone, 0, [5, W + 0x1000, 0x1000, 0x2000, 3]),
        ]);
        assert_eq!(match_invalidation(&r, 0), Err(InvalRefusal::Missing));
    }

    #[test]
    fn a_completion_before_the_request_is_not_its_acknowledgement() {
        let r = seqd(vec![
            (Kind::InvalDone, 0, [5, W, 0x1000, 0x2000, 4]),
            (Kind::InvalBegin, 0, [5, W, 0x1000, 0x2000, 4]),
        ]);
        assert_eq!(match_invalidation(&r, 1), Err(InvalRefusal::Missing));
    }

    #[test]
    fn a_duplicated_acknowledgement_fails() {
        let r = seqd(vec![
            (Kind::InvalBegin, 0, [5, W, 0x1000, 0x2000, 7]),
            (Kind::InvalDone, 0, [5, W, 0x1000, 0x2000, 7]),
            (Kind::InvalDone, 0, [5, W, 0x1000, 0x2000, 7]),
        ]);
        assert_eq!(match_invalidation(&r, 0), Err(InvalRefusal::Duplicate));
    }

    #[test]
    fn concurrent_requests_from_both_cpus_each_match_their_own_completion() {
        // Mutual round: CPU 0 invalidates asid 6 while CPU 1 invalidates asid 5, interleaved.
        let r = seqd(vec![
            (Kind::InvalBegin, 0, [6, W, 0x3000, 0x4000, 10]),
            (Kind::InvalBegin, 1, [5, W, 0x1000, 0x2000, 11]),
            (Kind::InvalDone, 1, [5, W, 0x1000, 0x2000, 11]),
            (Kind::InvalDone, 0, [6, W, 0x3000, 0x4000, 10]),
        ]);
        assert_eq!(match_invalidation(&r, 0), Ok(3));
        assert_eq!(match_invalidation(&r, 1), Ok(2));
    }

    #[test]
    fn every_sgi_is_delivered_and_completed_exactly_once() {
        let ok = seqd(vec![
            (Kind::SgiSent, 0, [1, 0x20001, 0, 0, 0]),
            (Kind::SgiArrived, 1, [0x401, 0, 1, 0, 0]),
            (Kind::SgiCompleted, 1, [0x401, 0, 0, 0, 0]),
        ]);
        assert_eq!(check_sgi_population(&ok), Ok(1));
        // Suppressed transmission: an arrival nothing sent.
        let unexplained = seqd(vec![(Kind::SgiArrived, 1, [0x401, 0, 1, 0, 0])]);
        assert!(matches!(
            check_sgi_population(&unexplained),
            Err(SgiRefusal::Unexplained { .. })
        ));
        // Sent and never taken.
        let lost = seqd(vec![(Kind::SgiSent, 0, [1, 0x20001, 0, 0, 0])]);
        assert!(matches!(
            check_sgi_population(&lost),
            Err(SgiRefusal::Undelivered { .. })
        ));
        // Omitted completion.
        let no_eoi = seqd(vec![
            (Kind::SgiSent, 0, [1, 0x20001, 0, 0, 0]),
            (Kind::SgiArrived, 1, [0x401, 0, 1, 0, 0]),
        ]);
        assert!(matches!(
            check_sgi_population(&no_eoi),
            Err(SgiRefusal::Completion { .. })
        ));
        // A completion carrying another token is not this arrival's.
        let wrong = seqd(vec![
            (Kind::SgiSent, 0, [1, 0x20001, 0, 0, 0]),
            (Kind::SgiArrived, 1, [0x401, 0, 1, 0, 0]),
            (Kind::SgiCompleted, 1, [0x001, 0, 0, 0, 0]),
        ]);
        assert!(matches!(
            check_sgi_population(&wrong),
            Err(SgiRefusal::Completion { .. })
        ));
        // Two sends pending together merge into one arrival; that is the GIC, not a loss.
        let merged = seqd(vec![
            (Kind::SgiSent, 0, [1, 0x20001, 0, 0, 0]),
            (Kind::SgiSent, 0, [1, 0x20001, 0, 0, 0]),
            (Kind::SgiArrived, 1, [0x401, 0, 1, 0, 0]),
            (Kind::SgiCompleted, 1, [0x401, 0, 0, 0, 0]),
        ]);
        assert_eq!(check_sgi_population(&merged), Ok(1));
    }

    #[test]
    fn a_parked_wake_resumes_exactly_the_task_it_woke() {
        let s = Role {
            tid: 9200,
            asid: 5,
            cpu: 1,
        };
        let c = Role {
            tid: 9201,
            asid: 6,
            cpu: 0,
        };
        let call = (Kind::User, 0, [c.tid, c.asid, code("C_P1_CALL"), 1, 0]);
        let sent = (Kind::SgiSent, 0, [1, 0x20001, 0, 0, 0]);
        let at_idle = |tid| (Kind::SgiArrived, 1, [0x401, 0, ORIGIN_IDLE, 0, tid]);
        let in_el0 = |tid| {
            (
                Kind::SgiArrived,
                1,
                [0x401, 0, ORIGIN_USER, 0x2000_0000, tid],
            )
        };
        let by_sgi = |tid| (Kind::IdleDispatch, 1, [tid, 1, 0, 0, 0]);
        let by_timer = |tid| (Kind::IdleDispatch, 1, [tid, 0, 0, 0, 0]);
        // The SGI taken at the idle boundary resumes exactly the woken task.
        let r = seqd(vec![call, sent, at_idle(0), by_sgi(s.tid)]);
        assert_eq!(parked_wake(&r, 0, c, 1, s), Ok((3, ParkedRoute::Sgi)));
        // Another task's continuation substituted for the woken one.
        let r = seqd(vec![call, sent, at_idle(0), by_sgi(9202)]);
        assert_eq!(parked_wake(&r, 0, c, 1, s), Err("sgi_dispatch_substituted"));
        // The SGI's own dispatch is missing: the next event on that CPU is another arrival.
        let r = seqd(vec![call, sent, at_idle(0), sent, at_idle(0)]);
        assert_eq!(parked_wake(&r, 0, c, 1, s), Err("sgi_dispatch_substituted"));
        // The timer's idle advance won the race: accepted only when it resumed the woken task
        // and the SGI then arrived in that same task.
        let r = seqd(vec![call, by_timer(s.tid), sent, in_el0(s.tid)]);
        assert_eq!(
            parked_wake(&r, 0, c, 1, s),
            Ok((1, ParkedRoute::TimerFirst))
        );
        let r = seqd(vec![call, by_timer(9202), sent, in_el0(9202)]);
        assert_eq!(parked_wake(&r, 0, c, 1, s), Err("sgi_target_not_parked"));
        let r = seqd(vec![call, by_timer(s.tid), sent, in_el0(9202)]);
        assert_eq!(
            parked_wake(&r, 0, c, 1, s),
            Err("sgi_arrived_in_another_task")
        );
        // Neither route: the target was running when the SGI arrived, with no idle dispatch.
        let r = seqd(vec![call, sent, in_el0(s.tid)]);
        assert_eq!(parked_wake(&r, 0, c, 1, s), Err("sgi_target_not_parked"));
        // An SGI-trigger dispatch cannot stand in for a timer-won race, nor the reverse.
        let r = seqd(vec![call, by_sgi(s.tid), sent, in_el0(s.tid)]);
        assert_eq!(parked_wake(&r, 0, c, 1, s), Err("sgi_target_not_parked"));
        let r = seqd(vec![call, sent, at_idle(0), by_timer(s.tid)]);
        assert_eq!(parked_wake(&r, 0, c, 1, s), Err("sgi_dispatch_substituted"));
    }

    #[test]
    fn step_codes_are_exact_and_failures_are_never_accepted() {
        assert_eq!(step_code("SMP2 S_DONE"), Some(13));
        assert_eq!(step_code("SMP2 S_FAIL_PAYLOAD"), Some(STEP_FAIL + 1));
        assert_eq!(step_code("SMP2 S_UNHEARD_OF"), Some(STEP_FAIL));
        assert_eq!(step_code("OTHER"), None);
        for (name, c) in STEPS {
            assert_eq!(step_name(*c), *name);
            assert!(*c < STEP_FAIL);
        }
    }

    #[test]
    fn the_recorder_seals_only_published_slots() {
        arm();
        let before = {
            let mut out = [None; SLOTS];
            seal(&mut out, 1).map(|(n, _)| n).unwrap_or(0)
        };
        push(Kind::SgiCompleted, 1, [9, 0, 0, 0, 0]);
        let mut out = [None; SLOTS];
        let (n, overflow) = seal(&mut out, 1).expect("every claimed slot is published");
        assert_eq!(n, before + 1);
        assert!(!overflow);
        let last = out[n - 1].expect("sealed");
        assert_eq!((last.kind, last.cpu, last.f[0]), (Kind::SgiCompleted, 1, 9));
        assert!(next_generation() < next_generation());
    }
}
