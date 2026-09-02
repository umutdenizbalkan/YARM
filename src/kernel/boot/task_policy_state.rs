// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState, RuntimeCapacityConfig, TidAllocationTelemetry};
use crate::kernel::capabilities::CNodeId;
use crate::kernel::ipc::ThreadId;
use crate::kernel::task::{TaskClass, ThreadControlBlock};

impl KernelState {
    pub(crate) fn register_task_with_class_and_cnode_slots_in_process(
        &mut self,
        tid: u64,
        class: TaskClass,
        process_pid: u64,
        requested_cnode_slots: Option<usize>,
    ) -> Result<(), KernelError> {
        if self.task_status(tid).is_some() {
            return Ok(());
        }
        let limits = self.runtime_capacity_config();
        if self.with_tcbs(|tcbs| tcbs.iter().flatten().count()) >= limits.max_tasks {
            return Err(KernelError::TaskTableFull);
        }
        let cnode = self
            .process_cnode_for_pid(process_pid)
            .unwrap_or(CNodeId(process_pid));
        let cnode_slots =
            Self::requested_cnode_slot_capacity_for_class(class, limits, requested_cnode_slots)?;
        self.ensure_cnode_space_with_slots(cnode, cnode_slots)?;
        self.set_process_cnode_for_pid(process_pid, cnode)?;
        // U9-SPAWN1 SP-2: the task-domain half — free slot, TCB, class entry and the fixed
        // per-slot kernel-stack range — is now ONE owner shared with the pre-lock route, taken
        // in ONE task-lock acquisition. Previously the slot claim, the class write and
        // `provision_default_kernel_context` were three separate acquisitions with the slot
        // observable-but-classless in between; the owner closes that window as a side effect of
        // having one home. The rank-4 CNode half above stays here, because a thread joining a
        // live process needs it to be a no-op while a fresh process genuinely needs it done.
        let _ = self.with_task_enqueue_policy_mut(|tcbs, classes| {
            super::spawn_thread_core::register_thread_incarnation_locked(
                tcbs,
                classes,
                tid,
                class,
                limits.max_tasks,
            )
        })?;
        Ok(())
    }

    pub(crate) fn register_task_with_class_in_process(
        &mut self,
        tid: u64,
        class: TaskClass,
        process_pid: u64,
    ) -> Result<(), KernelError> {
        self.register_task_with_class_and_cnode_slots_in_process(tid, class, process_pid, None)
    }

    pub fn register_task_with_class(
        &mut self,
        tid: u64,
        class: TaskClass,
    ) -> Result<(), KernelError> {
        self.register_task_with_class_in_process(tid, class, tid)
    }

    pub fn register_task(&mut self, tid: u64) -> Result<(), KernelError> {
        self.register_task_with_class(tid, TaskClass::App)
    }

    /// Stage 199D-WA3B: reserve `tid` for a future spawn, in `process_pid`.
    ///
    /// This is the counterpart to `register_task_with_class`, NOT a variant of it. Registration
    /// creates an ordinary already-live task, which is the right thing when that is genuinely
    /// what the caller means; this creates a **non-live reservation** that only
    /// `spawn_user_task_from_image` can turn into a live task, and only once.
    ///
    /// It reuses the same capacity, CNode-slot, class and kernel-context provisioning as ordinary
    /// registration — that policy is not duplicated here — so a reservation owns exactly the
    /// pre-spawn resources bootstrap needs: CNode/process association, class, and the kernel
    /// stack/kernel context. What it does NOT own is liveness: the TCB is `TaskStatus::Reserved`,
    /// so it cannot be dispatched, enqueued, woken, blocked, joined, or published as an endpoint
    /// waiter.
    ///
    /// Refuses without mutation if `tid` is already anything at all — reserved, spawning or live.
    /// Registration's idempotence is deliberately NOT inherited: it is precisely what allowed a
    /// spawn to overwrite an existing task.
    pub fn reserve_task_for_spawn_with_class_in_process(
        &mut self,
        tid: u64,
        class: TaskClass,
        process_pid: u64,
    ) -> Result<crate::kernel::spawn_reservation::SpawnReservationToken, KernelError> {
        if let Some(observed) = self.task_status(tid) {
            crate::yarm_log!(
                "SPAWN_RESERVE_REFUSED tid={} reason=tid_occupied observed={:?}",
                tid,
                observed
            );
            return Err(KernelError::TaskTableFull);
        }
        let limits = self.runtime_capacity_config();
        if self.with_tcbs(|tcbs| tcbs.iter().flatten().count()) >= limits.max_tasks {
            return Err(KernelError::TaskTableFull);
        }
        // U9-SPAWN2 §2: the CNode space and its PID association are ONE transaction, taken
        // through the shared owner. They used to be two independent rank-4 entries, and a
        // failure of the second left the first behind: a capability space provisioned for a
        // process that did not exist, which nothing ever removed.
        //
        // The PID is passed explicitly and comes from this reservation's own argument — there is
        // no ambient current-task fallback here, and the split route states it the same way.
        let cnode_request = crate::kernel::boot::process_cnode_txn::ProcessCNodeRequest {
            pid: process_pid,
            tid,
            class,
        };
        let cnode_grant = self.provision_process_cnode(&cnode_request)?;
        let cnode = cnode_grant.cnode;
        // Monotonic, never derived from the TID: a token for an earlier occupant of this numeric
        // TID cannot match this reservation.
        let generation = self.spawn_reservation_generation;
        self.spawn_reservation_generation = self.spawn_reservation_generation.saturating_add(1);
        let reservation = crate::kernel::task::SpawnReservation {
            generation,
            class,
            process_pid,
            phase: crate::kernel::task::SpawnPhase::ReservedUnstarted,
        };
        let inserted_idx = self.with_tcbs_mut(|tcbs| {
            if let Some(idx) = tcbs.iter().position(|slot| slot.is_none()) {
                tcbs[idx] = Some(ThreadControlBlock::reserved(ThreadId(tid), reservation));
                Some(idx)
            } else {
                None
            }
        });
        let Some(inserted_idx) = inserted_idx else {
            // U9-SPAWN2 §2: the CNode transaction happened; this reservation did not. Release
            // exactly what it created, through the owner that holds it.
            self.release_process_cnode_grant(&cnode_request, &cnode_grant);
            return Err(KernelError::TaskTableFull);
        };
        super::kernel_mut(&mut self.task_classes)[inserted_idx] = Some(class);
        if let Err(err) = self.provision_default_kernel_context(tid) {
            self.with_tcbs_mut(|tcbs| {
                crate::kernel::spawn_reservation::clear_reservation_slot(tcbs, inserted_idx)
            });
            super::kernel_mut(&mut self.task_classes)[inserted_idx] = None;
            self.release_process_cnode_grant(&cnode_request, &cnode_grant);
            return Err(err);
        }
        crate::yarm_log!(
            "SPAWN_RESERVE_OK tid={} class={:?} pid={} generation={}",
            tid,
            class,
            process_pid,
            generation
        );
        Ok(crate::kernel::spawn_reservation::mint_reservation(
            tid,
            generation,
            class,
            process_pid,
        ))
    }

