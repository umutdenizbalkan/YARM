import pathlib
R = pathlib.Path('src/runtime.rs'); r = R.read_text()
I = pathlib.Path('src/kernel/boot/ipc_state.rs'); i = I.read_text()


def rep(s, old, new, count=1):
    assert s.count(old) == count, (old[:90], s.count(old))
    return s.replace(old, new)


# ---- runtime: the promise type + helpers, replacing the refusable helper ----
start = r.index("    /// Stage 200C2B — the OFF-LOCK scheduler enqueue for a timeout-woken caller.")
end = r.index("    /// Stage 200C1 — the NR7 reply-win deadline-disarm hook.")
r = r[:start] + '''    /// SCHED-TIME-FOUNDATION1 A (IPC-TIMEOUT-CAP1) — promise `tid` a slot on its home run queue
    /// BEFORE an IPC terminal owner claims it, so the claim can never leave a task `Runnable`
    /// without a placement.
    ///
    /// The four off-lock IPC terminal owners — the reply-receive timeout
    /// (`complete_reply_timeout_over`), server death (`complete_server_death_over`), the
    /// blocking-send timeout and the receive timeout — each used to make the task `Runnable` under
    /// the task lock and only then try an ordinary, refusable enqueue whose `QueueFull` they
    /// discarded (`is_ok_and`). Now each reserves here first (FUTEX-WAKE-CAP1's order): plan at
    /// rank 2 — the task's fixed home and its class's priority — then the promise under that
    /// CPU's 1L only. `None` when the home queue has no room (or the task has no plan): the
    /// caller then claims NOTHING and keeps its obligation (the armed deadline, or the parked
    /// death) for a later pass. Every `Some` is consumed by exactly one of
    /// [`Self::commit_ipc_wake_split`] or [`Self::release_ipc_wake_split`].
    pub(crate) fn reserve_ipc_wake_split(&self, tid: u64, cpu: CpuId) -> Option<IpcWakePlacement> {
        use crate::kernel::task_enqueue::{self as te, EnqueueTarget};
        let mut plan = self
            .with_task_enqueue_policy_split_mut(|tcbs, classes| {
                te::plan_balanced_enqueue_locked(tcbs, classes, tid, cpu)
            })
            .ok()?;
        self.resolve_plan_home_split(&mut plan, cpu);
        let EnqueueTarget::Pinned(home) = plan.target else {
            return None;
        };
        self.with_cpu_sched_split(home, |c| te::reserve_enqueue_on_cpu_locked(c, &plan))
            .map_err(crate::kernel::boot::map_scheduler_error)
            .and_then(|r| r)
            .ok()
            .map(|()| IpcWakePlacement { plan, home })
    }

    /// IPC-TIMEOUT-CAP1 — fill a promise after its owner's claim committed. It cannot be refused
    /// for capacity; the destination's 1L only, the IPI after its release.
    pub(crate) fn commit_ipc_wake_split(&self, placement: IpcWakePlacement) {
        use crate::kernel::task_enqueue::{self as te, ReservedCommit};
        match self.with_cpu_sched_split(placement.home, |c| {
            te::commit_reserved_enqueue_on_cpu_locked(c, &placement.plan)
        }) {
            Ok(Ok(ReservedCommit::Queued(cpu))) => {
                #[cfg(feature = "sched-lock1-witness")]
                crate::kernel::schedlock1_witness::claim(self, cpu, placement.plan.tid);
                crate::arch::hal::notify_enqueued(cpu);
            }
            // Already current or queued there: its existing placement is the one it has.
            Ok(Ok(ReservedCommit::AlreadyPlaced(_))) => {}
            other => crate::yarm_log!(
                "IPC_WAKE_RESERVED_COMMIT_REFUSED tid={} cpu={} result={:?}",
                placement.plan.tid,
                placement.home.0,
                other
            ),
        }
    }

    /// IPC-TIMEOUT-CAP1 — give back a promise whose claim did not commit.
    pub(crate) fn release_ipc_wake_split(&self, placement: IpcWakePlacement) {
        let _ = self.with_cpu_sched_split(placement.home, |c| {
            crate::kernel::task_enqueue::release_enqueue_reservation_on_cpu_locked(
                c,
                &placement.plan,
            )
        });
    }

''' + r[end:]
r = rep(r, """pub(crate) struct OffLockReplyTimeout<'a>(pub(crate) &'a SharedKernel);""",
        """pub(crate) struct OffLockReplyTimeout<'a>(pub(crate) &'a SharedKernel);

/// SCHED-TIME-FOUNDATION1 A (IPC-TIMEOUT-CAP1) — a run-queue slot promised on `home` to an IPC
/// terminal completion's wake, held across its claim. Not `Clone`: one promise, one consumer.
#[derive(Debug)]
#[must_use = "an IPC wake promise must be committed or released"]
pub(crate) struct IpcWakePlacement {
    plan: crate::kernel::task_enqueue::EnqueuePlan,
    home: CpuId,
}""")
r = rep(r, """    fn rtd_enqueue(&mut self, tid: u64) {
        self.0.enqueue_reply_timeout_wake_split(tid);
    }
}""", """    type Placement = IpcWakePlacement;
    fn rtd_reserve(&mut self, tid: u64) -> Option<IpcWakePlacement> {
        let cpu = self.0.executing_cpu_split();
        self.0.reserve_ipc_wake_split(tid, cpu)
    }
    fn rtd_enqueue_reserved(&mut self, placement: IpcWakePlacement) {
        self.0.commit_ipc_wake_split(placement);
    }
    fn rtd_release(&mut self, placement: IpcWakePlacement) {
        self.0.release_ipc_wake_split(placement);
    }
}""")
r = rep(r, """    /// The token fire claim failed (stale/duplicate/cancelled): nothing was mutated.
    StaleToken,
}""", """    /// The token fire claim failed (stale/duplicate/cancelled): nothing was mutated.
    StaleToken,
    /// SCHED-TIME-FOUNDATION1 A (IPC-TIMEOUT-CAP1): the caller's home run queue had no room, so
    /// NOTHING was claimed — the deadline (or the parked death) is still the obligation, and a
    /// later pass retries. A task is never made `Runnable` without its slot.
    PlacementDeferred,
}""")
r = rep(r, """                other => {
                    // BL5a-2b: the reply-wins oracle's registration must never get here.""",
        """                // IPC-TIMEOUT-CAP1: nothing was claimed; the registration stays armed and due, so
                // the collector publishes it again on a later pass.
                ReplyTimeoutOutcome::PlacementDeferred => {
                    crate::yarm_log!(
                        "IPC_REPLY_TIMEOUT_PLACEMENT_DEFERRED arch={} caller_tid={} cpu={} result=ok",
                        crate::kernel::boot::REPLY_TIMEOUT_ARCH,
                        work.handle.identity().terminal_identity.caller_tid.0,
                        cpu_idx
                    );
                }
                other => {
                    // BL5a-2b: the reply-wins oracle's registration must never get here.""")
