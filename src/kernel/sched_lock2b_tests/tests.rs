// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-LOCK2B — exact-incarnation placement reads on the HOME alone.
//!
//! Every converted reader takes rank 2 for the incarnation's home and stamp, releases it, then
//! that one CPU's 1L with rank 2 nested and the identity revalidated (H-HOME, contract §20.5).
//! These tests pin, for each reader, that:
//!
//! * a stamp captured at the original authority boundary refuses a TID reused since — inside the
//!   SAME address space, which `{tid, asid}` cannot tell apart;
//! * the distinct placement outcomes of the retired coherent read survive (current here, current
//!   on another CPU, queued, current-and-queued, absent);
//! * an exited entry that is still queued is found on its home (exit versus dispatch), and the
//!   trapping CPU's own current slot is still consulted when it is not the exiting task's home;
//! * TID 0, kernel-thread ids and a homeless incarnation racing its first placement settle
//!   through the caller's own stale / absent outcome.
//!
//! Fixtures plant raw entries only where a test needs a state H-HOME forbids (the
//! contradiction cases); every other placement goes through the admitted owners.

use std::vec::Vec;

use crate::kernel::boot::Bootstrap;
use crate::kernel::ipc::ThreadId;
use crate::kernel::recv_waiter_split::{RecvEnteringIncarnation, RecvUnwindOutcome};
use crate::kernel::scheduler::{CpuId, TaskPlacement, TaskPriority};
use crate::kernel::syscall::reap_txn::{ReapOwners, SharedReapOwners};
use crate::kernel::task::{TaskStatus, ThreadControlBlock};
use crate::kernel::task_transition::{EnteringFrameAuthority as A, FrameForfeitReason as R};
use crate::kernel::vm::Asid;
use crate::runtime::{HomeMembership as M, HomePlacement, ReceiverEnqueue, SharedKernel};

const ASID: Asid = Asid(41);
const CPU0: CpuId = CpuId(0);
const CPU1: CpuId = CpuId(1);

fn kernel2() -> SharedKernel {
    let k = SharedKernel::new(Bootstrap::init().expect("init"));
    k.with(|s| s.bring_up_cpu(CPU1).expect("cpu1 online"));
    assert_eq!(k.current_tid_split_read(CPU1), None, "CPU 1 starts empty");
    assert_eq!(k.runnable_count_on_cpu_split_read(CPU1), 0);
    k
}

/// Install a FRESH incarnation of `tid` in the address space `ASID`: the TID's existing slot is
/// replaced (exit, reap and same-ASID TID reuse in one step) or a free slot is used. Returns the
/// new stamp.
fn incarnate(k: &SharedKernel, tid: u64, home: Option<CpuId>, status: TaskStatus) -> u64 {
    k.with_task_tcbs_split_mut(|tcbs| {
        let t = ThreadControlBlock {
            cpu_affinity: home,
            status,
            ..ThreadControlBlock::new(ThreadId(tid), Some(ASID))
        };
        let stamp = t.incarnation;
        let idx = tcbs
            .iter()
            .position(|s| s.as_ref().is_some_and(|x| x.tid.0 == tid))
            .or_else(|| tcbs.iter().position(|s| s.is_none()))
            .expect("a TCB slot");
        tcbs[idx] = Some(t);
        stamp
    })
}

fn home(k: &SharedKernel, tid: u64) -> Option<CpuId> {
    k.with(|s| s.task_home_cpu(tid))
}

/// Raw queue entry on `cpu` (a fixture planting; production places through the owners).
fn plant_queued(k: &SharedKernel, cpu: CpuId, tid: u64) {
    k.with_cpu_sched_split(cpu, |c| {
        c.enqueue_with_priority(ThreadId(tid), TaskPriority::Normal)
    })
    .expect("cpu")
    .expect("planted");
}

