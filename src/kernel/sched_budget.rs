// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! RT-SCHED1B — scheduling contexts and budgets (`doc/SCHEDULING_CONTRACT.md` §24).
//!
//! One CPU's contexts live inside that CPU's scheduler state ([`super::scheduler::CpuSched`]),
//! under its own lock (rank 1L): every charge, replenishment, exhaustion and rejoin is a mutation
//! of the CPU's own queues and ledger, so no new lock exists and no other CPU ever touches them.
//!
//! # The clock
//!
//! Both execution and release instants are measured in the **global scheduler tick** — the one
//! counter only the clock owner advances (SC-TIME-2), read by every CPU. Every 1L section of a
//! budgeted CPU begins with [`BudgetLedger::account`]: the interval since that CPU's previous
//! section is charged to whoever was current during it (its context, the idle ledger, or the
//! unaccounted ledger) and the section's start becomes the next interval's start. Nothing changes
//! `current` outside a 1L section, so consecutive intervals telescope: on each CPU every elapsed
//! tick is charged exactly once, to exactly one owner. The granularity is one tick.
//!
//! # The rules (SC-CTX-1..8)
//!
//! * A context is `{cpu, priority, B, T}` with `0 < B ≤ T`, bound to ONE thread incarnation on its
//!   home CPU at that incarnation's first placement ([`BudgetParams::validate`]).
//! * Charging: `use = min(remaining, elapsed)`; the rest is **overrun**, added to `debt` with
//!   checked arithmetic. A debt that would wrap saturates at `u64::MAX` and sets the overflow
//!   latch (SCHED-OVERFLOW1): a latched context is ineligible for good (no recovery owner exists).
//! * Exhausted = `remaining == 0` or latched. An exhausted context holds no queue position
//!   (SC-CTX-8): at the next scheduling point it is PARKED — still Ready, holding a reservation of
//!   one slot at its level (a kernel thread uses its kernel-thread headroom instead), so the rejoin
//!   at replenishment can never be refused for capacity.
//! * Replenishment at the fixed instants `bind + kT`: each pays `min(B, debt)`, leaves
//!   `remaining = B − paid`, carries the rest of the debt; the previous `remaining` is discarded.
//!   Several due instants are applied in one O(1) step with the same result as one at a time.
//! * Order at one instant (SC-CTX-7): charge, then every due replenishment in ascending context
//!   id (slot order on the CPU), then the scheduling decision. A parked context left with budget
//!   rejoins its level's tail, once, in that order.

/// How many scheduling contexts one CPU holds (SC-POOL-2: a fixed pool). A binding beyond it is
/// refused with `SchedulerError::ContextPoolFull`; nothing falls back to an unbudgeted thread.
pub const CTX_PER_CPU: usize = 32;
// §24.4: every thread placed on a budgeted CPU holds one of its contexts, so the pool bounds the
// CPU's entries below one level's admission bound — no queue can refuse a budgeted CPU's work.
const _: () = assert!(CTX_PER_CPU < crate::kernel::scheduler::MAX_RUN_QUEUE);
/// The default template for a thread the deployment configuration gives no budget of its own:
/// `B = T`, a full budget every period. It is accounted like every other context and can never
/// be exhausted by execution alone (a thread that runs the whole period is charged its last tick
/// at the release instant, which replenishes it in the same instant first, SC-CTX-7).
pub const BUDGET_DEFAULT_PERIOD: u64 = 64;
/// The longest period a configuration may state (validation bound).
pub const BUDGET_MAX_PERIOD: u64 = 1 << 32;

/// A validated `(B, T)`: `0 < budget ≤ period ≤ BUDGET_MAX_PERIOD`, in scheduler ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetParams {
    budget: u64,
    period: u64,
}

/// Why a budget configuration was refused. Nothing is written on a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetRefusal {
    ZeroBudget,
    BudgetAbovePeriod,
    PeriodTooLong,
}

impl BudgetParams {
    pub const DEFAULT: BudgetParams = BudgetParams {
        budget: BUDGET_DEFAULT_PERIOD,
        period: BUDGET_DEFAULT_PERIOD,
    };

