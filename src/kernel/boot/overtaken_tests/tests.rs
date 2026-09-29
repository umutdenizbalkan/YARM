// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP3-ACCEPTANCE §2 — the overtaken-deferral settlements of the shared x86_64/AArch64
//! bridge, driven through production owners.
//!
//! The shape every case starts from is the one the bridge's drains see: a blocking-class commit
//! on CPU 0 cleared `current` and armed the class's deferral cell, and the frame the bridge holds
//! is the outgoing task's (its entry bound `resume_owner` to that incarnation). Then another owner
//! runs before the drain — a FutexWake issued from CPU 1, a receiver on CPU 1 taking a blocked
//! sender's message — and the drain's class re-verify fails.
//!
//! The first two tests reproduce what the UNMODIFIED bridge was left with at that point, using
//! only owners that are unchanged by this package; the rest drive the new settlement.

use super::Bootstrap;
use crate::kernel::scheduler::CpuId;
use crate::kernel::task::{BlockedSyscallClass, TaskStatus, UserRegisterContext, WaitReason};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::{Asid, VirtAddr};
use crate::runtime::{
    DispatchAuthority, FpuHomeOwner, FpuHomeRefusal, OvertakenSettlement, SharedKernel,
    SplitReturnIdentity, UserFpuReturn,
};

/// The task whose drain is overtaken (a futex waiter or a blocked sender).
const W: u64 = 7101;
/// Another task, queued on CPU 0 ahead of it.
const Q: u64 = 7102;
const FUTEX_ADDR: usize = 0x4_2000;
/// A send result a resumed sender can only have received from its parked completion.
const SEND_RESULT: u64 = 7;
const CPU0: CpuId = CpuId(0);
const CPU1: CpuId = CpuId(1);

struct Fixture {
    k: SharedKernel,
    w_asid: Asid,
    q_asid: Asid,
    /// The continuation W's syscall entry committed, as the production capture does.
    w_saved: UserRegisterContext,
}

fn clear_cells() {
    crate::kernel::boot::futex_wait_dispatch_clear(0);
    crate::kernel::boot::d2_recv_dispatch_clear(0);
    crate::kernel::boot::d2_send_dispatch_clear(0);
    crate::kernel::boot::yield_dispatch_clear(0);
}

/// W running on CPU 0 with a committed continuation; Q registered but not queued; CPU 1 online.
fn fixture() -> Fixture {
    crate::arch::hal::reset_all_active_address_spaces();
    clear_cells();
    let k = SharedKernel::new(Bootstrap::init().expect("init"));
    let (w_asid, q_asid) = k.with(|s| {
        s.bring_up_cpu(CPU1).expect("cpu 1");
        s.register_task(W).expect("w");
        s.register_task(Q).expect("q");
        let (a, _) = s.create_user_address_space().expect("w asid");
        s.bind_task_asid(W, a).expect("bind w");
        let (b, _) = s.create_user_address_space().expect("q asid");
        s.bind_task_asid(Q, b).expect("bind q");
        for t in [W, Q] {
            s.set_task_status_for_test(t, TaskStatus::Runnable);
            s.seed_resumable_user_context_for_test(t);
            s.set_task_home_cpu(t, CPU0).expect("home");
        }
        s.enqueue_task(W).expect("enqueue w");
        s.dispatch_next_task().expect("dispatch w");
        (a, b)
    });
    assert_eq!(
        k.current_tid_split_read(CPU0),
        Some(W),
        "W is running on CPU 0"
    );
    let mut w_saved = entry_frame().capture_user_context();
    w_saved.instruction_ptr = VirtAddr(0x40_1234);
    w_saved.stack_ptr = VirtAddr(0x7ffe_f000);
    assert!(k.split_return_commit_context_split(
        SplitReturnIdentity {
            tid: W,
            asid: w_asid
        },
        w_saved
    ));
    Fixture {
        k,
        w_asid,
        q_asid,
        w_saved,
    }
}

/// The frame W's own trap entered with: its registers, and the owner the entry authenticated.
fn entry_frame() -> TrapFrame {
    let mut f = TrapFrame::new(9, [FUTEX_ADDR, 0, 0, 0, 0, 0]);
    for i in 0..16 {
        f.set_user_gpr(i, 0xE000 + i);
    }
    f.set_saved_pc(0x40_1230);
    f.set_saved_sp(0x7ffe_f000);
    f
}