# send drain
r = rep(r, """            if !current {
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_REFUSED_STALE tid={} asid={} send_generation={} deadline={}",
                    work.tid,
                    work.asid.map(|a| a.0).unwrap_or(0),
                    work.send_generation,
                    work.deadline
                );
                continue;
            }
""", """            if !current {
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_REFUSED_STALE tid={} asid={} send_generation={} deadline={}",
                    work.tid,
                    work.asid.map(|a| a.0).unwrap_or(0),
                    work.send_generation,
                    work.deadline
                );
                continue;
            }
            // ── (0) rank 1: IPC-TIMEOUT-CAP1 — the slot, promised before anything is touched ──
            // No room: nothing is withdrawn or claimed; the deadline stays armed and due, and the
            // collector publishes this cycle again on a later pass.
            let Some(placement) = self.reserve_ipc_wake_split(work.tid, cpu) else {
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_PLACEMENT_DEFERRED tid={} send_generation={} cpu={}",
                    work.tid,
                    work.send_generation,
                    cpu_idx
                );
                continue;
            };
""")
r = rep(r, """            let Some(envelope) = withdrawn else {
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_REFUSED_NO_WAITER tid={} asid={} send_generation={} deadline={}",""",
        """            let Some(envelope) = withdrawn else {
                self.release_ipc_wake_split(placement);
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_REFUSED_NO_WAITER tid={} asid={} send_generation={} deadline={}",""")
r = rep(r, """            if !claimed {
                continue;
            }
            // ── (4) rank 1: the wake, last ─────────────────────────────────────────────────
            self.enqueue_reply_timeout_wake_split(work.tid);""",
        """            if !claimed {
                self.release_ipc_wake_split(placement);
                continue;
            }
            // ── (4) rank 1: the wake, last — the promised slot, which cannot be refused ─────
            self.commit_ipc_wake_split(placement);""")