    pub const fn validate(budget: u64, period: u64) -> Result<Self, BudgetRefusal> {
        if budget == 0 {
            Err(BudgetRefusal::ZeroBudget)
        } else if budget > period {
            Err(BudgetRefusal::BudgetAbovePeriod)
        } else if period > BUDGET_MAX_PERIOD {
            Err(BudgetRefusal::PeriodTooLong)
        } else {
            Ok(Self { budget, period })
        }
    }

    pub const fn budget(self) -> u64 {
        self.budget
    }

    pub const fn period(self) -> u64 {
        self.period
    }
}

/// Where a parked (exhausted, Ready) context's guaranteed rejoin slot comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Park {
    /// A user thread: one reservation at its level, counted by the admission rules.
    Reserved,
    /// A kernel thread: the kernel-thread headroom of its level (`MAX_QUEUED_PER_PRIORITY`).
    KernelThread,
}

/// One scheduling context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedContext {
    pub(crate) tid: u64,
    /// The incarnation stamp it was bound to (`0`: bound by a path that carries none).
    pub(crate) stamp: u64,
    pub(crate) params: BudgetParams,
    pub(crate) remaining: u64,
    pub(crate) debt: u64,
    pub(crate) next_release: u64,
    pub(crate) bound_at: u64,
    pub(crate) latched: bool,
    /// `Some((how, level))` while exhausted and Ready: no queue position, a guaranteed rejoin slot
    /// at `level`.
    pub(crate) parked: Option<(Park, u8)>,
    /// Observation only: ticks charged, of which overrun; exhaustions; releases applied.
    pub(crate) charged: u64,
    pub(crate) overrun: u64,
    pub(crate) exhaustions: u32,
    pub(crate) releases: u64,
}

impl SchedContext {
    fn new(tid: u64, stamp: u64, params: BudgetParams, now: u64) -> Self {
        Self {
            tid,
            stamp,
            params,
            remaining: params.budget,
            debt: 0,
            next_release: now.saturating_add(params.period),
            bound_at: now,
            latched: false,
            parked: None,
            charged: 0,
            overrun: 0,
            exhaustions: 0,
            releases: 0,
        }
    }

    /// SC-CTX-3: ineligible whatever its priority.
    pub fn exhausted(&self) -> bool {
        self.remaining == 0 || self.latched
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }
    pub fn debt(&self) -> u64 {
        self.debt
    }
    pub fn latched(&self) -> bool {
        self.latched
    }
    pub fn parked(&self) -> bool {
        self.parked.is_some()
    }
    pub fn next_release(&self) -> u64 {
        self.next_release
    }
    pub fn params(&self) -> BudgetParams {
        self.params
    }
    pub fn charged(&self) -> u64 {
        self.charged
    }
    pub fn overrun(&self) -> u64 {
        self.overrun
    }
    pub fn exhaustions(&self) -> u32 {
        self.exhaustions
    }
    pub fn releases(&self) -> u64 {
        self.releases
    }
    pub fn bound_at(&self) -> u64 {
        self.bound_at
    }
}

/// What one charge did (for the witness and tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Charged {
    pub(crate) elapsed: u64,
    pub(crate) overrun: u64,
    pub(crate) newly_exhausted: bool,
}

/// SC-CTX-4/5 — charge `elapsed` ticks to `ctx`.
pub(crate) fn charge(ctx: &mut SchedContext, elapsed: u64) -> Charged {
    let was = ctx.exhausted();
    ctx.charged = ctx.charged.saturating_add(elapsed);
    if ctx.latched {
        return Charged {
            elapsed,
            overrun: elapsed,
            newly_exhausted: false,
        };
    }
    let used = ctx.remaining.min(elapsed);
    ctx.remaining -= used;
    let over = elapsed - used;
    if over > 0 {
        ctx.overrun = ctx.overrun.saturating_add(over);
        match ctx.debt.checked_add(over) {
            Some(d) => ctx.debt = d,
            None => {
                // SCHED-OVERFLOW1: saturate and latch. The excess beyond u64::MAX is lost; the
                // latch is what stops that loss from ever becoming usable budget.
                ctx.debt = u64::MAX;
                ctx.latched = true;
            }
        }
    }
    let newly = !was && ctx.exhausted();
    if newly {
        ctx.exhaustions = ctx.exhaustions.saturating_add(1);
    }
    Charged {
        elapsed,
        overrun: over,
        newly_exhausted: newly,
    }
}