/// Make `tid` the current task of an otherwise empty `cpu`.
fn plant_current(k: &SharedKernel, cpu: CpuId, tid: u64) {
    plant_queued(k, cpu, tid);
    assert_eq!(
        k.with_cpu_sched_split(cpu, |c| c.dispatch_next())
            .expect("cpu"),
        Some(ThreadId(tid))
    );
}

fn clear_current(k: &SharedKernel, cpu: CpuId, tid: u64) {
    assert!(
        k.with_cpu_sched_split(cpu, |c| c.block_current_exact(ThreadId(tid)))
            .expect("cpu")
            .is_some()
    );
}

fn placed(k: &SharedKernel, cpu: CpuId, tid: u64) -> bool {
    k.with_cpu_sched_split(cpu, |c| c.contains_or_current(ThreadId(tid)))
        .unwrap_or(false)
}

fn entering(tid: u64, stamp: u64) -> RecvEnteringIncarnation {
    RecvEnteringIncarnation {
        tid,
        asid: ASID,
        priority: TaskPriority::Normal,
        stamp,
    }
}

// ── same-ASID TID reuse ──────────────────────────────────────────────────────────────────────

/// A TID reused inside the SAME address space is a different incarnation. Every converted reader
/// and both stamp-bound placement owners refuse the earlier stamp, write nothing, and bind no home
/// for the replacement; the replacement's own stamp is served normally.
#[test]
fn sl2b_same_asid_tid_reuse_is_stale_for_every_reader_and_owner() {
    let k = kernel2();
    let tid = 95_001;
    let s1 = incarnate(&k, tid, Some(CPU1), TaskStatus::Runnable);
    plant_queued(&k, CPU1, tid);
    assert_eq!(
        k.incarnation_home_membership_split_read(tid, s1),
        M::Present
    );
    assert_eq!(
        k.incarnation_home_placement_split(tid, s1),
        HomePlacement::On(TaskPlacement::Queued(CPU1))
    );
    // It runs, blocks, exits, is reaped; the TID comes back in the same address space, homeless.
    plant_current_from_queue(&k, CPU1, tid);
    clear_current(&k, CPU1, tid);
    let s2 = incarnate(&k, tid, None, TaskStatus::Runnable);
    assert_ne!(s1, s2);

    // Readers: the old stamp is Stale, never "absent"; the new one is homeless, hence absent.
    assert_eq!(k.incarnation_home_membership_split_read(tid, s1), M::Stale);
    assert_eq!(
        k.incarnation_home_placement_split(tid, s1),
        HomePlacement::Stale
    );
    assert_eq!(k.incarnation_home_membership_split_read(tid, s2), M::Absent);
    assert_eq!(
        k.exact_status_and_home_placement_split(CPU1, tid, ASID, s1),
        None
    );
    assert_eq!(
        k.yield_forfeited_placement_split_read(CPU1, entering(tid, s1)),
        RecvUnwindOutcome::IncarnationMoved
    );

    // Owners: the exact restore and the direct-IPC committed enqueue refuse the old stamp.
    assert!(!k.restore_current_exact_split(CPU1, tid, s1, TaskPriority::Normal));
    assert_eq!(k.current_tid_split_read(CPU1), None, "nothing restored");
    assert!(matches!(
        k.sr_enqueue_committed_receiver_reconciled_split(tid, s1, Some(CPU1)),
        ReceiverEnqueue::Rejected {
            error: crate::kernel::scheduler::SchedulerError::CpuOffline,
            reconciled: None,
            ..
        }
    ));
    assert!(
        !placed(&k, CPU0, tid) && !placed(&k, CPU1, tid),
        "placed nowhere"
    );
    assert_eq!(home(&k, tid), None, "no home bound for the replacement");

    // The replacement's own stamp is admitted, bound and placed on that home.
    assert!(matches!(
        k.sr_enqueue_committed_receiver_reconciled_split(tid, s2, Some(CPU1)),
        ReceiverEnqueue::Enqueued { cpu } if cpu == CPU1
    ));
    assert_eq!(home(&k, tid), Some(CPU1));
    assert_eq!(
        k.incarnation_home_membership_split_read(tid, s2),
        M::Present
    );
    assert_eq!(k.incarnation_home_membership_split_read(tid, s1), M::Stale);
}