fn owned_entry_frame(tid: u64, asid: Asid) -> TrapFrame {
    let mut f = entry_frame();
    f.bind_resume_owner(FpuHomeOwner { tid, asid });
    f
}

fn status(k: &SharedKernel, tid: u64) -> Option<TaskStatus> {
    k.with(|s| s.task_status(tid))
}

/// The run queue of `cpu`, in dispatch order.
fn queued(k: &SharedKernel, cpu: CpuId) -> alloc::vec::Vec<u64> {
    k.with(|s| {
        s.with_scheduler_state(|sched| {
            let mut v = alloc::vec::Vec::new();
            super::kernel_ref(&sched.scheduler).for_each_queued_on(cpu, |_, t| v.push(t.0));
            v
        })
    })
}

/// The FutexWait commit, through its production owners: W parks (status registered, `current`
/// compare-and-cleared) and the class's drain cell is armed with W as the outgoing task.
fn futex_wait_commit(fx: &Fixture) {
    let parked =
        fx.k.futex_wait_park_exact_split(CPU0, W, fx.w_asid, FUTEX_ADDR);
    assert!(
        matches!(
            parked,
            crate::kernel::syscall::sched::FutexParkOutcome::Parked { .. }
        ),
        "W parked: {parked:?}"
    );
    assert!(crate::kernel::boot::futex_wait_dispatch_try_defer(0, W));
    assert_eq!(
        fx.k.current_tid_split_read(CPU0),
        None,
        "the commit cleared current"
    );
}

/// The overtaking wake: an ordinary FutexWake issued on CPU 1, through its production owner.
fn remote_futex_wake(fx: &Fixture) {
    assert_eq!(
        fx.k.futex_wake_split_mut(CPU1, FUTEX_ADDR, 1),
        1,
        "one waiter woken"
    );
}

fn futex_step(k: &SharedKernel, a: DispatchAuthority) -> crate::runtime::CpuDispatch {
    k.futex_wait_dispatch_step_mut(a)
}

// ── The base, reproduced ──────────────────────────────────────────────────────────────────────

/// What the unmodified drain left: its re-verify fails, it clears the cell and falls through, and
/// the bridge returns with the frame still holding W's continuation while W is QUEUED, `Runnable`
/// and current nowhere. On AArch64 the vector tail then asks the FP/SIMD return settlement for
/// that owner — which refuses (`NoCurrent`), and the tail halts the CPU. Ordinary contention
/// ended in a fatal stop.
#[test]
fn base_futex_wait_overtaken_leaves_a_queued_task_in_the_frame() {
    let fx = fixture();
    futex_wait_commit(&fx);
    remote_futex_wake(&fx);
    let frame = owned_entry_frame(W, fx.w_asid);

    assert!(
        !fx.k.futex_wait_reverify_blocked(W),
        "the drain's re-verify fails"
    );
    crate::kernel::boot::futex_wait_dispatch_clear(0);
    assert_eq!(status(&fx.k, W), Some(TaskStatus::Runnable));
    assert_eq!(queued(&fx.k, CPU0), [W], "W is on CPU 0's run queue");
    assert_eq!(
        fx.k.current_tid_split_read(CPU0),
        None,
        "and current nowhere"
    );
    assert_eq!(
        frame.resume_owner(),
        Some(FpuHomeOwner {
            tid: W,
            asid: fx.w_asid
        })
    );
    assert_eq!(
        fx.k.settle_user_fpu_return_split(CPU0, frame.resume_owner()),
        UserFpuReturn::Refuse(FpuHomeRefusal::NoCurrent),
        "the AArch64 tail's authentication refuses this frame: the base halts"
    );
    clear_cells();
}