/// What one replenishment step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Replenished {
    /// The LAST release instant applied (`bound + kT`).
    pub(crate) release: u64,
    /// How many release instants were due.
    pub(crate) count: u64,
}

/// SC-CTX-4 — apply every release instant `<= now` to `ctx`, in order, in O(1). `None` when none
/// is due. Equal to applying them one at a time: each pays `min(B, debt)`, sets
/// `remaining = B − paid` (discarding the previous remaining) and carries the rest of the debt.
/// A latched context's instants advance and nothing is paid or credited (SCHED-OVERFLOW1).
pub(crate) fn replenish_due(ctx: &mut SchedContext, now: u64) -> Option<Replenished> {
    if ctx.next_release > now {
        return None;
    }
    let period = ctx.params.period;
    let budget = ctx.params.budget;
    let n = (now - ctx.next_release) / period + 1;
    let last = ctx
        .next_release
        .saturating_add((n - 1).saturating_mul(period));
    ctx.next_release = last.saturating_add(period);
    ctx.releases = ctx.releases.saturating_add(n);
    if !ctx.latched {
        // The first n - 1 releases pay min((n - 1) B, debt) between them; the last pays what it
        // can of the rest and leaves B minus that.
        let before_last = ctx.debt - ctx.debt.min((n - 1).saturating_mul(budget));
        let paid_last = budget.min(before_last);
        ctx.remaining = budget - paid_last;
        ctx.debt = before_last - paid_last;
    }
    Some(Replenished {
        release: last,
        count: n,
    })
}

/// Per-CPU ledger: the context pool and the CPU's accounting interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BudgetLedger {
    pub(crate) enabled: bool,
    /// The CPU this ledger belongs to (set at its first section; observation only).
    pub(crate) cpu: u8,
    pub(crate) ctx: [Option<SchedContext>; CTX_PER_CPU],
    /// The section start the CPU's next interval is measured from.
    pub(crate) acct: u64,
    /// The instant of the section in progress (the global tick read when it began).
    pub(crate) now: u64,
    /// The earliest release instant of any bound context (`u64::MAX`: none).
    pub(crate) earliest: u64,
    /// Ticks charged to no context: an empty slot or the idle placeholder.
    pub(crate) idle: u64,
    /// Ticks charged while a thread with NO context was current (a budgeted CPU must keep this 0).
    pub(crate) unaccounted: u64,
    /// The essential-work ledger (SC-EXEC-5, enforcement PARTITION1): timer interrupts and RT
    /// reschedule arrivals settled on this CPU. Counted, not timed — at tick granularity their
    /// time lies inside the interval of whoever they interrupted.
    pub(crate) essential_ticks: u64,
    pub(crate) essential_arrivals: u64,
}

impl Default for BudgetLedger {
    fn default() -> Self {
        Self {
            enabled: false,
            cpu: 0,
            ctx: [None; CTX_PER_CPU],
            acct: 0,
            now: 0,
            earliest: u64::MAX,
            idle: 0,
            unaccounted: 0,
            essential_ticks: 0,
            essential_arrivals: 0,
        }
    }
}

impl BudgetLedger {
    pub(crate) fn slot_of(&self, tid: u64) -> Option<usize> {
        self.ctx
            .iter()
            .position(|c| c.is_some_and(|c| c.tid == tid))
    }

    pub(crate) fn get(&self, tid: u64) -> Option<&SchedContext> {
        self.slot_of(tid).and_then(|i| self.ctx[i].as_ref())
    }

    pub(crate) fn get_mut(&mut self, tid: u64) -> Option<&mut SchedContext> {
        let i = self.slot_of(tid)?;
        self.ctx[i].as_mut()
    }

    pub(crate) fn recompute_earliest(&mut self) {
        self.earliest = self
            .ctx
            .iter()
            .flatten()
            .map(|c| c.next_release)
            .min()
            .unwrap_or(u64::MAX);
    }