fn plant_current_from_queue(k: &SharedKernel, cpu: CpuId, tid: u64) {
    assert_eq!(
        k.with_cpu_sched_split(cpu, |c| c.dispatch_next())
            .expect("cpu"),
        Some(ThreadId(tid))
    );
}

/// The receive family's capture: Phase A reads the stamp in the same section as its clear, and a
/// recovery bound to it refuses a same-ASID replacement — the exact restore, the
/// preempt-and-prefer commit and the placement classifier all answer `IncarnationMoved`, with the
/// slot left empty. Without the reuse the same recovery restores.
#[test]
fn sl2b_recv_recovery_is_bound_to_the_phase_a_stamp() {
    for reuse in [false, true] {
        let k = kernel2();
        let tid = 95_101;
        let s1 = incarnate(&k, tid, Some(CPU1), TaskStatus::Running);
        plant_current(&k, CPU1, tid);
        let captured = k
            .current_entering_incarnation_split_read(CPU1)
            .expect("entering capture");
        assert_eq!(captured.stamp, s1, "captured with tid, asid and priority");
        let (asid, priority, stamp) = k.recv_block_phase_a_split(CPU1, tid).expect("phase A");
        assert_eq!((asid, stamp), (ASID, s1));
        let stamp_now = if reuse {
            incarnate(&k, tid, Some(CPU1), TaskStatus::Runnable)
        } else {
            s1
        };
        let outcome = k.restore_entering_incarnation_exact_split(
            CPU1,
            RecvEnteringIncarnation {
                tid,
                asid,
                priority,
                stamp,
            },
        );
        if reuse {
            assert_ne!(stamp_now, s1);
            assert_eq!(outcome, RecvUnwindOutcome::IncarnationMoved);
            assert_eq!(k.current_tid_split_read(CPU1), None, "slot left empty");
        } else {
            assert_eq!(outcome, RecvUnwindOutcome::Restored);
            assert_eq!(k.current_tid_split_read(CPU1), Some(tid));
        }
    }
}

// ── distinct outcomes, including the current-and-queued contradiction ────────────────────────

/// The five entering-frame verdicts and the yield classifier's outcomes, each answered from the
/// home's 1L alone — including "current on another CPU" (the asking CPU is not the home) and the
/// current-and-queued contradiction.
#[test]
fn sl2b_home_reads_keep_every_distinct_placement_outcome() {
    let k = kernel2();
    let tid = 95_201;
    let s = incarnate(&k, tid, Some(CPU1), TaskStatus::Runnable);
    // Absent.
    assert_eq!(
        k.entering_frame_authority_split_read(CPU1, tid, ASID),
        A::Forfeited(R::PlacedNowhere)
    );
    assert_eq!(
        k.yield_forfeited_placement_split_read(CPU1, entering(tid, s)),
        RecvUnwindOutcome::Unpublished
    );
    // Queued on its home.
    plant_queued(&k, CPU1, tid);
    assert_eq!(
        k.entering_frame_authority_split_read(CPU0, tid, ASID),
        A::Forfeited(R::QueuedForDispatch)
    );
    assert_eq!(
        k.yield_forfeited_placement_split_read(CPU0, entering(tid, s)),
        RecvUnwindOutcome::QueuedRunnable(CPU1)
    );
    // A `Running` incarnation current on its home: this CPU owns the frame; any other CPU sees it
    // running elsewhere. (The `Runnable` one leaves CPU 1 first.)
    plant_current_from_queue(&k, CPU1, tid);
    clear_current(&k, CPU1, tid);
    let tid = 95_202;
    let s = incarnate(&k, tid, Some(CPU1), TaskStatus::Running);
    plant_current(&k, CPU1, tid);
    assert_eq!(
        k.entering_frame_authority_split_read(CPU1, tid, ASID),
        A::OwnsEnteringFrame
    );
    assert_eq!(
        k.entering_frame_authority_split_read(CPU0, tid, ASID),
        A::Forfeited(R::CurrentOnAnotherCpu)
    );
    assert_eq!(
        k.yield_forfeited_placement_split_read(CPU0, entering(tid, s)),
        RecvUnwindOutcome::RunningElsewhere(CPU1)
    );
    assert_eq!(
        k.yield_forfeited_placement_split_read(CPU1, entering(tid, s)),
        RecvUnwindOutcome::TableDisagree,
        "this CPU still holds the task whose frame was disowned"
    );
    // Current AND queued (a planted contradiction): a second placement, wherever it is asked.
    k.with_cpu_sched_split(CPU1, |c| {
        c.plant_second_placement_for_test(ThreadId(tid), TaskPriority::Normal)
    })
    .expect("cpu")
    .expect("planted");
    assert_eq!(
        k.incarnation_home_placement_split(tid, s),
        HomePlacement::On(TaskPlacement::CurrentAndQueued(CPU1))
    );
    for asking in [CPU0, CPU1] {
        assert_eq!(
            k.entering_frame_authority_split_read(asking, tid, ASID),
            A::Forfeited(R::SecondPlacement)
        );
        assert_eq!(
            k.yield_forfeited_placement_split_read(asking, entering(tid, s)),
            RecvUnwindOutcome::TableDisagree
        );
    }
    assert_eq!(k.incarnation_home_membership_split_read(tid, s), M::Present);
}