/// The x86_64 base did not halt: its trap tail found `current == None` and ran the owner
/// revalidation, which selects from the run queue and restores the selected task's SAVED CONTEXT.
/// For a blocked SENDER that is not the continuation: the result of the send lives in the parked
/// `IpcSend` completion the receiver published, which only the exact-token resume consumes. The
/// revalidated sender resumes with its pre-block lanes and the completion stays parked.
#[test]
fn base_x86_revalidation_resumes_an_overtaken_sender_without_its_completion() {
    let fx = fixture();
    // The blocking-send commit: W blocked in EndpointSend, `current` cleared, the cell armed.
    fx.k.with(|s| {
        s.set_task_status_for_test(
            W,
            TaskStatus::Blocked(WaitReason::EndpointSend(
                crate::kernel::capabilities::CapId(4),
            )),
        );
        assert_eq!(s.block_current_cpu(), Some(W), "the commit clears current");
    });
    assert!(crate::kernel::boot::d2_send_dispatch_try_defer(0, W));
    // The overtaking receiver on CPU 1: it parks the sender's result, then wakes it.
    fx.k.with(|s| {
        s.set_pending_syscall_completion_of_class_for_test(
            W,
            BlockedSyscallClass::IpcSend,
            SEND_RESULT,
        );
        s.set_task_status_for_test(W, TaskStatus::Runnable);
        s.enqueue_task(W).expect("wake");
    });
    assert!(
        !fx.k.d2_send_reverify_blocked(W),
        "the drain's re-verify fails"
    );
    crate::kernel::boot::d2_send_dispatch_clear(0);

    let mut frame = owned_entry_frame(W, fx.w_asid);
    let outcome = fx.k.revalidate_idle_owner_after_drains(CPU0, &mut frame);
    assert_eq!(outcome, crate::runtime::OwnerRevalidation::Replacement(W));
    assert_eq!(frame.saved_pc(), fx.w_saved.instruction_ptr.0 as usize);
    assert_eq!(
        (frame.ret0, frame.error),
        (0, 0),
        "no result lane carries the send's result {SEND_RESULT}"
    );
    assert!(
        fx.k.with(|s| s.has_pending_syscall_completion(W)),
        "the sender's completion was never consumed"
    );
    clear_cells();
}

// ── The settlement ────────────────────────────────────────────────────────────────────────────

const R: u64 = 7103;

fn settle(fx: &Fixture, prepared: Option<FpuHomeOwner>) -> OvertakenSettlement {
    fx.k.settle_overtaken_deferral_split(
        DispatchAuthority::live_for_test(CPU0),
        prepared,
        "overtaken_test",
        futex_step,
    )
}

fn w_owner(fx: &Fixture) -> FpuHomeOwner {
    FpuHomeOwner {
        tid: W,
        asid: fx.w_asid,
    }
}

/// Remote wake after the blocking publication, before the drain; the woken task is queued HERE and
/// nothing else is. The one queue advance selects it by exact token, and the frame receives
/// exactly its committed continuation, owned by it — the FP/SIMD settlement admits that owner.
#[test]
fn overtaken_waiter_is_resumed_through_its_exact_token() {
    let fx = fixture();
    futex_wait_commit(&fx);
    remote_futex_wake(&fx);
    crate::kernel::boot::futex_wait_dispatch_clear(0);

    let token = match settle(&fx, Some(w_owner(&fx))) {
        OvertakenSettlement::Switch(t) => t,
        other => panic!("expected a switch, got {other:?}"),
    };
    assert_eq!(token.tid(), W);
    assert_eq!(token.expect_asid(), Some(fx.w_asid));
    assert_eq!(fx.k.current_tid_split_read(CPU0), Some(W));
    assert_eq!(status(&fx.k, W), Some(TaskStatus::Running));
    assert!(queued(&fx.k, CPU0).is_empty(), "dequeued exactly once");
    assert!(queued(&fx.k, CPU1).is_empty());

    let mut frame = owned_entry_frame(W, fx.w_asid);
    assert_eq!(
        crate::arch::x86_64::trap::x86_post_lock_resume_marked_incoming(
            &fx.k,
            token,
            Some(&mut frame)
        ),
        Ok(())
    );
    assert_eq!(frame.saved_pc(), fx.w_saved.instruction_ptr.0 as usize);
    assert_eq!(frame.saved_sp(), fx.w_saved.stack_ptr.0 as usize);
    assert_eq!(frame.user_gprs, fx.w_saved.user_gprs);
    assert_eq!(frame.user_status, fx.w_saved.user_status);
    assert_eq!(frame.resume_owner(), Some(w_owner(&fx)));
    assert_eq!(
        fx.k.settle_user_fpu_return_split(CPU0, frame.resume_owner()),
        UserFpuReturn::Install {
            owner: w_owner(&fx),
            state: crate::kernel::user_fpu::UserFpuState::initial(),
        },
        "the return's FP/SIMD home is the resumed incarnation's"
    );
    clear_cells();
}

