// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D-WA3B — the one-shot spawn reservation/consumption protocol.
//!
//! # Why
//!
//! `spawn_user_task_from_image` was the last Group-3 `CAN` site in the waiter-owner census
//! (`doc/KERNEL_UNLOCK_AUDIT.md` §6.1.35 C). It relied on `register_task_with_class` being
//! **idempotent**: handed a TID that already had a live TCB, registration returned `Ok(())` and
//! the spawn then overwrote that task's register context, entry point, stack, ASID, thread group
//! and scheduler membership. A `Blocked(EndpointReceive)` receiver was as overwritable as an idle
//! one.
//!
//! The obvious repair — "the destination TID must be absent" — was implemented in WA3A and
//! reverted, because it is genuinely incompatible with bootstrap: all three architectures
//! register the supervisor / PM / init TIDs BEFORE spawning onto them, so the capability grants
//! have a destination CNode and the task has its kernel-side provisioning. The absence gate
//! refused the kernel's own supervisor on an ordinary boot.
//!
//! This module resolves that properly. Bootstrap still creates the task's resources first, but it
//! creates them as an explicit **reservation** rather than as something that merely looks like a
//! live task, and spawn consumes that exact reservation:
//!
//! ```text
//!   ReservedUnstarted --claim--> Spawning --commit--> LiveSpawned (Runnable)
//!                          ^                 |
//!                          +-----restore-----+   (spawn failed; same incarnation)
//! ```
//!
//! # Exact identity
//!
//! A [`SpawnReservationToken`] names ONE reservation: its TID, its generation, its class and its
//! owning process. The generation comes from a monotonic kernel counter and never from the TID,
//! so a token minted for an earlier occupant of numeric TID `T` can never authorize a later
//! occupant of `T`. Fields are private and there is no public constructor: the only way to obtain
//! a token is to have created the reservation.
//!
//! # This module takes no lock
//!
//! Like [`crate::kernel::task_transition`], every entry point operates on a
//! `&mut [Option<ThreadControlBlock>]` slice the caller already holds under the task rank-2
//! acquisition. Nothing here acquires, nests or reorders a domain lock, so no path gains a broad
//! lock acquisition and no task(2) → scheduler(1) inversion is introduced.

use crate::kernel::scheduler::CpuId;
use crate::kernel::task::{
    SpawnPhase, SpawnReservation, TaskClass, TaskStatus, ThreadControlBlock, ThreadGroupId,
    UserRegisterContext,
};
use crate::kernel::vm::{Asid, VirtAddr};

/// Stage 199D-WA3B — the EXACT pre-spawn value of every TCB field the spawn body may write.
///
/// Captured by [`claim_for_spawn`] before the phase flip, and replayed by
/// [`restore_after_failed_spawn`]. `spawn_user_task_from_image` binds the incoming ASID before
/// the fallible stack allocation and user-memory copy, because the x86_64 kernel switch-frame
/// retry and the stack allocator both need the target address space bound — so "all fallible
/// work completes before any TCB mutation" is NOT achievable without reordering the boot
/// provisioning this stage exists to preserve. This baseline is the other permitted proof: after
/// a failed spawn the reservation is observationally identical to the pre-claim reservation, with
/// no stale ASID, entry, stack top, register context, TLS pointer, affinity or thread-group
/// identity left behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpawnBaseline {
    status: TaskStatus,
    thread_group_id: ThreadGroupId,
    asid: Option<Asid>,
    user_entry: Option<VirtAddr>,
    user_stack_top: Option<VirtAddr>,
    user_context: UserRegisterContext,
    tls_ptr: Option<VirtAddr>,
    cpu_affinity: Option<CpuId>,
}

impl SpawnBaseline {
    fn capture(tcb: &ThreadControlBlock) -> Self {
        Self {
            status: tcb.status,
            thread_group_id: tcb.thread_group_id,
            asid: tcb.asid,
            user_entry: tcb.user_entry,
            user_stack_top: tcb.user_stack_top,
            user_context: tcb.user_context,
            tls_ptr: tcb.tls_ptr,
            cpu_affinity: tcb.cpu_affinity,
        }
    }

    fn replay(self, tcb: &mut ThreadControlBlock) {
        tcb.status = self.status;
        tcb.thread_group_id = self.thread_group_id;
        tcb.asid = self.asid;
        tcb.user_entry = self.user_entry;
        tcb.user_stack_top = self.user_stack_top;
        tcb.user_context = self.user_context;
        tcb.tls_ptr = self.tls_ptr;
        tcb.cpu_affinity = self.cpu_affinity;
    }
}

/// Proof that one specific spawn reservation exists, and authority to consume it exactly once.
///
/// Opaque by construction: the fields are private and the only constructor is
/// [`mint_reservation`], which is called from the reservation API after the reservation has
/// actually been recorded. A token is `Copy` so a caller can keep one across the spawn call, but
/// copying it grants nothing — the *reservation* is one-shot, not the token, and a second
/// consume finds the reservation already claimed or already gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnReservationToken {
    tid: u64,
    generation: u64,
    class: TaskClass,
    process_pid: u64,
}