# recv drain
r = rep(r, """        while let Some(work) = crate::kernel::boot::recv_timeout_work_drain_next(cpu_idx) {
            // ── (1) rank 2: the exact claim + the TimedOut publication ──────────────────────""",
        """        while let Some(work) = crate::kernel::boot::recv_timeout_work_drain_next(cpu_idx) {
            // ── (0) rank 1: IPC-TIMEOUT-CAP1 — the slot, promised before the claim ──────────
            // No room: nothing is claimed; the deadline stays armed and due, and the collector
            // publishes this cycle again on a later pass.
            let Some(placement) = self.reserve_ipc_wake_split(work.tid, cpu) else {
                crate::yarm_log!(
                    "U8_RECV_TIMEOUT_PLACEMENT_DEFERRED tid={} wait_generation={} cpu={}",
                    work.tid,
                    work.wait_generation,
                    cpu_idx
                );
                continue;
            };
            // ── (1) rank 2: the exact claim + the TimedOut publication ──────────────────────""")
r = rep(r, """            if !claimed {
                crate::yarm_log!(
                    "U8_RECV_TIMEOUT_REFUSED_STALE tid={} asid={} wait_generation={} deadline={}",""",
        """            if !claimed {
                self.release_ipc_wake_split(placement);
                crate::yarm_log!(
                    "U8_RECV_TIMEOUT_REFUSED_STALE tid={} asid={} wait_generation={} deadline={}",""")
r = rep(r, """            // ── (3) rank 1: the wake, last ─────────────────────────────────────────────────
            self.enqueue_reply_timeout_wake_split(work.tid);""",
        """            // ── (3) rank 1: the wake, last — the promised slot, which cannot be refused ─────
            self.commit_ipc_wake_split(placement);""")
R.write_text(r)

# ---- ipc_state: the trait + the KernelState impl ----
i = rep(i, """    /// Enqueue a woken caller onto its run queue — the transaction's LAST, non-fallible
    /// step, taken with NO ipc/task lock held.
    fn rtd_enqueue(&mut self, tid: u64);
}""", """    /// SCHED-TIME-FOUNDATION1 A (IPC-TIMEOUT-CAP1) — a run-queue slot promised to the caller.
    type Placement;
    /// Promise the caller a slot on its home run queue BEFORE any claim, with no ipc/task lock
    /// held. `None`: no room — the transaction claims nothing and returns `PlacementDeferred`.
    fn rtd_reserve(&mut self, tid: u64) -> Option<Self::Placement>;
    /// Fill the promise: the transaction's LAST step, taken with NO ipc/task lock held. It cannot
    /// be refused for capacity.
    fn rtd_enqueue_reserved(&mut self, placement: Self::Placement);
    /// Give back a promise whose claim did not commit.
    fn rtd_release(&mut self, placement: Self::Placement);
}""")
i = rep(i, """    fn rtd_enqueue(&mut self, tid: u64) {
        let _ = self.enqueue_task(tid);
    }
}""", """    type Placement = crate::kernel::task_enqueue::EnqueuePlan;
    fn rtd_reserve(&mut self, tid: u64) -> Option<Self::Placement> {
        use crate::kernel::task_enqueue::{self as te, EnqueueTarget};
        let cpu = self.current_cpu();
        let mut plan = self
            .with_task_enqueue_policy_mut(|tcbs, classes| {
                te::plan_balanced_enqueue_locked(tcbs, classes, tid, cpu)
            })
            .ok()?;
        self.fix_plan_home(&mut plan, cpu);
        let EnqueueTarget::Pinned(home) = plan.target else {
            return None;
        };
        let mut guard = self.scheduler_state();
        let mut all = guard.view();
        let mut view = all.cpu(home).ok()?;
        te::reserve_enqueue_on_cpu_locked(&mut view, &plan).ok()?;
        Some(plan)
    }
    fn rtd_enqueue_reserved(&mut self, plan: Self::Placement) {
        use crate::kernel::task_enqueue::{self as te, EnqueueTarget};
        let EnqueueTarget::Pinned(home) = plan.target else {
            return;
        };
        let mut guard = self.scheduler_state();
        let mut all = guard.view();
        if let Ok(mut view) = all.cpu(home)
            && let Err(err) = te::commit_reserved_enqueue_on_cpu_locked(&mut view, &plan)
        {
            crate::yarm_log!(
                "IPC_WAKE_RESERVED_COMMIT_REFUSED tid={} cpu={} err={:?}",
                plan.tid,
                home.0,
                err
            );
        }
    }
    fn rtd_release(&mut self, plan: Self::Placement) {
        use crate::kernel::task_enqueue::{self as te, EnqueueTarget};
        let EnqueueTarget::Pinned(home) = plan.target else {
            return;
        };
        let mut guard = self.scheduler_state();
        let mut all = guard.view();
        if let Ok(mut view) = all.cpu(home) {
            te::release_enqueue_reservation_on_cpu_locked(&mut view, &plan);
        }
    }
}""")
I.write_text(i)
print('ok')