    /// Stage 199D-WA3B: reserve `tid` for a future spawn in its own process.
    pub fn reserve_task_for_spawn_with_class(
        &mut self,
        tid: u64,
        class: TaskClass,
    ) -> Result<crate::kernel::spawn_reservation::SpawnReservationToken, KernelError> {
        self.reserve_task_for_spawn_with_class_in_process(tid, class, tid)
    }

    /// Stage 199D-WA3B: cancel an unstarted reservation, for when pre-spawn setup fails BEFORE
    /// `spawn_user_task_from_image` is invoked.
    ///
    /// Validates the exact TID, generation, class, process and `ReservedUnstarted` phase before
    /// touching anything, so a stale token, a token naming a replacement occupant of the same
    /// numeric TID, a reservation already claimed by an in-flight spawn, and a live task are all
    /// refused with **zero mutation**. On success the reserved TCB is removed and its
    /// reservation-owned kernel resources are released through the EXISTING cleanup mechanisms
    /// (`release_kernel_context`, then the no-alloc process-CNode reap, which itself only
    /// proceeds when no other thread still owns the process) — no second cleanup policy is
    /// introduced. After cancellation the token names nothing and can authorize nothing.
    pub fn cancel_spawn_reservation(
        &mut self,
        token: crate::kernel::spawn_reservation::SpawnReservationToken,
    ) -> Result<(), KernelError> {
        let tid = token.tid();
        // Validate first. This is read-only, so every refusal leaves the reservation — or the
        // replacement task the stale token failed to name — byte-for-byte unchanged.
        let (index, process_pid) = self.with_tcbs_mut(|tcbs| {
            crate::kernel::spawn_reservation::validate_cancellable(tcbs, &token).map_err(
                |refusal| {
                    crate::kernel::spawn_reservation::log_reservation_refusal(
                        "cancel_spawn_reservation",
                        tid,
                        refusal,
                    );
                    KernelError::WrongObject
                },
            )
        })?;
        // Release the kernel context BEFORE the slot goes away: the existing primitive resolves
        // the task by TID and would find nothing afterwards.
        let _ = self.release_kernel_context(tid);
        self.with_tcbs_mut(|tcbs| {
            crate::kernel::spawn_reservation::clear_reservation_slot(tcbs, index)
        });
        super::kernel_mut(&mut self.task_classes)[index] = None;
        let cnode_reaped = self.maybe_cleanup_process_cnode_for_pid_noalloc_reap(process_pid);
        crate::yarm_log!(
            "SPAWN_RESERVATION_CANCELLED tid={} generation={} pid={} cnode_reaped={}",
            tid,
            token.generation(),
            process_pid,
            u8::from(cnode_reaped)
        );
        Ok(())
    }

    /// Test/hosted-only observation helpers for the Stage 199D-WA3B reservation suite.
    ///
    /// These only READ or drive existing production seams; none of them is a second lifecycle
    /// authority.
    #[cfg(test)]
    pub(crate) fn wake_task_for_test(&mut self, tid: u64) -> Result<(), KernelError> {
        self.wake_tid_to_runnable_for_test(ThreadId(tid))
    }

