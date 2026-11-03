// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD1-ACCEPTANCE §3 — finding F1, the RISC-V D2 drains' empty-current settlement.
//!
//! The RISC-V bridge itself compiles only for RISC-V, so what is driven here is everything it
//! consumes: the D2 receive and send selection steps and the mark, through the production owners,
//! from the state those drains see — the outgoing task's blocking commit cleared `current` on CPU 0
//! and armed the class's deferral. Each case checks the scheduler and task state the outcome
//! leaves, which is what decides the settlement:
//!
//! * no acceptable candidate (`NoneAcceptable` → `RefusedNoSchedulerChange`) and a mark rolled
//!   back (`RefusedRolledBack`) both leave NOTHING current, the outgoing task still blocked and the
//!   refused entry still queued. Returning to "current" there resumes the blocked caller's stale
//!   frame — the F1 defect — so the drain must settle as typed kernel idle;
//! * an empty queue is a clean idle, the one outcome that publishes return provenance;
//! * a newly eligible candidate is marked and resumable.
//!
//! The source guards at the end pin that the RISC-V drains consume those outcomes that way.

use super::Bootstrap;
use crate::kernel::ipc::ThreadId;
use crate::kernel::scheduler::CpuId;
use crate::kernel::task::{TaskStatus, WaitReason};
use crate::runtime::{DispatchAuthority, DispatchMarkOutcome, SharedKernel};

const RV_TRAP_SRC: &str = include_str!("../../../arch/riscv64/trap.rs");

/// The task whose blocking syscall committed (a receiver or a sender).
const W: u64 = 7201;
/// A task that can be dispatched.
const Q: u64 = 7202;
/// A queued id no owner will accept: no task control block names it.
const PHANTOM: u64 = 7299;
const CPU0: CpuId = CpuId(0);

fn clear_cells() {
    crate::kernel::boot::d2_recv_dispatch_clear(0);
    crate::kernel::boot::d2_send_dispatch_clear(0);
    let _ = crate::kernel::boot::blocked_syscall_idle_provenance_take(0);
}

/// W running on CPU 0, then blocked in `reason` by its own commit (`current` cleared); Q
/// registered and resumable but not queued.
fn blocked(reason: WaitReason) -> SharedKernel {
    crate::arch::hal::reset_all_active_address_spaces();
    clear_cells();
    let k = SharedKernel::new(Bootstrap::init().expect("init"));
    k.with(|s| {
        for t in [W, Q] {
            s.register_task(t).expect("task");
            let (a, _) = s.create_user_address_space().expect("asid");
            s.bind_task_asid(t, a).expect("bind");
            s.set_task_status_for_test(t, TaskStatus::Runnable);
            s.seed_resumable_user_context_for_test(t);
            s.set_task_home_cpu(t, CPU0).expect("home");
        }
        s.enqueue_task(W).expect("enqueue w");
        s.dispatch_next_task().expect("dispatch w");
        s.set_task_status_for_test(W, TaskStatus::Blocked(reason));
        assert_eq!(s.block_current_cpu(), Some(W));
    });
    assert_eq!(
        k.current_tid_split_read(CPU0),
        None,
        "the commit cleared current"
    );
    k
}

fn recv_blocked() -> SharedKernel {
    let k = blocked(WaitReason::EndpointReceive(
        crate::kernel::capabilities::CapId(5),
    ));
    assert!(crate::kernel::boot::d2_recv_dispatch_try_defer(0, W));
    k
}

fn send_blocked() -> SharedKernel {
    let k = blocked(WaitReason::EndpointSend(
        crate::kernel::capabilities::CapId(5),
    ));
    assert!(crate::kernel::boot::d2_send_dispatch_try_defer(0, W));
    k
}

fn queued(k: &SharedKernel) -> std::vec::Vec<u64> {
    let mut v = std::vec::Vec::new();
    k.with_all_cpus_sched_split(|all| all.for_each_queued_on(CPU0, |_, t| v.push(t.0)));
    v
}

fn status(k: &SharedKernel, tid: u64) -> Option<TaskStatus> {
    k.with(|s| s.task_status(tid))
}

fn enqueue_phantom(k: &SharedKernel) {
    k.with_cpu_sched_split(CPU0, |c| {
        c.enqueue_with_priority(
            ThreadId(PHANTOM),
            crate::kernel::scheduler::TaskPriority::Normal,
        )
    })
    .expect("cpu")
    .expect("phantom queued");
}

fn recv_step(k: &SharedKernel) -> DispatchMarkOutcome {
    let d = k.d2_recv_dispatch_step_mut(DispatchAuthority::live_for_test(CPU0));
    k.d6_genuine_mark_running_via_task_seam(d)
}

fn send_step(k: &SharedKernel) -> DispatchMarkOutcome {
    let d = k.d2_send_dispatch_step_mut(DispatchAuthority::live_for_test(CPU0));
    k.d6_genuine_mark_running_via_task_seam(d)
}

/// What every refusal leaves: nothing current, W still blocked, the refused entry still queued.
fn assert_refusal_left_nothing_current(k: &SharedKernel, refused: u64, w_status: TaskStatus) {
    assert_eq!(k.current_tid_split_read(CPU0), None, "nothing is current");
    assert_eq!(
        status(k, W),
        Some(w_status),
        "the outgoing task is still blocked"
    );
    assert!(
        queued(k).contains(&refused),
        "the refused entry is still queued"
    );
    assert!(!queued(k).contains(&W), "the blocked caller is in no queue");
}

