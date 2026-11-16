// SCHED-LOCK2A old-path demonstration (scratch, never committed): the two regressions that use
// only pre-SCHEDULER-LOCK2A seams, copied verbatim from src/kernel/sched_lock2a_tests/tests.rs.
use std::sync::Barrier;
use std::vec::Vec;
use crate::kernel::boot::Bootstrap;
use crate::kernel::ipc::ThreadId;
use crate::kernel::scheduler::CpuId;
use crate::kernel::task::TaskClass;
use crate::runtime::SharedKernel;
fn kernel2() -> SharedKernel {
    let k = SharedKernel::new(Bootstrap::init().expect("init"));
    k.with(|s| s.bring_up_cpu(CpuId(1)).expect("cpu1 online"));
    k
}

fn register(k: &SharedKernel, tids: impl IntoIterator<Item = u64>) {
    k.with(|s| {
        for tid in tids {
            s.register_task_with_class(tid, TaskClass::App)
                .expect("register");
        }
    });
}

fn queued_on(k: &SharedKernel, cpu: CpuId, tid: u64) -> bool {
    k.with_cpu_sched_split(cpu, |c| c.contains_or_current(ThreadId(tid)))
        .unwrap_or(false)
}

fn home(k: &SharedKernel, tid: u64) -> Option<CpuId> {
    k.with(|s| s.task_home_cpu(tid))
}

/// A homed task named for another CPU is refused, and nothing anywhere changes: not the target
/// queue, not the home queue, not the home record. On the old code the pinned plan overrode the
/// home and the task was queued on CPU 0 while its home stayed CPU 1 — this test fails there.
#[test]
fn sl2a_an_off_home_pinned_enqueue_is_refused_with_no_partial_placement() {
    let k = kernel2();
    register(&k, [91_001]);
    assert_eq!(k.enqueue_on_cpu_split(CpuId(1), 91_001), Ok(CpuId(1)));
    assert_eq!(home(&k, 91_001), Some(CpuId(1)));
    let cpu0_before = k.runnable_count_on_cpu_split_read(CpuId(0));
    let cpu1_before = k.runnable_count_on_cpu_split_read(CpuId(1));
    assert!(
        k.enqueue_on_cpu_split(CpuId(0), 91_001).is_err(),
        "an explicit placement off the home must be refused"
    );
    assert_eq!(k.runnable_count_on_cpu_split_read(CpuId(0)), cpu0_before);
    assert_eq!(k.runnable_count_on_cpu_split_read(CpuId(1)), cpu1_before);
    assert!(!queued_on(&k, CpuId(0), 91_001));
    assert!(queued_on(&k, CpuId(1), 91_001));
    assert_eq!(
        home(&k, 91_001),
        Some(CpuId(1)),
        "the home record is unchanged"
    );
}

/// Two CPUs racing to place a homeless task FIRST, on different CPUs: exactly one wins, its CPU
/// becomes the home, and the task is placed there and nowhere else. On the old code both pinned
/// plans were committed (the second ignored the home the first recorded), so the task could be
/// queued on both CPUs.
#[test]
fn sl2a_competing_first_placements_record_exactly_one_home() {
    let k = kernel2();
    let tids: Vec<u64> = (92_000..92_200).collect();
    register(&k, tids.iter().copied());
    for &tid in &tids {
        let barrier = Barrier::new(2);
        let (a, b) = std::thread::scope(|s| {
            let ha = s.spawn(|| {
                barrier.wait();
                k.enqueue_on_cpu_split(CpuId(0), tid)
            });
            let hb = s.spawn(|| {
                barrier.wait();
                k.enqueue_on_cpu_split(CpuId(1), tid)
            });
            (ha.join().expect("a"), hb.join().expect("b"))
        });
        let won: Vec<CpuId> = [a, b].into_iter().filter_map(Result::ok).collect();
        assert_eq!(won.len(), 1, "tid {tid}: exactly one first placement wins");
        let h = home(&k, tid).expect("a home was recorded");
        assert_eq!(won[0], h, "tid {tid}: the winner's CPU is the home");
        assert!(queued_on(&k, h, tid));
        let other = CpuId(1 - h.0);
        assert!(!queued_on(&k, other, tid), "tid {tid}: placed off its home");
        // Keep both 64-slot queues from filling over the 200 rounds.
        let _ = k.with_cpu_sched_split(h, |c| c.withdraw_queued_tid(ThreadId(tid)));
    }
}