    /// Bind (or rebind) `tid`'s incarnation `stamp`. An existing binding of the SAME incarnation
    /// (or one bound with no stamp) is kept; a binding of an EARLIER incarnation of the TID is
    /// replaced by a fresh context — a reused TID inherits no budget, debt or latch. `None`: the
    /// pool is full.
    pub(crate) fn bind(&mut self, tid: u64, stamp: u64, params: BudgetParams) -> Option<usize> {
        let now = self.now;
        if let Some(i) = self.slot_of(tid) {
            let c = self.ctx[i].as_mut()?;
            if stamp != 0 && c.stamp != stamp {
                // An earlier incarnation's context (a reused TID), or the default template a
                // stampless path bound before the configured binding: a fresh context. A parked
                // entry keeps its rejoin slot (`parked` is carried), so nothing is lost or doubled.
                let parked = c.parked;
                *c = SchedContext::new(tid, stamp, params, now);
                c.parked = parked;
                self.recompute_earliest();
            }
            return Some(i);
        }
        let i = self.ctx.iter().position(|c| c.is_none())?;
        self.ctx[i] = Some(SchedContext::new(tid, stamp, params, now));
        self.earliest = self.earliest.min(now.saturating_add(params.period));
        Some(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(b: u64, t: u64) -> SchedContext {
        SchedContext::new(1, 1, BudgetParams::validate(b, t).unwrap(), 0)
    }

    #[test]
    fn rt1b_params_require_zero_below_budget_at_most_period() {
        assert_eq!(BudgetParams::validate(0, 5), Err(BudgetRefusal::ZeroBudget));
        assert_eq!(
            BudgetParams::validate(6, 5),
            Err(BudgetRefusal::BudgetAbovePeriod)
        );
        assert_eq!(
            BudgetParams::validate(1, BUDGET_MAX_PERIOD + 1),
            Err(BudgetRefusal::PeriodTooLong)
        );
        assert!(BudgetParams::validate(5, 5).is_ok());
    }

    /// SC-CTX-4's example: budget 3, debt 8 → (0, 5), (0, 2), (1, 0), one release at a time and
    /// all at once alike.
    #[test]
    fn rt1b_debt_spanning_several_periods_is_repaid_and_never_forgiven() {
        let mut c = ctx(3, 5);
        assert_eq!(charge(&mut c, 11).overrun, 8);
        assert_eq!((c.remaining, c.debt), (0, 8));
        let mut steps = alloc::vec::Vec::new();
        for now in [5, 10, 15] {
            replenish_due(&mut c, now).unwrap();
            steps.push((c.remaining, c.debt));
        }
        assert_eq!(steps, [(0, 5), (0, 2), (1, 0)]);
        let mut d = ctx(3, 5);
        charge(&mut d, 11);
        let r = replenish_due(&mut d, 17).unwrap();
        assert_eq!((r.count, r.release, d.remaining, d.debt), (3, 15, 1, 0));
        assert_eq!(d.next_release, 20);
    }

    /// Unused budget is discarded, not accumulated.
    #[test]
    fn rt1b_unused_budget_is_discarded_at_replenishment() {
        let mut c = ctx(3, 5);
        charge(&mut c, 1);
        replenish_due(&mut c, 5).unwrap();
        assert_eq!(c.remaining, 3);
        replenish_due(&mut c, 30).unwrap();
        assert_eq!((c.remaining, c.debt), (3, 0));
    }

    /// SCHED-OVERFLOW1, constructed state: a debt that would wrap saturates and latches; no
    /// replenishment ever credits a latched context, whatever its counters say.
    #[test]
    fn rt1b_debt_overflow_saturates_and_latches_through_replenishment_constructed() {
        let mut c = ctx(3, 5);
        c.debt = u64::MAX - 1;
        charge(&mut c, 10);
        assert!(c.latched && c.debt == u64::MAX && c.exhausted());
        // Constructed: even a restored budget and a cleared debt do not make it eligible.
        c.remaining = 3;
        c.debt = 0;
        assert!(c.exhausted());
        replenish_due(&mut c, 1_000).unwrap();
        assert!(c.exhausted() && c.latched);
        assert!(c.next_release > 1_000, "its release instants still advance");
    }

    /// A budget of `T` charged the whole period is not exhausted after the same instant's
    /// replenishment (the default template).
    #[test]
    fn rt1b_full_budget_is_replenished_at_the_instant_it_runs_out() {
        let mut c = ctx(5, 5);
        assert!(charge(&mut c, 5).newly_exhausted);
        replenish_due(&mut c, 5).unwrap();
        assert!(!c.exhausted());
    }
}