// ── exit validation, exit versus dispatch, and reap ──────────────────────────────────────────

/// An exited incarnation that is still queued is found on its home — before and after a dispatch
/// on the home takes it — so the post-lock exit validation fails closed and the reaper refuses.
/// Absent there, both pass. The trapping CPU (not the home) reports its own current slot.
#[test]
fn sl2b_an_exited_entry_still_queued_is_found_on_its_home() {
    let k = kernel2();
    let tid = 95_301;
    let s = incarnate(&k, tid, Some(CPU1), TaskStatus::Exited(0));
    plant_queued(&k, CPU1, tid);
    let cpu0_current = k.current_tid_split_read(CPU0);
    let v = k
        .post_lock_exit_validation_split(CPU0, tid, ASID)
        .expect("validation");
    assert!(v.identity_ok && v.terminal && v.in_runqueue);
    assert_eq!(v.current, cpu0_current, "the trapping CPU's own slot");
    let reap = SharedReapOwners {
        shared: &k,
        cpu: CPU0,
    };
    assert_eq!(reap.target_is_scheduled(tid, 0), M::Present);
    assert_eq!(reap.target_is_scheduled(tid, s), M::Present);
    // Exit versus dispatch: the home dispatches the exited entry; it is now current there.
    plant_current_from_queue(&k, CPU1, tid);
    let v = k
        .post_lock_exit_validation_split(CPU0, tid, ASID)
        .expect("validation");
    assert!(v.in_runqueue, "current on its home is still a placement");
    let v = k
        .post_lock_exit_validation_split(CPU1, tid, ASID)
        .expect("validation");
    assert_eq!(v.current, Some(tid), "still current on the trapping CPU");
    // Gone from its home: absent everywhere.
    clear_current(&k, CPU1, tid);
    let v = k
        .post_lock_exit_validation_split(CPU0, tid, ASID)
        .expect("validation");
    assert!(v.identity_ok && v.terminal && !v.in_runqueue);
    assert_eq!(reap.target_is_scheduled(tid, s), M::Absent);
    let crate::runtime::HomeSection::Home { stamp, .. } =
        k.with_incarnation_home_split(tid, 0, |_, _| ())
    else {
        panic!("homed");
    };
    assert_eq!(stamp, s, "a numeric-TID read pins the incarnation it saw");
}