/// The advance is the scheduler's, not the overtaken task's: another task queued first is
/// selected, and the woken task stays queued, `Runnable`, exactly once.
#[test]
fn overtaken_drain_selects_the_queue_head_and_leaves_the_woken_task_queued() {
    let fx = fixture();
    futex_wait_commit(&fx);
    fx.k.with(|s| s.enqueue_task(Q).expect("q first"));
    remote_futex_wake(&fx);
    crate::kernel::boot::futex_wait_dispatch_clear(0);
    let token = match settle(&fx, Some(w_owner(&fx))) {
        OvertakenSettlement::Switch(t) => t,
        other => panic!("expected a switch, got {other:?}"),
    };
    assert_eq!(token.tid(), Q);
    assert_eq!(token.expect_asid(), Some(fx.q_asid));
    assert_eq!(fx.k.current_tid_split_read(CPU0), Some(Q));
    assert_eq!(status(&fx.k, W), Some(TaskStatus::Runnable));
    assert_eq!(queued(&fx.k, CPU0), [W]);
    clear_cells();
}

/// The woken task was queued on ANOTHER CPU and nothing is queued here: this CPU owes idle, and
/// W is left exactly where its waker placed it. The frame is not touched.
#[test]
fn overtaken_waiter_queued_elsewhere_leaves_this_cpu_idle() {
    let fx = fixture();
    futex_wait_commit(&fx);
    fx.k.with(|s| s.set_task_home_cpu(W, CPU1).expect("home 1"));
    remote_futex_wake(&fx);
    crate::kernel::boot::futex_wait_dispatch_clear(0);
    let frame = owned_entry_frame(W, fx.w_asid);
    let before = frame.clone();
    assert_eq!(
        settle(&fx, frame.resume_owner()),
        OvertakenSettlement::Idle { reason: "idle" }
    );
    assert_eq!(frame, before);
    assert_eq!(fx.k.current_tid_split_read(CPU0), None);
    assert_eq!(queued(&fx.k, CPU1), [W]);
    assert!(queued(&fx.k, CPU0).is_empty());
    assert_eq!(status(&fx.k, W), Some(TaskStatus::Runnable));
    clear_cells();
}

/// No acceptable incoming task: the only queued entry here has no resumable continuation. The
/// outcome is idle under its OWN name, nothing is dequeued and nothing becomes current.
#[test]
fn no_acceptable_incoming_settles_idle_under_its_own_reason() {
    let fx = fixture();
    fx.k.with(|s| {
        s.register_task(R).expect("r");
        let (c, _) = s.create_user_address_space().expect("r asid");
        s.bind_task_asid(R, c).expect("bind r");
        s.set_task_status_for_test(R, TaskStatus::Runnable);
        s.set_task_home_cpu(R, CPU0).expect("home");
    });
    futex_wait_commit(&fx);
    fx.k.with(|s| s.set_task_home_cpu(W, CPU1).expect("home 1"));
    fx.k.with(|s| s.enqueue_task(R).expect("r queued"));
    remote_futex_wake(&fx);
    crate::kernel::boot::futex_wait_dispatch_clear(0);
    assert_eq!(
        settle(&fx, Some(w_owner(&fx))),
        OvertakenSettlement::Idle {
            reason: "none_acceptable"
        }
    );
    assert_eq!(queued(&fx.k, CPU0), [R], "examined, not dequeued");
    assert_eq!(fx.k.current_tid_split_read(CPU0), None);
    clear_cells();
}

/// An incoming task already selected and its continuation installed (by an owner that ran before
/// this drain): nothing more is owed, and the frame is returned to — because it authenticates.
#[test]
fn an_installed_continuation_is_returned_to_when_it_authenticates() {
    let fx = fixture();
    futex_wait_commit(&fx);
    remote_futex_wake(&fx);
    fx.k.with(|s| s.dispatch_next_task().expect("installed by another owner"));
    assert_eq!(fx.k.current_tid_split_read(CPU0), Some(W));
    let queue_before = queued(&fx.k, CPU0);
    assert_eq!(
        settle(&fx, Some(w_owner(&fx))),
        OvertakenSettlement::ReturnToInstalled {
            owner: w_owner(&fx)
        }
    );
    assert_eq!(queued(&fx.k, CPU0), queue_before, "no second dispatch");
    assert_eq!(fx.k.current_tid_split_read(CPU0), Some(W));
    clear_cells();
}