#[test]
fn no_acceptable_candidate_leaves_nothing_current_on_receive_and_send() {
    for (k, step, w) in [
        (
            recv_blocked(),
            recv_step as fn(&SharedKernel) -> DispatchMarkOutcome,
            TaskStatus::Blocked(WaitReason::EndpointReceive(
                crate::kernel::capabilities::CapId(5),
            )),
        ),
        (
            send_blocked(),
            send_step as fn(&SharedKernel) -> DispatchMarkOutcome,
            TaskStatus::Blocked(WaitReason::EndpointSend(
                crate::kernel::capabilities::CapId(5),
            )),
        ),
    ] {
        enqueue_phantom(&k);
        let out = step(&k);
        assert_eq!(out, DispatchMarkOutcome::RefusedNoSchedulerChange);
        assert_refusal_left_nothing_current(&k, PHANTOM, w);
        clear_cells();
    }
}

#[test]
fn a_rolled_back_mark_leaves_nothing_current_and_the_candidate_queued() {
    let k = recv_blocked();
    k.with(|s| s.enqueue_task(Q).expect("q"));
    let d = k.d2_recv_dispatch_step_mut(DispatchAuthority::live_for_test(CPU0));
    // The accepted candidate stops being markable between acceptance and the mark, as a
    // concurrent owner on another CPU would make it.
    k.with(|s| s.set_task_status_for_test(Q, TaskStatus::Blocked(WaitReason::Poll)));
    let out = k.d6_genuine_mark_running_via_task_seam(d);
    assert_eq!(out, DispatchMarkOutcome::RefusedRolledBack);
    assert_refusal_left_nothing_current(
        &k,
        Q,
        TaskStatus::Blocked(WaitReason::EndpointReceive(
            crate::kernel::capabilities::CapId(5),
        )),
    );
    clear_cells();
}

#[test]
fn an_empty_queue_is_a_clean_idle_and_a_newly_eligible_candidate_is_marked() {
    let k = recv_blocked();
    assert_eq!(recv_step(&k), DispatchMarkOutcome::Idle);
    assert_eq!(k.current_tid_split_read(CPU0), None);
    // After a refusal settled as idle, the candidate that becomes eligible is dispatched by the
    // next queue advance: here, the same step finds and marks it.
    enqueue_phantom(&k);
    assert_eq!(recv_step(&k), DispatchMarkOutcome::RefusedNoSchedulerChange);
    k.with(|s| s.enqueue_task(Q).expect("q"));
    match recv_step(&k) {
        DispatchMarkOutcome::Marked(token) => assert_eq!(token.tid(), Q),
        other => panic!("a newly eligible candidate is marked, got {other:?}"),
    }
    assert_eq!(k.current_tid_split_read(CPU0), Some(Q));
    assert_eq!(status(&k, Q), Some(TaskStatus::Running));
    assert!(
        queued(&k).contains(&PHANTOM),
        "the refused entry was never dequeued"
    );
    clear_cells();
}

// ── the RISC-V consumer ─────────────────────────────────────────────────────────────────────

fn drain(start: &str, end: &str) -> &'static str {
    RV_TRAP_SRC
        .split(start)
        .nth(1)
        .expect("the drain")
        .split(end)
        .next()
        .expect("its body")
}

fn send_drain() -> &'static str {
    drain(
        "if cpu_idx < MAX_CPUS && crate::kernel::boot::d2_send_dispatch_is_deferred(cpu_idx) {",
        "// ── U4: queue-advancing D2 RECV drain",
    )
}

fn recv_drain() -> &'static str {
    drain(
        "if cpu_idx < MAX_CPUS && crate::kernel::boot::d2_recv_dispatch_is_deferred(cpu_idx) {",
        "// ── Stage 196E",
    )
}

#[test]
fn both_risc_v_d2_drains_settle_a_refusal_as_typed_kernel_idle() {
    for (name, body, prefix) in [
        ("send", send_drain(), "D2_SEND"),
        ("recv", recv_drain(), "D2_RECV"),
    ] {
        for reason in ["refused_dequeue_undone", "refused_scheduler_untouched"] {
            assert!(
                body.contains(&std::format!("refused = Some(\"{reason}\");")),
                "{name}: the {reason} arm records its refusal"
            );
        }
        let settle = body
            .split("if let Some(reason) = refused {")
            .nth(1)
            .unwrap_or_else(|| panic!("{name}: a refusal settlement"));
        let settle = &settle[..settle.find("\n            }\n").expect("its end")];
        assert!(
            settle.contains(&std::format!("{prefix}_GENUINE_DISPATCH_SETTLED"))
                && settle.contains("reason: RiscvIdleReason::QueueAdvanceNoIncoming")
                && settle.contains("_dispatch_clear(cpu_idx);"),
            "{name}: a refusal clears the deferral and settles as typed kernel idle"
        );
        assert!(
            !settle.contains("provenance_set"),
            "{name}: a refusal publishes no provenance"
        );
        // It is settled before the resume and before the idle branch, so the tail never sees it.
        assert!(
            body.find("if let Some(reason) = refused {")
                < body.find("if let Some(token) = marked {"),
            "{name}: settled before anything else"
        );
    }
}

#[test]
fn the_send_drain_publishes_provenance_for_a_clean_idle_alone() {
    let body = send_drain();
    assert_eq!(
        body.matches("clean_idle = true;").count(),
        1,
        "set by the Idle arm only"
    );
    let idle_arm = body.split("Mark::Idle => {").nth(1).expect("the idle arm");
    let idle_arm = &idle_arm[..idle_arm.find("None\n").expect("arm end")];
    assert!(idle_arm.contains("clean_idle = true;"));
    let publish = body
        .split("if clean_idle {")
        .nth(1)
        .expect("the gated publication");
    assert!(publish.contains("BlockingSyscallClass::IpcSend"));
}