impl SpawnReservationToken {
    /// The reserved TID. Exposed because callers legitimately need to name the task they are
    /// about to spawn; it is NOT sufficient authority on its own.
    pub fn tid(&self) -> u64 {
        self.tid
    }

    /// The class this reservation was provisioned for.
    pub fn class(&self) -> TaskClass {
        self.class
    }

    /// The process this reservation belongs to.
    pub fn process_pid(&self) -> u64 {
        self.process_pid
    }

    /// The monotonic generation that pins this reservation to one incarnation of the TID.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Mint a token for a reservation that has just been recorded.
///
/// `pub(crate)` and deliberately not a `SpawnReservationToken::new`: the reservation API is the
/// only caller, so a token cannot be conjured for a reservation that does not exist.
pub(crate) fn mint_reservation(
    tid: u64,
    generation: u64,
    class: TaskClass,
    process_pid: u64,
) -> SpawnReservationToken {
    SpawnReservationToken {
        tid,
        generation,
        class,
        process_pid,
    }
}

/// Why a reservation operation was refused. Every variant is fail-closed: **no field of the TCB
/// is written**, so a refusal is observationally identical to never having called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationRefusal {
    /// No TCB holds this TID at all.
    TaskMissing,
    /// The TID exists but is a LIVE task, not a reservation. This is the case that used to be
    /// silently overwritten.
    NotAReservation { observed: TaskStatus },
    /// The TID is a reservation, but a different incarnation than the token names.
    GenerationMismatch { observed: u64 },
    /// The reservation exists but was provisioned for a different class.
    ClassMismatch { observed: TaskClass },
    /// The reservation exists but belongs to a different process.
    ProcessMismatch { observed: u64 },
    /// The reservation is already claimed by an in-flight spawn (or, for a restore, is not
    /// claimed at all).
    WrongPhase { observed: SpawnPhase },
    /// The TCB is marked `Reserved` but carries no reservation record. A kernel invariant break
    /// rather than a caller error; refused rather than repaired silently.
    MissingReservationRecord,
}

/// Resolve `token` against `tcbs`, or refuse. Reads only — nothing is mutated.
///
/// The checks run in the order the audit argues for: identity before state. A stale numeric TID
/// must not even be told what phase the replacement reservation is in, let alone move it.
fn resolve<'a>(
    tcbs: &'a mut [Option<ThreadControlBlock>],
    token: &SpawnReservationToken,
) -> Result<(&'a mut ThreadControlBlock, SpawnReservation), ReservationRefusal> {
    let Some(tcb) = tcbs.iter_mut().flatten().find(|t| t.tid.0 == token.tid) else {
        return Err(ReservationRefusal::TaskMissing);
    };
    if !matches!(tcb.status, TaskStatus::Reserved) {
        return Err(ReservationRefusal::NotAReservation {
            observed: tcb.status,
        });
    }
    let Some(reservation) = tcb.spawn_reservation else {
        return Err(ReservationRefusal::MissingReservationRecord);
    };
    if reservation.generation != token.generation {
        return Err(ReservationRefusal::GenerationMismatch {
            observed: reservation.generation,
        });
    }
    if reservation.class != token.class {
        return Err(ReservationRefusal::ClassMismatch {
            observed: reservation.class,
        });
    }
    if reservation.process_pid != token.process_pid {
        return Err(ReservationRefusal::ProcessMismatch {
            observed: reservation.process_pid,
        });
    }
    Ok((tcb, reservation))
}

/// `ReservedUnstarted -> Spawning`: claim the exact reservation `token` names.
///
/// This is the ONLY gate into spawn-specific work. It validates TID, generation, class, process
/// and phase atomically under the caller's single rank-2 acquisition, and mutates nothing on any
/// refusal — so a stale token, a duplicate token, a wrong class, a wrong process, or a TID whose
/// TCB is live, blocked, faulted, exited or dead all fail closed BEFORE any spawn side effect.
pub(crate) fn claim_for_spawn(
    tcbs: &mut [Option<ThreadControlBlock>],
    token: &SpawnReservationToken,
) -> Result<SpawnBaseline, ReservationRefusal> {
    let (tcb, reservation) = resolve(tcbs, token)?;
    if reservation.phase != SpawnPhase::ReservedUnstarted {
        return Err(ReservationRefusal::WrongPhase {
            observed: reservation.phase,
        });
    }
    let baseline = SpawnBaseline::capture(tcb);
    tcb.spawn_reservation = Some(SpawnReservation {
        phase: SpawnPhase::Spawning,
        ..reservation
    });
    Ok(baseline)
}