/// The control: an installed continuation that is NOT the frame's is refused, whatever the
/// mismatch — another task, no owner at all, or the same TID in another address space. Nothing
/// is mutated by the refusal.
#[test]
fn a_mismatched_or_unowned_installed_continuation_is_refused() {
    let fx = fixture();
    futex_wait_commit(&fx);
    fx.k.with(|s| {
        s.enqueue_task(Q).expect("q");
        s.dispatch_next_task().expect("q installed");
    });
    remote_futex_wake(&fx);
    assert_eq!(fx.k.current_tid_split_read(CPU0), Some(Q));
    let queue_before = queued(&fx.k, CPU0);
    assert_eq!(
        settle(&fx, Some(w_owner(&fx))),
        OvertakenSettlement::Unauthenticated(FpuHomeRefusal::CurrentMismatch {
            tid: Q,
            expected: W
        })
    );
    assert_eq!(
        settle(&fx, None),
        OvertakenSettlement::Unauthenticated(FpuHomeRefusal::UnownedContinuation)
    );
    assert_eq!(
        settle(
            &fx,
            Some(FpuHomeOwner {
                tid: Q,
                asid: fx.w_asid
            })
        ),
        OvertakenSettlement::Unauthenticated(FpuHomeRefusal::IncarnationReplaced { tid: Q })
    );
    assert_eq!(fx.k.current_tid_split_read(CPU0), Some(Q));
    assert_eq!(queued(&fx.k, CPU0), queue_before);
    clear_cells();
}

/// A stale or replaced incarnation: the TID was re-bound to another address space after the
/// selection marked it. That contradicts the mark itself, so it is one of the states reserved for
/// the fatal terminal: the exact-token resume refuses before touching the frame, and the exact
/// rollback refuses too — its authority names the incarnation that no longer exists — so the
/// bridge diverges with `rolled_back=0` rather than resuming anything under either identity.
#[test]
fn a_replaced_incarnation_refuses_before_the_frame_and_fails_closed() {
    let fx = fixture();
    futex_wait_commit(&fx);
    remote_futex_wake(&fx);
    let token = match settle(&fx, Some(w_owner(&fx))) {
        OvertakenSettlement::Switch(t) => t,
        other => panic!("expected a switch, got {other:?}"),
    };
    fx.k.with(|s| {
        let (fresh, _) = s.create_user_address_space().expect("replacement");
        s.bind_task_asid(W, fresh).expect("rebind");
    });
    let mut frame = owned_entry_frame(W, fx.w_asid);
    let before = frame.clone();
    assert_eq!(
        crate::arch::x86_64::trap::x86_post_lock_resume_marked_incoming(
            &fx.k,
            token,
            Some(&mut frame)
        ),
        Err(crate::arch::x86_64::trap::X86ResumeRefusal::Asid)
    );
    assert_eq!(frame, before, "a refusal never touches the frame");
    assert!(
        !fx.k.direct_dispatch_rollback_split(
            token.into_dequeued_authority().expect("a genuine dequeue")
        ),
        "no rollback is fabricated for an incarnation that no longer exists"
    );
    assert!(queued(&fx.k, CPU0).is_empty(), "and nothing was re-queued");
    clear_cells();
}