    /// A copy of the whole TCB, so a test can assert "observationally identical".
    #[cfg(any(test, feature = "hosted-dev"))]
    pub(crate) fn thread_control_block_snapshot_for_test(
        &self,
        tid: u64,
    ) -> Option<ThreadControlBlock> {
        self.with_tcbs(|tcbs| tcbs.iter().flatten().find(|t| t.tid.0 == tid).cloned())
    }

    /// Test/hosted-only: drive a task's blocked-receive generation to an exact value, so the
    /// checked-exhaustion path can be reached without a billion real blocking cycles.
    #[cfg(any(test, feature = "hosted-dev"))]
    pub(crate) fn set_blocked_recv_generation_for_test(&mut self, tid: u64, generation: u64) {
        self.with_tcbs_mut(|tcbs| {
            if let Some(t) = tcbs.iter_mut().flatten().find(|t| t.tid.0 == tid) {
                t.blocked_recv_generation = generation;
            }
        });
    }

    /// U9-SPAWN1 SP-2: the allocation rule moved to
    /// [`super::spawn_thread_core::allocate_dynamic_tid_locked`], so the broad path and the
    /// pre-lock route allocate from the SAME cursor under the SAME lock. The telemetry the
    /// allocator owes is returned rather than written, and applied here at rank 10 — outside the
    /// task lock, which is what keeps 2 → 10 from ever being held nested.
    pub fn allocate_thread_id(&mut self) -> Result<u64, KernelError> {
        let max_tasks = self.runtime_capacity_config().max_tasks;
        let policy = self.tid_allocation_policy;
        let (tid, delta) = self.with_task_tid_alloc_mut(|tcbs, cursor| {
            super::spawn_thread_core::allocate_dynamic_tid_locked(tcbs, cursor, policy, max_tasks)
        })?;
        self.apply_tid_allocation_delta(delta);
        Ok(tid)
    }

    /// Apply one allocation's telemetry, at rank 10, with the task lock released.
    pub(crate) fn apply_tid_allocation_delta(
        &mut self,
        delta: super::spawn_thread_core::TidAllocationDelta,
    ) {
        if delta == super::spawn_thread_core::TidAllocationDelta::default() {
            return;
        }
        self.with_telemetry_state_mut(|telemetry| {
            let t = &mut telemetry.tid_allocation;
            t.gap_floor_repairs = t.gap_floor_repairs.saturating_add(delta.gap_floor_repairs);
            t.dynamic_tid_allocations = t
                .dynamic_tid_allocations
                .saturating_add(delta.dynamic_tid_allocations);
            t.dynamic_tid_wraps = t.dynamic_tid_wraps.saturating_add(delta.dynamic_tid_wraps);
        });
    }

    #[cfg(test)]
    pub(crate) fn set_dynamic_tid_cursor_for_test(&mut self, next_dynamic_tid: u64) {
        self.tid_allocation_cursor
            .set_next_dynamic_tid_for_test(next_dynamic_tid);
    }

    #[cfg(test)]
    pub(crate) fn next_dynamic_tid_for_test(&self) -> u64 {
        self.tid_allocation_cursor
            .next_dynamic_tid(self.tid_allocation_policy)
    }

    pub fn task_class(&self, tid: u64) -> Option<TaskClass> {
        self.with_tcbs(|tcbs| {
            tcbs.iter().enumerate().find_map(|(idx, slot)| {
                slot.as_ref()
                    .filter(|tcb| tcb.tid.0 == tid)
                    .and(self.task_classes[idx])
            })
        })
    }

    pub fn tid_allocation_telemetry(&self) -> TidAllocationTelemetry {
        self.with_telemetry_state(|telemetry| telemetry.tid_allocation)
    }

    pub fn dynamic_tid_floor(&self) -> u64 {
        self.tid_allocation_policy.dynamic_tid_floor()
    }

    pub fn static_tid_upper_bound(&self) -> u64 {
        self.tid_allocation_policy.static_tid_upper_bound()
    }

    pub fn is_dynamic_tid(&self, tid: u64) -> bool {
        tid >= self.dynamic_tid_floor()
    }

    fn default_cnode_slot_capacity_for_class(
        class: TaskClass,
        limits: RuntimeCapacityConfig,
    ) -> usize {
        match class {
            TaskClass::Driver => limits.driver_cnode_slot_capacity,
            TaskClass::App | TaskClass::SystemServer => limits.default_cnode_slot_capacity,
        }
    }

    pub(crate) fn requested_cnode_slot_capacity_for_class(
        class: TaskClass,
        limits: RuntimeCapacityConfig,
        requested: Option<usize>,
    ) -> Result<usize, KernelError> {
        let default = Self::default_cnode_slot_capacity_for_class(class, limits);
        let requested = requested.unwrap_or(default);
        if requested == 0 {
            return Err(KernelError::WrongObject);
        }
        match class {
            TaskClass::App => {
                if requested != default {
                    return Err(KernelError::MissingRight);
                }
            }
            TaskClass::SystemServer | TaskClass::Driver => {}
        }
        Ok(requested)
    }
}