/// `Spawning -> ReservedUnstarted`: restore the SAME reservation incarnation after a failed spawn.
///
/// Transactional in the narrow sense this stage owns: the reservation lifecycle itself never
/// stays `Spawning` after an ordinary returned error, and the restored reservation keeps its
/// generation, so the caller's token is still exactly as valid as it was before the attempt.
pub(crate) fn restore_after_failed_spawn(
    tcbs: &mut [Option<ThreadControlBlock>],
    token: &SpawnReservationToken,
    baseline: SpawnBaseline,
) -> Result<(), ReservationRefusal> {
    let (tcb, reservation) = resolve(tcbs, token)?;
    if reservation.phase != SpawnPhase::Spawning {
        return Err(ReservationRefusal::WrongPhase {
            observed: reservation.phase,
        });
    }
    // Every field the spawn body may have written goes back to its exact pre-claim value —
    // not just the phase. A restored reservation carries no partial live identity.
    baseline.replay(tcb);
    tcb.spawn_reservation = Some(SpawnReservation {
        phase: SpawnPhase::ReservedUnstarted,
        ..reservation
    });
    Ok(())
}

/// Cancel an unstarted reservation, making it non-existent and non-authoritative.
///
/// For the case where pre-spawn setup fails BEFORE spawn is invoked. Validates exact TID,
/// generation, class, process and `ReservedUnstarted` phase, so a stale token, a token for a
/// replacement occupant of the same numeric TID, a `Spawning` reservation and a live task are all
/// refused with zero mutation. Returns the process this reservation belonged to, so the caller
/// can run the existing process/CNode cleanup once no other live or reserved thread owns it.
pub(crate) fn validate_cancellable(
    tcbs: &mut [Option<ThreadControlBlock>],
    token: &SpawnReservationToken,
) -> Result<(usize, u64), ReservationRefusal> {
    {
        let (_, reservation) = resolve(tcbs, token)?;
        if reservation.phase != SpawnPhase::ReservedUnstarted {
            return Err(ReservationRefusal::WrongPhase {
                observed: reservation.phase,
            });
        }
    }
    let index = tcbs
        .iter()
        .position(|slot| slot.as_ref().is_some_and(|t| t.tid.0 == token.tid))
        .ok_or(ReservationRefusal::TaskMissing)?;
    Ok((index, token.process_pid))
}

/// Remove a validated reservation's TCB slot. Separate from [`validate_cancellable`] so the
/// caller can run the existing `release_kernel_context` cleanup — which resolves the task by TID
/// and therefore needs the slot still present — in between.
pub(crate) fn clear_reservation_slot(tcbs: &mut [Option<ThreadControlBlock>], index: usize) {
    tcbs[index] = None;
}

/// `Spawning -> LiveSpawned`: the exact one-shot commit.
///
/// Clears the reservation record as it goes, so the live task carries no residual reservation
/// authority and the token that produced it can never be consumed again. This is the single
/// point at which a reserved TCB becomes `Runnable`, and therefore the single point at which it
/// becomes eligible to be enqueued — the caller must not enqueue before calling this.
pub(crate) fn commit_live_spawn(
    tcbs: &mut [Option<ThreadControlBlock>],
    token: &SpawnReservationToken,
) -> Result<(), ReservationRefusal> {
    let (tcb, reservation) = resolve(tcbs, token)?;
    if reservation.phase != SpawnPhase::Spawning {
        return Err(ReservationRefusal::WrongPhase {
            observed: reservation.phase,
        });
    }
    tcb.spawn_reservation = None;
    tcb.status = TaskStatus::Runnable;
    Ok(())
}

/// Emit the canonical refusal marker. One place, so every refusal is greppable and every site
/// reports the same fields.
pub(crate) fn log_reservation_refusal(site: &str, tid: u64, refusal: ReservationRefusal) {
    match refusal {
        ReservationRefusal::TaskMissing => crate::yarm_log!(
            "SPAWN_RESERVATION_REFUSED site={} tid={} reason=task_missing",
            site,
            tid
        ),
        ReservationRefusal::NotAReservation { observed } => crate::yarm_log!(
            "SPAWN_RESERVATION_REFUSED site={} tid={} reason=not_a_reservation observed={:?}",
            site,
            tid,
            observed
        ),
        ReservationRefusal::GenerationMismatch { observed } => crate::yarm_log!(
            "SPAWN_RESERVATION_REFUSED site={} tid={} reason=generation_mismatch observed={}",
            site,
            tid,
            observed
        ),
        ReservationRefusal::ClassMismatch { observed } => crate::yarm_log!(
            "SPAWN_RESERVATION_REFUSED site={} tid={} reason=class_mismatch observed={:?}",
            site,
            tid,
            observed
        ),
        ReservationRefusal::ProcessMismatch { observed } => crate::yarm_log!(
            "SPAWN_RESERVATION_REFUSED site={} tid={} reason=process_mismatch observed={}",
            site,
            tid,
            observed
        ),
        ReservationRefusal::WrongPhase { observed } => crate::yarm_log!(
            "SPAWN_RESERVATION_REFUSED site={} tid={} reason=wrong_phase observed={:?}",
            site,
            tid,
            observed
        ),
        ReservationRefusal::MissingReservationRecord => crate::yarm_log!(
            "SPAWN_RESERVATION_REFUSED site={} tid={} reason=missing_reservation_record",
            site,
            tid
        ),
    }
}
