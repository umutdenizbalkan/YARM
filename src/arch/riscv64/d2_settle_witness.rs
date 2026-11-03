// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD1-ACCEPTANCE §3 — the default-off FIXTURE (`riscv64-d2-settle-witness`) for finding F1,
//! the D2 drains' empty-current settlement.
//!
//! It makes one production drain meet a queue whose every entry is refused, which is the state F1
//! needs and which ordinary traffic produces only by accident. Before a D2 drain's selection, when
//! nothing on this CPU is runnable, it queues ONE phantom id that no task control block names: the
//! acceptance filter refuses it in place, so the selection answers `NoneAcceptable` and the mark
//! `RefusedNoSchedulerChange`. Everything after that is the production drain, unchanged: on the
//! delivered base it fell through to `ReturnToCurrent` with nothing current (the blocked caller's
//! stale frame); repaired, it settles as typed kernel idle.
//!
//! At the next idle point the fixture records what it finds — nothing current, the outgoing task
//! still blocked in its class, the phantom still queued and never dequeued — and withdraws the
//! phantom, so the boot continues normally. It never touches a claim, a mark or a frame.

use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use crate::kernel::ipc::ThreadId;
use crate::kernel::scheduler::CpuId;
use crate::runtime::SharedKernel;

/// No task control block names it, and it is not a kernel-thread id.
pub const PHANTOM: u64 = 0xD2_5E77;

pub const CLASS_SEND: u8 = 1;
pub const CLASS_RECV: u8 = 2;

/// Per class: 0 = not yet, 1 = injected, 2 = recorded and withdrawn.
static STATE: [AtomicU8; 3] = [const { AtomicU8::new(0) }; 3];
static OUTGOING: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static CPU: [AtomicU8; 3] = [const { AtomicU8::new(0) }; 3];

fn class_name(class: u8) -> &'static str {
    if class == CLASS_SEND { "send" } else { "recv" }
}

/// Before a D2 drain's selection on `cpu`: the first time a drain of this class finds nothing
/// runnable here, queue the phantom so its selection meets a queue whose every entry is refused.
pub fn before_drain(shared: &SharedKernel, cpu: CpuId, class: u8, outgoing: Option<u64>) {
    let c = class as usize;
    if c >= STATE.len() || STATE[c].load(Ordering::Acquire) != 0 {
        return;
    }
    if shared.runnable_count_on_cpu_split_read(cpu) != 0 {
        return;
    }
    let queued = shared
        .with_cpu_sched_split(cpu, |v| {
            v.enqueue_with_priority(
                ThreadId(PHANTOM),
                crate::kernel::scheduler::TaskPriority::Normal,
            )
        })
        .map(|r| r.is_ok())
        .unwrap_or(false);
    if !queued {
        return;
    }
    OUTGOING[c].store(outgoing.unwrap_or(0), Ordering::Release);
    CPU[c].store(cpu.0, Ordering::Release);
    STATE[c].store(1, Ordering::Release);
    crate::yarm_log!(
        "D2SW_INJECTED class={} cpu={} outgoing={} phantom={:#x}",
        class_name(class),
        cpu.0,
        outgoing.unwrap_or(0),
        PHANTOM
    );
}

/// At an idle point on `cpu`: record the state each injected case settled into, then withdraw
/// the phantom.
pub fn at_idle(shared: &SharedKernel, cpu: CpuId) {
    for class in [CLASS_SEND, CLASS_RECV] {
        let c = class as usize;
        if STATE[c].load(Ordering::Acquire) != 1 || CPU[c].load(Ordering::Acquire) != cpu.0 {
            continue;
        }
        let out = OUTGOING[c].load(Ordering::Acquire);
        let current = shared.current_tid_split_read(cpu).unwrap_or(0);
        let status = shared.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|t| t.tid.0 == out)
                .map(|t| t.status)
        });
        let blocked_in_class = match (class, status) {
            (
                CLASS_SEND,
                Some(crate::kernel::task::TaskStatus::Blocked(
                    crate::kernel::task::WaitReason::EndpointSend(_),
                )),
            ) => 1,
            (
                CLASS_RECV,
                Some(crate::kernel::task::TaskStatus::Blocked(
                    crate::kernel::task::WaitReason::EndpointReceive(_),
                )),
            ) => 1,
            _ => 0,
        };
        let withdrawn = shared
            .with_cpu_sched_split(cpu, |v| v.withdraw_queued_tid(ThreadId(PHANTOM)))
            .map(|o| o == crate::kernel::scheduler::WithdrawOutcome::Removed)
            .unwrap_or(false);
        STATE[c].store(2, Ordering::Release);
        crate::yarm_log!(
            "D2SW_SETTLED class={} cpu={} outgoing={} current={} outgoing_blocked_in_class={} phantom_withdrawn={} status={:?}",
            class_name(class),
            cpu.0,
            out,
            current,
            blocked_in_class,
            u8::from(withdrawn),
            status
        );
    }
}