/// The sender overtaken by a receiver on CPU 1: the settlement's exact-token resume consumes the
/// parked `IpcSend` completion — exactly once — and the frame carries its result, where the base
/// revalidation left the pre-block lanes and the completion parked. The drain cell is consumed
/// once and not latched.
#[test]
fn an_overtaken_sender_consumes_its_parked_completion_exactly_once() {
    let fx = fixture();
    fx.k.with(|s| {
        s.set_task_status_for_test(
            W,
            TaskStatus::Blocked(WaitReason::EndpointSend(
                crate::kernel::capabilities::CapId(4),
            )),
        );
        assert_eq!(s.block_current_cpu(), Some(W));
    });
    assert!(crate::kernel::boot::d2_send_dispatch_try_defer(0, W));
    fx.k.with(|s| {
        s.set_pending_syscall_completion_of_class_for_test(
            W,
            BlockedSyscallClass::IpcSend,
            SEND_RESULT,
        );
        s.set_task_status_for_test(W, TaskStatus::Runnable);
        s.enqueue_task(W).expect("wake");
    });
    assert!(!fx.k.d2_send_reverify_blocked(W));
    crate::kernel::boot::d2_send_dispatch_clear(0);
    assert!(!crate::kernel::boot::d2_send_dispatch_is_deferred(0));

    let token = match fx.k.settle_overtaken_deferral_split(
        DispatchAuthority::live_for_test(CPU0),
        Some(w_owner(&fx)),
        "overtaken_test",
        |k, a| k.d2_send_dispatch_step_mut(a),
    ) {
        OvertakenSettlement::Switch(t) => t,
        other => panic!("expected a switch, got {other:?}"),
    };
    let mut frame = owned_entry_frame(W, fx.w_asid);
    assert_eq!(
        crate::arch::x86_64::trap::x86_post_lock_resume_marked_incoming(
            &fx.k,
            token,
            Some(&mut frame)
        ),
        Ok(())
    );
    assert_eq!(
        frame.error, SEND_RESULT as usize,
        "the send's result reaches the frame"
    );
    assert!(
        !fx.k.with(|s| s.has_pending_syscall_completion(W)),
        "consumed"
    );
    assert_eq!(
        fx.k.direct_dispatch_take_send_completion_split(token),
        None,
        "and only once"
    );
    assert!(
        crate::kernel::boot::d2_send_dispatch_try_defer(0, W),
        "the cell is not latched"
    );
    clear_cells();
}

/// The receiver overtaken by a completion on CPU 1 — the production receive-timeout publication,
/// which installs the x86_64 result lanes into the saved continuation (and parks the arch-neutral
/// record AArch64 consumes). The settlement resumes the receiver by exact token, and the frame
/// carries exactly the continuation the completion installed: RIP, RSP and the TimedOut lanes.
#[test]
fn an_overtaken_receiver_resumes_with_the_installed_completion() {
    const TIMED_OUT: u64 = 9;
    let fx = fixture();
    fx.k.with(|s| {
        s.set_task_status_for_test(
            W,
            TaskStatus::Blocked(WaitReason::EndpointReceive(
                crate::kernel::capabilities::CapId(5),
            )),
        );
        assert_eq!(s.block_current_cpu(), Some(W));
    });
    assert!(crate::kernel::boot::d2_recv_dispatch_try_defer(0, W));
    fx.k.with(|s| {
        s.with_tcbs_mut(|tcbs| {
            let tcb = tcbs.iter_mut().flatten().find(|t| t.tid.0 == W).expect("w");
            crate::kernel::boot::ipc_state::publish_blocked_recv_timeout_result_with_identity(
                tcb, TIMED_OUT, W, fx.w_asid,
            );
        });
        s.set_task_status_for_test(W, TaskStatus::Runnable);
        s.enqueue_task(W).expect("wake");
    });
    assert!(
        !fx.k.d2_recv_reverify_blocked(W),
        "the drain's re-verify fails"
    );
    crate::kernel::boot::d2_recv_dispatch_clear(0);

    let token = match fx.k.settle_overtaken_deferral_split(
        DispatchAuthority::live_for_test(CPU0),
        Some(w_owner(&fx)),
        "overtaken_test",
        |k, a| k.d2_recv_dispatch_step_mut(a),
    ) {
        OvertakenSettlement::Switch(t) => t,
        other => panic!("expected a switch, got {other:?}"),
    };
    assert_eq!(token.tid(), W);
    let mut frame = owned_entry_frame(W, fx.w_asid);
    assert_eq!(
        crate::arch::x86_64::trap::x86_post_lock_resume_marked_incoming(
            &fx.k,
            token,
            Some(&mut frame)
        ),
        Ok(())
    );
    assert_eq!(frame.saved_pc(), fx.w_saved.instruction_ptr.0 as usize);
    assert_eq!(frame.saved_sp(), fx.w_saved.stack_ptr.0 as usize);
    assert_eq!(frame.user_gpr(0), 0, "RAX = ret0");
    assert_eq!(
        frame.user_gpr(2),
        TIMED_OUT as usize,
        "RCX = the TimedOut error lane"
    );
    assert_eq!(frame.resume_owner(), Some(w_owner(&fx)));
    assert!(queued(&fx.k, CPU0).is_empty());
    clear_cells();
}