/// When the trapping CPU is NOT the exiting task's home, its own current slot is still read under
/// its own 1L: a (planted, H-HOME-violating) exiting task current there is reported as `current`,
/// which the consumer refuses as `EXIT_TASK_EXITING_STILL_CURRENT`.
#[test]
fn sl2b_exit_validation_reads_the_trapping_cpu_slot_when_it_is_not_the_home() {
    let k = kernel2();
    let tid = 95_401;
    let _ = incarnate(&k, tid, Some(CPU0), TaskStatus::Exited(0));
    plant_current(&k, CPU1, tid);
    let v = k
        .post_lock_exit_validation_split(CPU1, tid, ASID)
        .expect("validation");
    assert_eq!(v.current, Some(tid));
    assert!(v.identity_ok && v.terminal);
    assert!(
        !v.in_runqueue,
        "the home (CPU 0) holds no placement; the contradiction is caught by `current`"
    );
    // An invalid trapping CPU keeps its admission error.
    assert!(
        k.post_lock_exit_validation_split(CpuId(200), tid, ASID)
            .is_err()
    );
}

// ── TID 0, kernel threads, and the homeless incarnation racing its first placement ───────────

/// TID 0 and kernel-thread ids have no TCB: every reader answers through its stale outcome, none
/// as "absent", and the entering-frame authority forfeits as not resumable.
#[test]
fn sl2b_tid0_and_kernel_thread_ids_are_never_answered_as_absent() {
    let k = kernel2();
    let kthread = (0x4B54u64 << 48) | 1;
    assert!(crate::kernel::kthread::is_kthread_tid(kthread));
    for tid in [0, kthread] {
        assert_eq!(k.incarnation_home_membership_split_read(tid, 0), M::Stale);
        assert_eq!(
            k.incarnation_home_placement_split(tid, 0),
            HomePlacement::Stale
        );
    }
    // A kernel thread has no TCB, so no frame authority.
    assert_eq!(
        k.entering_frame_authority_split_read(CPU0, kthread, Asid(0)),
        A::Forfeited(R::IncarnationNotResumable)
    );
    // TID 0 is each CPU's own sentinel: asked on CPU 1 (where it is not current), it never owns
    // CPU 1's entering frame, whatever CPU 0's TID 0 is doing.
    assert_ne!(
        k.entering_frame_authority_split_read(CPU1, 0, Asid(0)),
        A::OwnsEnteringFrame
    );
}

/// A homeless incarnation is placed nowhere; racing its first placement, a reader sees `Absent`
/// and then `Present` — never `Stale`, never a placement off the home that was recorded.
#[test]
fn sl2b_a_homeless_incarnation_racing_its_first_placement() {
    let k = kernel2();
    let tids: Vec<u64> = (95_500..95_560).collect();
    for &tid in &tids {
        let s = incarnate(&k, tid, None, TaskStatus::Runnable);
        let (seen, placed_on) = std::thread::scope(|sc| {
            let reader = sc.spawn(|| {
                let mut seen = Vec::new();
                for _ in 0..64 {
                    seen.push(k.incarnation_home_membership_split_read(tid, s));
                }
                seen
            });
            // The direct-IPC committed enqueue is a first placement that binds the home.
            let placed_on = match k.sr_enqueue_committed_receiver_reconciled_split(tid, s, None) {
                ReceiverEnqueue::Enqueued { cpu } => cpu,
                other => panic!("first placement: {other:?}"),
            };
            (reader.join().expect("reader"), placed_on)
        });
        assert!(seen.iter().all(|m| *m != M::Stale));
        let first_present = seen.iter().position(|m| *m == M::Present);
        if let Some(p) = first_present {
            assert!(seen[p..].iter().all(|m| *m == M::Present), "monotone");
        }
        assert_eq!(home(&k, tid), Some(placed_on));
        assert_eq!(k.incarnation_home_membership_split_read(tid, s), M::Present);
        // Withdraw so the queue does not fill.
        plant_current_from_queue(&k, placed_on, tid);
        clear_current(&k, placed_on, tid);
    }
}
