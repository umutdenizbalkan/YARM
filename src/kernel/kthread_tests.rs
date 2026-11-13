// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD1 hosted tests. The bodies below are genuine kernel threads: each runs on its own pool
//! stack, entered and left through the real x86_64 switch, and every scheduling operation goes
//! through the production owners (the per-CPU 1L, the run queues, the acceptance-filtered
//! selection). The test thread plays the dispatcher of one CPU.

use super::*;
use crate::kernel::boot::Bootstrap;
use crate::kernel::scheduler::{AcceptedSelection, DispatchSelection};
use std::sync::Mutex;
use std::vec::Vec;

/// A CPU no other hosted test drives, so its run queue and hosted notification counter are ours.
const CPU: CpuId = CpuId(5);
const OTHER: CpuId = CpuId(6);

fn kernel() -> SharedKernel {
    let k = SharedKernel::new(Bootstrap::init().expect("init"));
    k.with_all_cpus_sched_split(|all| {
        let present = all.present_cpu_bitmap() | (1 << CPU.0) | (1 << OTHER.0);
        all.set_present_cpu_bitmap(present);
        all.bring_up_cpu(CPU).expect("cpu online");
        all.bring_up_cpu(OTHER).expect("cpu online");
    });
    k
}

/// The dispatcher of `cpu`: select through the acceptance filter (admitting kernel threads, as
/// the drain owner does) and run each selected kernel thread until it leaves the CPU. Returns the
/// number of kernel-thread dispatches.
fn run_cpu(k: &SharedKernel, cpu: CpuId) -> usize {
    let mut n = 0;
    loop {
        let sel = k
            .with_cpu_sched_split(cpu, |c| {
                c.dispatch_next_accepted_selection(|t| k.kthreads.admit_dispatch(cpu, t))
            })
            .expect("cpu");
        match sel {
            AcceptedSelection::Selected(DispatchSelection::Dequeued { tid })
                if is_kthread_tid(tid.0) =>
            {
                let _ = k.kthreads.dispatch_selected(cpu, tid);
                n += 1;
            }
            _ => return n,
        }
        assert!(n < 100_000, "dispatcher did not settle");
    }
}

fn queued(k: &SharedKernel, cpu: CpuId) -> Vec<u64> {
    let mut v = Vec::new();
    k.with_all_cpus_sched_split(|all| all.for_each_queued_on(cpu, |_, t| v.push(t.0)));
    v
}

fn current(k: &SharedKernel, cpu: CpuId) -> Option<u64> {
    k.with_cpu_sched_split(cpu, |c| c.current_tid().map(|t| t.0))
        .expect("cpu")
}

fn nop(_ctx: &KThreadCtx, _arg: usize) {}

// ── identity ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn ids_are_tagged_and_incarnation_exact() {
    let a = KThreadId::encode(3, 7);
    assert!(is_kthread_tid(a.raw()));
    assert_eq!((a.slot(), a.generation()), (3, 7));
    assert_eq!(KThreadId::from_raw(a.raw()), Some(a));
    assert_ne!(
        KThreadId::encode(3, 8),
        a,
        "another incarnation of the same slot"
    );
    for user in [0u64, 1, 2, 4096, u32::MAX as u64] {
        assert!(
            !is_kthread_tid(user),
            "user tid {user} is never a kernel thread"
        );
        assert_eq!(KThreadId::from_raw(user), None);
    }
    assert_eq!(
        KThreadId::from_raw(TID_TAG | KTHREAD_CAPACITY as u64),
        None,
        "a slot outside the pool names nothing"
    );
}

// ── creation, exhaustion, rollback, reuse ────────────────────────────────────────────────────

#[test]
fn creation_is_bounded_and_exhaustion_changes_nothing() {
    let k = kernel();
    let pool = &k.kthreads;
    let mut ids = Vec::new();
    for _ in 0..KTHREAD_CAPACITY {
        ids.push(
            pool.create(nop, 0, CPU, TaskPriority::Normal)
                .expect("create"),
        );
    }
    assert_eq!(pool.free_slots(), 0);
    let before = queued(&k, CPU);
    assert_eq!(
        pool.create(nop, 0, CPU, TaskPriority::Normal),
        Err(KCreateError::Exhausted)
    );
    assert_eq!(queued(&k, CPU), before, "a refused creation queues nothing");
    assert_eq!(pool.counters().exhausted, 1);
    for id in &ids {
        assert_eq!(pool.state_of(*id), Some(KState::Ready));
    }
    // All of them run and exit; every slot comes back.
    assert_eq!(run_cpu(&k, CPU), KTHREAD_CAPACITY);
    assert_eq!(pool.free_slots(), KTHREAD_CAPACITY);
    assert_eq!(pool.counters().reclaimed, KTHREAD_CAPACITY as u64);
    assert!(queued(&k, CPU).is_empty());
    assert_eq!(current(&k, CPU), None);
}

#[test]
fn refused_placement_rolls_back_and_its_incarnation_never_becomes_valid() {
    let k = kernel();
    let pool = &k.kthreads;
    let offline = CpuId(9);
    assert!(matches!(
        pool.create(nop, 0, offline, TaskPriority::Normal),
        Err(KCreateError::Placement(SchedulerError::CpuOffline))
    ));
    assert_eq!(
        pool.create(nop, 0, CpuId(MAX_CPUS as u8), TaskPriority::Normal),
        Err(KCreateError::InvalidCpu)
    );
    assert_eq!(pool.free_slots(), KTHREAD_CAPACITY, "the slot went back");
    assert_eq!(pool.counters().rolled_back, 1);
    // The rolled-back incarnation was slot 0 generation 1; the next creation takes the same slot
    // with a NEW generation.
    let next = pool
        .create(nop, 0, CPU, TaskPriority::Normal)
        .expect("create");
    assert_eq!(next.slot(), 0);
    assert_eq!(next.generation(), 2);
    assert_eq!(pool.state_of(KThreadId::encode(0, 1)), None);
    assert_eq!(pool.wake(KThreadId::encode(0, 1)), KWake::Stale);
    run_cpu(&k, CPU);
}

#[test]
fn a_reused_slot_is_a_new_incarnation_and_stale_ids_do_not_reach_it() {
    let k = kernel();
    let pool = &k.kthreads;
    let first = pool
        .create(nop, 0, CPU, TaskPriority::Normal)
        .expect("create");
    run_cpu(&k, CPU);
    assert_eq!(pool.state_of(first), None, "the slot moved on");
    let second = pool
        .create(waiter, 0, CPU, TaskPriority::Normal)
        .expect("create");
    assert_eq!(second.slot(), first.slot());
    assert_ne!(second.generation(), first.generation());
    run_cpu(&k, CPU);
    assert_eq!(pool.state_of(second), Some(KState::Blocked));
    // The old id is stale: the replacement stays parked and is not queued.
    assert_eq!(pool.wake(first), KWake::Stale);
    assert_eq!(pool.state_of(second), Some(KState::Blocked));
    assert!(queued(&k, CPU).is_empty());
    assert_eq!(pool.wake(second), KWake::Enqueued);
    run_cpu(&k, CPU);
    assert_eq!(pool.state_of(second), None);
}

/// Parks once on a private queue and exits when woken.
fn waiter(ctx: &KThreadCtx, _arg: usize) {
    static Q: KWaitQueue = KWaitQueue::new();
    let woken = core::cell::Cell::new(false);
    let _ = Q.wait_until(ctx, || woken.replace(true));
}

// ── entry, yield, private stacks ─────────────────────────────────────────────────────────────

struct Alternation {
    trace: Mutex<Vec<u64>>,
    rounds: usize,
}

fn alternator(ctx: &KThreadCtx, arg: usize) {
    // SAFETY: the test keeps the fixture alive until every thread exited.
    let t = unsafe { &*(arg as *const Alternation) };
    let seed = ctx.id().raw();
    let mut private = [0u64; 256];
    for (i, w) in private.iter_mut().enumerate() {
        *w = seed.rotate_left(i as u32) ^ i as u64;
    }
    let sp_at_entry = arch::current_sp();
    for _ in 0..t.rounds {
        t.trace.lock().unwrap().push(seed);
        ctx.yield_now().expect("yield");
        assert_eq!(arch::current_sp() > sp_at_entry - 4096, true);
        for (i, w) in private.iter().enumerate() {
            assert_eq!(
                *w,
                seed.rotate_left(i as u32) ^ i as u64,
                "private stack intact"
            );
        }
    }
}

#[test]
fn two_threads_alternate_on_one_cpu_and_keep_their_stacks() {
    let k = kernel();
    let fixture = Alternation {
        trace: Mutex::new(Vec::new()),
        rounds: 50,
    };
    let arg = &fixture as *const _ as usize;
    let a = k
        .kthreads
        .create(alternator, arg, CPU, TaskPriority::Normal)
        .expect("a");
    let b = k
        .kthreads
        .create(alternator, arg, CPU, TaskPriority::Normal)
        .expect("b");
    let dispatches = run_cpu(&k, CPU);
    // 50 yields each, plus the final run to exit.
    assert_eq!(dispatches, 2 * 51);
    let trace = fixture.trace.lock().unwrap().clone();
    assert_eq!(trace.len(), 100);
    for (i, who) in trace.iter().enumerate() {
        let expect = if i % 2 == 0 { a.raw() } else { b.raw() };
        assert_eq!(*who, expect, "strict alternation at step {i}");
    }
    assert_eq!(k.kthreads.dispatch_count(a), 51);
    assert_eq!(k.kthreads.state_of(a), None);
    assert_eq!(k.kthreads.state_of(b), None);
    assert_eq!(k.kthreads.free_slots(), KTHREAD_CAPACITY);
}

#[test]
fn a_returning_entry_reaches_exit_and_the_dispatcher_reclaims_the_stack() {
    let k = kernel();
    let id = k
        .kthreads
        .create(nop, 0, CPU, TaskPriority::High)
        .expect("create");
    // Fresh: only the first frame (at most the return slot under the 16-byte-aligned top).
    assert!(k.kthreads.stack_high_water(id) <= 16, "freshly painted");
    assert_eq!(run_cpu(&k, CPU), 1);
    assert_eq!(
        k.kthreads.resident_on(CPU),
        None,
        "residency cleared after the switch away"
    );
    assert_eq!(k.kthreads.counters().reclaimed, 1);
    assert_eq!(current(&k, CPU), None);
}

fn deep(ctx: &KThreadCtx, _arg: usize) {
    let mut buf = [0u8; 8192];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = i as u8;
    }
    core::hint::black_box(&buf);
    let _ = ctx.yield_now();
}

#[test]
fn stack_use_is_measured_from_the_paint() {
    let k = kernel();
    let id = k
        .kthreads
        .create(deep, 0, CPU, TaskPriority::Normal)
        .expect("create");
    // Run until it yields once (it is then Ready again), and measure while it is parked mid-life.
    let sel = k
        .with_cpu_sched_split(CPU, |c| {
            c.dispatch_next_accepted_selection(|t| k.kthreads.admit_dispatch(CPU, t))
        })
        .expect("cpu");
    let AcceptedSelection::Selected(DispatchSelection::Dequeued { tid }) = sel else {
        panic!("selected {sel:?}");
    };
    assert_eq!(k.kthreads.dispatch_selected(CPU, tid), KSwitchReason::Yield);
    let used = k.kthreads.stack_high_water(id);
    assert!(
        used >= 8192 && used < KTHREAD_STACK_BYTES,
        "observed depth {used}"
    );
    run_cpu(&k, CPU);
}

// ── waiting: the races ───────────────────────────────────────────────────────────────────────

struct Gate {
    q: KWaitQueue,
    open: AtomicBool,
    blocked: AtomicU32,
    done: AtomicU32,
    /// When set, the first condition evaluation wakes the waiter itself — a wake landing between
    /// the failed check (and registration) and the park.
    wake_inside_check: AtomicBool,
}

impl Gate {
    fn new() -> Self {
        Self {
            q: KWaitQueue::new(),
            open: AtomicBool::new(false),
            blocked: AtomicU32::new(0),
            done: AtomicU32::new(0),
            wake_inside_check: AtomicBool::new(false),
        }
    }
}

fn gate_waiter(ctx: &KThreadCtx, arg: usize) {
    // SAFETY: the test keeps the gate alive until every thread exited.
    let g = unsafe { &*(arg as *const Gate) };
    let pool = ctx.pool();
    let me = ctx.id();
    let n =
        g.q.wait_until(ctx, || {
            if g.wake_inside_check.swap(false, Ordering::AcqRel) {
                assert_eq!(
                    pool.wake(me),
                    KWake::Pending,
                    "not parked yet: the wake is kept"
                );
                return false;
            }
            g.open.load(Ordering::Acquire)
        })
        .expect("wait");
    g.blocked.fetch_add(n, Ordering::AcqRel);
    g.done.fetch_add(1, Ordering::AcqRel);
}

#[test]
fn wake_before_registration_does_not_block() {
    let k = kernel();
    let g = Gate::new();
    g.open.store(true, Ordering::Release);
    k.kthreads
        .create(
            gate_waiter,
            &g as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    assert_eq!(g.q.wake_all(&k.kthreads), 0, "nobody registered yet");
    run_cpu(&k, CPU);
    assert_eq!(g.done.load(Ordering::Acquire), 1);
    assert_eq!(g.blocked.load(Ordering::Acquire), 0);
    assert_eq!(g.q.registered(), 0);
}

#[test]
fn wake_between_check_and_park_is_not_lost() {
    let k = kernel();
    let g = Gate::new();
    g.wake_inside_check.store(true, Ordering::Release);
    let id = k
        .kthreads
        .create(
            gate_waiter,
            &g as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    // The pending wake makes the park return at once; the condition is still false, so the
    // second park blocks for real.
    run_cpu(&k, CPU);
    assert_eq!(k.kthreads.counters().early_wakes, 1);
    assert_eq!(k.kthreads.state_of(id), Some(KState::Blocked));
    g.open.store(true, Ordering::Release);
    assert_eq!(g.q.wake_all(&k.kthreads), 1);
    run_cpu(&k, CPU);
    assert_eq!(g.done.load(Ordering::Acquire), 1);
    assert_eq!(g.blocked.load(Ordering::Acquire), 1);
}

#[test]
fn wake_after_block_and_duplicate_wakes_give_one_placement() {
    let k = kernel();
    let g = Gate::new();
    let id = k
        .kthreads
        .create(
            gate_waiter,
            &g as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    run_cpu(&k, CPU);
    assert_eq!(k.kthreads.state_of(id), Some(KState::Blocked));
    assert!(
        queued(&k, CPU).is_empty(),
        "a blocked thread is in no queue"
    );
    // A spurious wake (condition still false): it runs, re-checks, and parks again.
    assert_eq!(k.kthreads.wake(id), KWake::Enqueued);
    assert_eq!(
        k.kthreads.wake(id),
        KWake::Pending,
        "already ready: no second placement"
    );
    assert_eq!(queued(&k, CPU), std::vec![id.raw()]);
    run_cpu(&k, CPU);
    // The pending wake was consumed by one early return; the thread is parked again.
    assert_eq!(k.kthreads.state_of(id), Some(KState::Blocked));
    g.open.store(true, Ordering::Release);
    g.q.wake_all(&k.kthreads);
    g.q.wake_all(&k.kthreads);
    assert_eq!(queued(&k, CPU), std::vec![id.raw()]);
    run_cpu(&k, CPU);
    assert_eq!(g.done.load(Ordering::Acquire), 1);
}

#[test]
fn cross_cpu_wake_enqueues_on_the_home_cpu_and_notifies_it() {
    let k = kernel();
    let g = Gate::new();
    let id = k
        .kthreads
        .create(
            gate_waiter,
            &g as *const _ as usize,
            OTHER,
            TaskPriority::Normal,
        )
        .expect("create");
    run_cpu(&k, OTHER);
    assert_eq!(k.kthreads.state_of(id), Some(KState::Blocked));
    let notified = crate::arch::hal::enqueues_notified(OTHER);
    g.open.store(true, Ordering::Release);
    // The waker is "another CPU": it touches only the home CPU's 1L and queue.
    assert_eq!(g.q.wake_all(&k.kthreads), 1);
    assert_eq!(queued(&k, OTHER), std::vec![id.raw()]);
    assert!(queued(&k, CPU).is_empty());
    assert!(crate::arch::hal::enqueues_notified(OTHER) > notified);
    // The home CPU's next dispatch resumes exactly this incarnation.
    assert_eq!(run_cpu(&k, CPU), 0, "another CPU cannot run it");
    assert_eq!(run_cpu(&k, OTHER), 1);
    assert_eq!(g.done.load(Ordering::Acquire), 1);
}

fn wake_self_then_exit(ctx: &KThreadCtx, _arg: usize) {
    assert_eq!(ctx.pool().wake(ctx.id()), KWake::Pending);
}

#[test]
fn an_exited_thread_s_outstanding_wake_does_not_reach_its_replacement() {
    let k = kernel();
    let old = k
        .kthreads
        .create(wake_self_then_exit, 0, CPU, TaskPriority::Normal)
        .expect("create");
    run_cpu(&k, CPU);
    // A queue still holding the old incarnation's registration.
    let g = Gate::new();
    let replacement = k
        .kthreads
        .create(
            gate_waiter,
            &g as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    assert_eq!(replacement.slot(), old.slot());
    run_cpu(&k, CPU);
    // The old pending wake was not inherited: the replacement blocked.
    assert_eq!(k.kthreads.state_of(replacement), Some(KState::Blocked));
    assert_eq!(g.blocked.load(Ordering::Acquire), 0);
    assert_eq!(k.kthreads.counters().early_wakes, 0);
    assert_eq!(k.kthreads.wake(old), KWake::Stale);
    assert_eq!(k.kthreads.state_of(replacement), Some(KState::Blocked));
    g.open.store(true, Ordering::Release);
    g.q.wake_all(&k.kthreads);
    run_cpu(&k, CPU);
    assert_eq!(g.done.load(Ordering::Acquire), 1);
}

// ── switch safety ────────────────────────────────────────────────────────────────────────────

struct Refusals {
    seen: Mutex<Vec<KSwitchRefusal>>,
}

fn refusing(ctx: &KThreadCtx, arg: usize) {
    // SAFETY: the test keeps the fixture alive.
    let r = unsafe { &*(arg as *const Refusals) };
    let state = || ctx.pool().state_of(ctx.id());
    {
        let _m = ctx.masked();
        let e = ctx.yield_now().expect_err("switch under a masked section");
        r.seen.lock().unwrap().push(e);
        assert_eq!(state(), Some(KState::Running), "a refusal changes nothing");
        let q = KWaitQueue::new();
        assert_eq!(
            q.wait_until(ctx, || false),
            Err(KWaitError::Refused(KSwitchRefusal::IrqMasked))
        );
        assert_eq!(q.registered(), 0, "a refused wait leaves no registration");
    }
    {
        let was = arch::irq_disable_save();
        let e = ctx
            .sched_point()
            .expect_err("switch with interrupts masked");
        r.seen.lock().unwrap().push(e);
        arch::irq_restore(was);
    }
    ctx.yield_now().expect("clean again");
}

#[test]
fn a_switch_under_a_guard_or_masked_is_refused_by_name() {
    let k = kernel();
    let r = Refusals {
        seen: Mutex::new(Vec::new()),
    };
    k.kthreads
        .create(refusing, &r as *const _ as usize, CPU, TaskPriority::Normal)
        .expect("create");
    assert_eq!(run_cpu(&k, CPU), 2);
    assert_eq!(
        *r.seen.lock().unwrap(),
        std::vec![KSwitchRefusal::IrqMasked, KSwitchRefusal::IrqMasked]
    );
    assert_eq!(k.kthreads.counters().switch_refused, 3);
}

#[test]
fn a_context_outside_its_thread_is_refused() {
    let k = kernel();
    let id = k
        .kthreads
        .create(nop, 0, CPU, TaskPriority::Normal)
        .expect("create");
    // SAFETY-free forgery attempt: a context naming a queued, not running, thread.
    let forged = KThreadCtx {
        pool: unsafe { &*(&k.kthreads as *const KThreadPool) },
        id,
        home: CPU,
        masked_depth: Cell::new(0),
        _not_send: PhantomData,
    };
    assert_eq!(
        forged.yield_now(),
        Err(KSwitchRefusal::IrqMasked),
        "dispatcher context is masked"
    );
    arch::irq_enable();
    assert_eq!(forged.yield_now(), Err(KSwitchRefusal::NotResident));
    let _ = arch::irq_disable_save();
    assert_eq!(k.kthreads.state_of(id), Some(KState::Ready));
    assert_eq!(queued(&k, CPU), std::vec![id.raw()]);
    run_cpu(&k, CPU);
}

fn stack_mismatch(ctx: &KThreadCtx, _arg: usize) {
    // Another slot's context for this same CPU: resident is THIS thread, so the check refuses.
    let other = KThreadCtx {
        pool: ctx.pool,
        id: KThreadId::encode((ctx.id().slot() + 1) % KTHREAD_CAPACITY, 1),
        home: ctx.home,
        masked_depth: Cell::new(0),
        _not_send: PhantomData,
    };
    assert_eq!(other.yield_now(), Err(KSwitchRefusal::NotResident));
}

#[test]
fn only_the_resident_thread_can_switch() {
    let k = kernel();
    k.kthreads
        .create(stack_mismatch, 0, CPU, TaskPriority::Normal)
        .expect("create");
    assert_eq!(run_cpu(&k, CPU), 1);
}

// ── deferred rescheduling and interrupt authentication ───────────────────────────────────────

struct Resched {
    yielded: Mutex<Vec<bool>>,
    auth: Mutex<Vec<Result<(CpuId, KThreadId), KIrqRefusal>>>,
}

fn resched_body(ctx: &KThreadCtx, arg: usize) {
    // SAFETY: the test keeps the fixture alive.
    let f = unsafe { &*(arg as *const Resched) };
    let pool = ctx.pool();
    let probe = 0u64;
    let here = &probe as *const u64 as usize;
    // The interrupt entry's checks, made from a frame address on this stack.
    let mut auth = f.auth.lock().unwrap();
    auth.push(pool.authenticate_interrupt(here, ctx.home()));
    auth.push(pool.authenticate_interrupt(here, OTHER));
    auth.push(pool.authenticate_interrupt(0x1000, ctx.home()));
    drop(auth);
    f.yielded
        .lock()
        .unwrap()
        .push(ctx.sched_point().expect("point"));
    // An interrupt left a reschedule: the next point honours it.
    pool.note_resched_interrupt(ctx.home(), true);
    f.yielded
        .lock()
        .unwrap()
        .push(ctx.sched_point().expect("point"));
    // An announcement-free arrival (consumed by an earlier one) owes nothing.
    pool.note_resched_interrupt(ctx.home(), false);
    f.yielded
        .lock()
        .unwrap()
        .push(ctx.sched_point().expect("point"));
}

#[test]
fn a_pending_reschedule_is_honoured_at_the_next_scheduling_point_only() {
    let k = kernel();
    let f = Resched {
        yielded: Mutex::new(Vec::new()),
        auth: Mutex::new(Vec::new()),
    };
    let id = k
        .kthreads
        .create(
            resched_body,
            &f as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    assert_eq!(run_cpu(&k, CPU), 2, "exactly one honoured reschedule");
    assert_eq!(*f.yielded.lock().unwrap(), std::vec![false, true, false]);
    let auth = f.auth.lock().unwrap();
    assert_eq!(auth[0], Ok((CPU, id)));
    assert_eq!(auth[1], Err(KIrqRefusal::CpuMismatch));
    assert_eq!(auth[2], Err(KIrqRefusal::NotKernelThreadStack));
    let c = k.kthreads.cpu_counters(CPU);
    assert_eq!((c.resched_interrupts, c.resched_honoured), (2, 1));
    // After the thread is gone, an interrupt "on its stack" is not a kernel-thread interrupt.
    let (low, _) = k.kthreads.stack_bounds(id.slot());
    assert_eq!(
        k.kthreads.authenticate_interrupt(low + 64, CPU),
        Err(KIrqRefusal::NotRunning)
    );
}

// ── user/kernel separation ───────────────────────────────────────────────────────────────────

#[test]
fn unfiltered_selection_never_returns_a_kernel_thread() {
    let k = kernel();
    let id = k
        .kthreads
        .create(nop, 0, CPU, TaskPriority::High)
        .expect("create");
    let user = ThreadId(4242);
    k.with_cpu_sched_split(CPU, |c| c.enqueue_with_priority(user, TaskPriority::Low))
        .expect("cpu")
        .expect("enqueue");
    k.with_cpu_sched_split(CPU, |c| {
        assert_eq!(
            c.peek_next_runnable(),
            Some(user),
            "the kernel thread is skipped in place"
        );
        assert_eq!(c.dispatch_next(), Some(user));
        assert_eq!(c.block_current(), Some(user));
        assert_eq!(
            c.dispatch_next(),
            None,
            "only the kernel thread is left: nothing for it"
        );
    })
    .expect("cpu");
    assert_eq!(queued(&k, CPU), std::vec![id.raw()], "kept its place");
    // The filtered selection without the kernel-thread admission skips it too.
    let sel = k
        .with_cpu_sched_split(CPU, |c| c.dispatch_next_accepted_selection(|_| false))
        .expect("cpu");
    assert_eq!(sel, AcceptedSelection::NoneAcceptable { examined: 1 });
    run_cpu(&k, CPU);
}

#[test]
fn a_kernel_thread_has_no_task_control_block_and_no_user_root() {
    let k = kernel();
    let id = k
        .kthreads
        .create(nop, 0, CPU, TaskPriority::Normal)
        .expect("create");
    assert_eq!(k.task_asid_option_split_read(id.raw()), None);
    let tcb_seen =
        k.with_task_tcbs_split_mut(|tcbs| tcbs.iter().flatten().any(|t| t.tid.0 == id.raw()));
    assert!(!tcb_seen);
    run_cpu(&k, CPU);
}

#[test]
fn the_drain_owner_runs_kernel_threads_and_reports_only_user_or_idle() {
    let k = kernel();
    let fixture = Alternation {
        trace: Mutex::new(Vec::new()),
        rounds: 3,
    };
    let arg = &fixture as *const _ as usize;
    k.kthreads
        .create(alternator, arg, CPU, TaskPriority::Normal)
        .expect("a");
    k.kthreads
        .create(alternator, arg, CPU, TaskPriority::Normal)
        .expect("b");
    // The production drain owner, under a live authority for this CPU. Another hosted test may
    // retire this CPU's window concurrently; that refusal touches nothing, so retry.
    let mut outcome = None;
    for _ in 0..1000 {
        let authority = crate::runtime::DispatchAuthority::live_for_test(CPU);
        let got = k.queue_advance_acquire_incoming_split(authority, "kthread1_test", |s, a| {
            s.queue_advance_select_step_split(a, "kthread1_test")
        });
        if !matches!(got, crate::runtime::DispatchAcquire::NoAuthority { .. }) {
            outcome = Some(got);
            break;
        }
    }
    assert!(
        matches!(outcome, Some(crate::runtime::DispatchAcquire::Idle)),
        "{outcome:?}"
    );
    assert_eq!(fixture.trace.lock().unwrap().len(), 6);
    assert_eq!(k.kthreads.free_slots(), KTHREAD_CAPACITY);
}

/// Balanced placement measures user tasks only: a queued kernel thread does not raise the load it
/// publishes, so it never steers where a user task's permanent home lands.
#[test]
fn balanced_placement_load_counts_user_tasks_only() {
    let k = kernel();
    let before = k.sched_domain().load_mirror(CPU.0 as usize);
    let id = k
        .kthreads
        .create(nop, 0, CPU, TaskPriority::Normal)
        .expect("create");
    assert_eq!(queued(&k, CPU), std::vec![id.raw()]);
    assert_eq!(
        k.sched_domain().load_mirror(CPU.0 as usize),
        before,
        "a queued kernel thread is not placement load"
    );
    k.with_cpu_sched_split(CPU, |c| {
        c.enqueue_with_priority(ThreadId(4343), TaskPriority::Low)
    })
    .expect("cpu")
    .expect("enqueue");
    assert_eq!(k.sched_domain().load_mirror(CPU.0 as usize), before + 1);
    assert_eq!(run_cpu(&k, CPU), 1, "the kernel thread still runs");
}

// ── KTHREAD1-ACCEPTANCE §1: a wake is never refused, so no waiter is left without an owner ────
//
// On the delivered base a wake into a full destination queue was refused (`QueueFull`), and
// `wake_all` had already taken the registration: the thread stayed Blocked with nobody
// responsible for waking it, and freeing capacity resumed nothing (reproduced on `40132e1a`).
// The run queue now reserves room for every kernel thread (`MAX_QUEUED_PER_PRIORITY`).

/// Fill `cpu`'s Normal queue with user entries until a user push is refused. Returns how many.
fn fill_with_users(k: &SharedKernel, cpu: CpuId, base: u64) -> u64 {
    let mut n = 0;
    loop {
        let r = k
            .with_cpu_sched_split(cpu, |c| {
                c.enqueue_with_priority(ThreadId(base + n), TaskPriority::Normal)
            })
            .expect("cpu");
        match r {
            Ok(()) => n += 1,
            Err(e) => {
                assert_eq!(e, SchedulerError::QueueFull);
                return n;
            }
        }
    }
}

fn user_push(k: &SharedKernel, cpu: CpuId, tid: u64) -> Result<(), SchedulerError> {
    k.with_cpu_sched_split(cpu, |c| {
        c.enqueue_with_priority(ThreadId(tid), TaskPriority::Normal)
    })
    .expect("cpu")
}

fn occurrences(k: &SharedKernel, cpu: CpuId, raw: u64) -> usize {
    queued(k, cpu).iter().filter(|&&t| t == raw).count()
}

#[test]
fn a_wake_into_a_user_full_queue_is_placed_and_completes_without_a_second_condition_change() {
    let k = kernel();
    let g = Gate::new();
    let id = k
        .kthreads
        .create(
            gate_waiter,
            &g as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    run_cpu(&k, CPU);
    assert_eq!(k.kthreads.state_of(id), Some(KState::Blocked));
    assert_eq!(g.q.registered(), 1);
    assert_eq!(
        fill_with_users(&k, CPU, 900_000),
        crate::kernel::scheduler::MAX_RUN_QUEUE as u64
    );
    // The condition is published once; the one wake places the thread despite the full queue.
    g.open.store(true, Ordering::Release);
    assert_eq!(g.q.wake_all(&k.kthreads), 1);
    assert_eq!(k.kthreads.state_of(id), Some(KState::Ready));
    assert_eq!(occurrences(&k, CPU, id.raw()), 1);
    assert_eq!(
        g.q.registered(),
        0,
        "the registration was consumed by a placement"
    );
    // The user limit is unchanged: a kernel-thread entry counts against it.
    assert_eq!(user_push(&k, CPU, 990_000), Err(SchedulerError::QueueFull));
    assert_eq!(run_cpu(&k, CPU), 1);
    assert_eq!(g.done.load(Ordering::Acquire), 1);
    assert_eq!(k.kthreads.state_of(id), None, "exited and reclaimed");
}

#[test]
fn every_kernel_thread_fits_beside_a_user_full_queue() {
    let k = kernel();
    // Four queues of eight waiters: the whole pool, all on one CPU at one priority.
    let gates: Vec<Gate> = (0..KTHREAD_CAPACITY / KWAIT_CAPACITY)
        .map(|_| Gate::new())
        .collect();
    let mut ids = Vec::new();
    for g in &gates {
        for _ in 0..KWAIT_CAPACITY {
            ids.push(
                k.kthreads
                    .create(
                        gate_waiter,
                        g as *const _ as usize,
                        CPU,
                        TaskPriority::Normal,
                    )
                    .expect("create"),
            );
        }
    }
    run_cpu(&k, CPU);
    assert!(
        ids.iter()
            .all(|&id| k.kthreads.state_of(id) == Some(KState::Blocked))
    );
    assert_eq!(
        fill_with_users(&k, CPU, 900_000),
        crate::kernel::scheduler::MAX_RUN_QUEUE as u64
    );
    for g in &gates {
        g.open.store(true, Ordering::Release);
        assert_eq!(g.q.wake_all(&k.kthreads), KWAIT_CAPACITY);
    }
    // 64 user entries and 32 kernel threads: the reserved bound, reached and not exceeded.
    // SCHED-TIME-FOUNDATION1 A: `MAX_QUEUED_PER_PRIORITY` also holds the one rotation slot of a
    // returning current task, unused here, so the count is named by its two parts.
    assert_eq!(
        queued(&k, CPU).len(),
        crate::kernel::scheduler::MAX_RUN_QUEUE + KTHREAD_CAPACITY
    );
    for &id in &ids {
        assert_eq!(occurrences(&k, CPU, id.raw()), 1);
    }
    assert_eq!(run_cpu(&k, CPU), KTHREAD_CAPACITY);
    assert!(
        gates
            .iter()
            .all(|g| g.done.load(Ordering::Acquire) == KWAIT_CAPACITY as u32)
    );
    assert_eq!(k.kthreads.free_slots(), KTHREAD_CAPACITY);
}

#[test]
fn repeated_and_duplicate_wakes_against_a_full_queue_give_one_placement() {
    let k = kernel();
    let g = Gate::new();
    let id = k
        .kthreads
        .create(
            gate_waiter,
            &g as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    run_cpu(&k, CPU);
    fill_with_users(&k, CPU, 900_000);
    g.open.store(true, Ordering::Release);
    assert_eq!(k.kthreads.wake(id), KWake::Enqueued);
    for _ in 0..3 {
        assert_eq!(k.kthreads.wake(id), KWake::Pending, "already placed");
    }
    g.q.wake_all(&k.kthreads);
    g.q.wake_all(&k.kthreads);
    assert_eq!(occurrences(&k, CPU, id.raw()), 1, "no duplicate placement");
    assert_eq!(run_cpu(&k, CPU), 1);
    assert_eq!(g.done.load(Ordering::Acquire), 1);
}

#[test]
fn several_waiters_with_different_outcomes_in_one_wake_all() {
    let k = kernel();
    let g = Gate::new();
    // One waiter parked on each of two CPUs, and one woken during its own check (not parked).
    let a = k
        .kthreads
        .create(
            gate_waiter,
            &g as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    let b = k
        .kthreads
        .create(
            gate_waiter,
            &g as *const _ as usize,
            OTHER,
            TaskPriority::Normal,
        )
        .expect("create");
    run_cpu(&k, CPU);
    run_cpu(&k, OTHER);
    assert_eq!(g.q.registered(), 2);
    fill_with_users(&k, CPU, 900_000);
    fill_with_users(&k, OTHER, 910_000);
    g.open.store(true, Ordering::Release);
    assert_eq!(
        g.q.wake_all(&k.kthreads),
        2,
        "both placed, each on its own home"
    );
    assert_eq!(occurrences(&k, CPU, a.raw()), 1);
    assert_eq!(occurrences(&k, OTHER, b.raw()), 1);
    assert_eq!(occurrences(&k, CPU, b.raw()), 0);
    assert_eq!(run_cpu(&k, CPU) + run_cpu(&k, OTHER), 2);
    assert_eq!(g.done.load(Ordering::Acquire), 2);
}

#[test]
fn a_wake_during_publication_is_kept_with_a_full_queue() {
    let k = kernel();
    let g = Gate::new();
    g.wake_inside_check.store(true, Ordering::Release);
    fill_with_users(&k, CPU, 900_000);
    let id = k
        .kthreads
        .create(
            gate_waiter,
            &g as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create beside a user-full queue");
    run_cpu(&k, CPU);
    assert_eq!(k.kthreads.counters().early_wakes, 1);
    assert_eq!(k.kthreads.state_of(id), Some(KState::Blocked));
    g.open.store(true, Ordering::Release);
    assert_eq!(g.q.wake_all(&k.kthreads), 1);
    run_cpu(&k, CPU);
    assert_eq!(g.done.load(Ordering::Acquire), 1);
}

#[test]
fn a_stale_wake_after_reuse_is_refused_with_a_full_queue() {
    let k = kernel();
    let old = k
        .kthreads
        .create(nop, 0, CPU, TaskPriority::Normal)
        .expect("create");
    run_cpu(&k, CPU);
    fill_with_users(&k, CPU, 900_000);
    let g = Gate::new();
    let replacement = k
        .kthreads
        .create(
            gate_waiter,
            &g as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    assert_eq!(replacement.slot(), old.slot());
    run_cpu(&k, CPU);
    assert_eq!(k.kthreads.wake(old), KWake::Stale);
    assert_eq!(
        occurrences(&k, CPU, replacement.raw()),
        0,
        "the stale wake placed nothing"
    );
    assert_eq!(k.kthreads.state_of(replacement), Some(KState::Blocked));
    g.open.store(true, Ordering::Release);
    assert_eq!(g.q.wake_all(&k.kthreads), 1);
    run_cpu(&k, CPU);
    assert_eq!(g.done.load(Ordering::Acquire), 1);
}

#[test]
fn the_user_limit_is_unchanged_and_kernel_threads_have_reserved_room() {
    let k = kernel();
    // Kernel-thread ids enqueued directly (the scheduler sees only ids).
    let kt = |i: usize| ThreadId(KThreadId::encode(i, 1).raw());
    for i in 0..10 {
        k.with_cpu_sched_split(CPU, |c| {
            c.enqueue_with_priority(kt(i), TaskPriority::Normal)
        })
        .expect("cpu")
        .expect("kernel-thread entry");
    }
    // Users fill to MAX_RUN_QUEUE entries in total, kernel threads included.
    assert_eq!(
        fill_with_users(&k, CPU, 900_000),
        (crate::kernel::scheduler::MAX_RUN_QUEUE - 10) as u64
    );
    for i in 10..KTHREAD_CAPACITY {
        k.with_cpu_sched_split(CPU, |c| {
            c.enqueue_with_priority(kt(i), TaskPriority::Normal)
        })
        .expect("cpu")
        .expect("reserved room");
    }
    assert_eq!(
        queued(&k, CPU).len(),
        crate::kernel::scheduler::MAX_RUN_QUEUE - 10 + KTHREAD_CAPACITY
    );
    assert_eq!(user_push(&k, CPU, 990_000), Err(SchedulerError::QueueFull));
}

#[test]
#[should_panic(expected = "KTHREAD_WAKE_INVARIANT")]
fn a_refusal_for_a_live_blocked_thread_fails_closed() {
    let k = kernel();
    let g = Gate::new();
    let id = k
        .kthreads
        .create(
            gate_waiter,
            &g as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    run_cpu(&k, CPU);
    assert_eq!(k.kthreads.state_of(id), Some(KState::Blocked));
    // Break the invariant the reservation relies on: the home stops accepting work.
    k.with_all_cpus_sched_split(|all| all.set_cpu_wake_only(CPU, true))
        .expect("wake-only");
    g.open.store(true, Ordering::Release);
    let _ = g.q.wake_all(&k.kthreads);
}

// ── KTHREAD1-ACCEPTANCE §5: a user deadline is delivered while kernel threads keep running ─────

struct Ticker {
    /// The tick count at which a ticker first saw task 0 `Runnable` (0 = never, during the run).
    seen_at: AtomicU32,
    rounds: AtomicU32,
}

fn task0_runnable(shared: &SharedKernel) -> bool {
    shared.with_task_tcbs_split_mut(|tcbs| {
        tcbs.iter()
            .flatten()
            .find(|t| t.tid.0 == 0)
            .is_some_and(|t| t.status == crate::kernel::task::TaskStatus::Runnable)
    })
}

/// A kernel thread that drives the clock (as CPU 0's timer would) and yields every round, so the
/// CPU never becomes idle while it runs.
fn ticking(ctx: &KThreadCtx, arg: usize) {
    // SAFETY: the test keeps the ticker alive until every thread exited.
    let t = unsafe { &*(arg as *const Ticker) };
    for round in 1..=40u32 {
        let _ = ctx.shared().scheduler_tick_split_mut(CpuId(0));
        ctx.yield_now().expect("yield");
        if t.seen_at.load(Ordering::Acquire) == 0 && task0_runnable(ctx.shared()) {
            t.seen_at.store(round, Ordering::Release);
        }
        t.rounds.fetch_add(1, Ordering::AcqRel);
    }
}

#[test]
fn a_user_deadline_is_delivered_while_kernel_threads_keep_running() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("register task 1");
    state.enqueue_current_cpu(1).expect("queue task 1");
    let (_eid, _send, recv_cap) = state.create_endpoint(4).expect("endpoint");
    // The clock starts at 0; the deadline is ten ticks away.
    let deadline = 10;
    assert!(
        state
            .ipc_recv_until_deadline(recv_cap, deadline)
            .expect("recv with a deadline")
            .is_none(),
        "task 0 blocks on a real deadline"
    );
    let k = SharedKernel::new(state);
    k.with_all_cpus_sched_split(|all| {
        let present = all.present_cpu_bitmap() | (1 << CPU.0);
        all.set_present_cpu_bitmap(present);
        all.bring_up_cpu(CPU).expect("cpu online");
    });
    crate::kernel::boot::reset_ipc_timeout_scan_cursor(CPU.0 as usize);
    let t = Ticker {
        seen_at: AtomicU32::new(0),
        rounds: AtomicU32::new(0),
    };
    for _ in 0..2 {
        k.kthreads
            .create(ticking, &t as *const _ as usize, CPU, TaskPriority::Normal)
            .expect("create");
    }
    assert!(!task0_runnable(&k));
    run_cpu(&k, CPU);
    let seen = t.seen_at.load(Ordering::Acquire);
    assert!(
        seen != 0 && seen < 40,
        "the timeout must be settled at a kernel-thread boundary while they still run (seen at {seen})"
    );
    // Read through the task domain's split owner (no broad acquisition).
    let fired = k.with_task_tcbs_split_mut(|tcbs| {
        tcbs.iter()
            .flatten()
            .find(|t| t.tid.0 == 0)
            .is_some_and(|t| t.ipc_timeout_fired)
    });
    assert!(
        fired,
        "and the TimedOut fact is staged for the receive's return"
    );
}

// ── KTHREAD1-ACCEPTANCE §2: device interrupts on a running kernel thread ─────────────────────────

/// Spins until the device interrupt the test raises has been settled, then reaches its scheduling
/// point: the note left a reschedule pending, so the point yields and the thread resumes after.
fn device_spinner(ctx: &KThreadCtx, arg: usize) {
    let seen = unsafe { &*(arg as *const AtomicU32) };
    let before = ctx.pool().cpu_counters(ctx.home()).device_interrupts;
    // The "interrupt": settled through the pool's note, as the port bridges do after delivery.
    ctx.pool().note_device_interrupt(ctx.home());
    assert_eq!(
        ctx.pool().cpu_counters(ctx.home()).device_interrupts,
        before + 1
    );
    assert!(
        ctx.pool().resched_pending(ctx.home()),
        "the decision is deferred, not taken"
    );
    seen.store(1, Ordering::Release);
    assert_eq!(
        ctx.sched_point(),
        Ok(true),
        "the next scheduling point honours it"
    );
    seen.store(2, Ordering::Release);
}

#[test]
fn a_settled_device_interrupt_defers_scheduling_to_the_next_point() {
    let k = kernel();
    let seen = AtomicU32::new(0);
    k.kthreads
        .create(
            device_spinner,
            &seen as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    run_cpu(&k, CPU);
    assert_eq!(
        seen.load(Ordering::Acquire),
        2,
        "the thread resumed after its point and exited"
    );
    assert!(!k.kthreads.resched_pending(CPU));
    assert_eq!(current(&k, CPU), None);
}

fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let at = src.find(sig).unwrap_or_else(|| panic!("{sig}"));
    let rest = &src[at..];
    &rest[..rest.find("\n}\n").map_or(rest.len(), |e| e + 2)]
}

/// RISC-V: the kernel-thread vector is an entry owner of its own. It reads the claim once, after
/// authentication, only for a supervisor external interrupt, hands it to the bridge's settlement
/// (which owns the one completion) and writes no completion itself.
#[test]
fn riscv_kernel_thread_external_interrupts_reach_the_claim_and_settlement_owners() {
    const SRC: &str = include_str!("../arch/riscv64/kthread.rs");
    let h = fn_body(SRC, "extern \"C\" fn yarm_riscv64_kthread_trap(");
    assert_eq!(h.matches("claim_external_interrupt_once()").count(), 1);
    assert_eq!(SRC.matches("claim_external_interrupt_once()").count(), 1);
    let auth = h
        .find("pool.authenticate_interrupt(")
        .expect("authentication");
    let branch = h
        .find("} else if code == SUPERVISOR_EXTERNAL {")
        .expect("external branch");
    let claim = h.find("claim_external_interrupt_once()").unwrap();
    let settle = h
        .find("trap::settle_riscv_external_claim(shared, cpu, claim)")
        .expect("settle");
    let note = h
        .find("pool.note_device_interrupt(cpu)")
        .expect("deferred decision");
    assert!(auth < branch && branch < claim && claim < settle && settle < note);
    for completion in [
        "complete_external_interrupt_claim",
        "write_claim_completion",
        ".complete(",
    ] {
        assert!(
            !SRC.contains(completion),
            "the vector writes no completion: {completion}"
        );
    }
    // Every other trap on a kernel thread stays fatal.
    assert!(h.contains(
        "code != SUPERVISOR_TIMER && code != SUPERVISOR_SOFTWARE && code != SUPERVISOR_EXTERNAL"
    ));
    assert!(h.contains("kthread_fatal_halt()"));
    // And it never switches.
    for switch in ["yield_now", "sched_point", "switch("] {
        assert!(!h.contains(switch), "{switch}");
    }
}

/// AArch64: a device SPI is delivered through the bridge with the inert single-step completion,
/// and this vector's tail writes the claim's one EOIR afterwards. Private interrupts other than
/// the timer and the reschedule SGI stay fatal.
#[test]
fn aarch64_kernel_thread_device_interrupts_are_settled_and_completed_once() {
    const SRC: &str = include_str!("../arch/aarch64/kthread.rs");
    let h = fn_body(SRC, "extern \"C\" fn yarm_aarch64_kthread_interrupt(");
    let spi = h
        .find("} else {\n        // SAFETY: the entry saved SPSR_EL1")
        .expect("device arm");
    let dev = &h[spi..];
    assert_eq!(
        dev.matches("settle_external_interrupt_at_bridge(").count(),
        1
    );
    assert!(dev.contains("InterruptCompletion::ArchSingleStep { line: intid }"));
    assert_eq!(
        dev.matches("complete_interrupt(ack)").count(),
        1,
        "one EOIR in the device arm"
    );
    let settle = dev.find("settle_external_interrupt_at_bridge(").unwrap();
    let eoi = dev.find("complete_interrupt(ack)").unwrap();
    assert!(settle < eoi, "the completion follows the delivery");
    assert!(dev.contains("pool.note_device_interrupt(cpu)"));
    assert!(h.contains("} else if intid < FIRST_SPI_INTID {"));
    assert!(h.contains("KTHREAD_IRQ_UNSUPPORTED"));
}

/// x86_64: the kernel-thread IDT routes every external line above the timer to the device bridge,
/// which delivers through the bridge (whose single step is the one LAPIC EOI) and writes no EOI of
/// its own.
#[test]
fn x86_kernel_thread_device_vectors_enter_the_device_bridge() {
    const SRC: &str = include_str!("../arch/x86_64/kthread.rs");
    const IDT: &str = include_str!("../arch/x86_64/descriptor_tables.rs");
    let h = fn_body(SRC, "extern \"C\" fn yarm_x86_64_kthread_device_interrupt(");
    let auth = h
        .find("pool.authenticate_interrupt(frame, hardware)")
        .expect("auth");
    let settle = h
        .find("settle_external_interrupt_at_bridge(")
        .expect("settle");
    assert!(auth < settle);
    assert_eq!(h.matches("settle_external_interrupt_at_bridge(").count(), 1);
    assert!(h.contains("InterruptCompletion::ArchSingleStep { line }"));
    for eoi in ["external_irq_eoi", "lapic_eoi", "acknowledge_interrupt"] {
        assert!(!h.contains(eoi), "no second EOI: {eoi}");
    }
    assert!(h.contains("pool.note_device_interrupt(cpu)"));
    let vecs: Vec<usize> = (0x21..0x20 + crate::arch::platform_constants::MAX_IRQ_LINES).collect();
    let list = vecs
        .iter()
        .map(|v| std::string::ToString::to_string(v))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        SRC.matches(&std::format!(".irp v, {list}")).count(),
        2,
        "stub per line, and its table"
    );
    let load = fn_body(IDT, "pub(crate) fn load_kthread_idt()");
    assert!(load.contains("super::kthread::yarm_x86_64_kthread_dev_stubs"));
    assert!(load.contains("KTHREAD_DEVICE_VECTOR_FIRST + i"));
}

// ── KTHREAD-CLOSURE1 §B: plain (non-masking) lock guards at scheduling points ───────────────

use crate::kernel::lock::SpinLock;
use core::ptr::NonNull;
use std::sync::atomic::{AtomicBool as StdAtomicBool, AtomicUsize as StdAtomicUsize};

/// Per-test plain locks (never shared between tests) and what the thread observed.
struct Plain {
    a: SpinLock<u64>,
    b: SpinLock<u64>,
    c: SpinLock<u64>,
    seen: Mutex<Vec<(&'static str, Option<KSwitchRefusal>, u32)>>,
    /// The thread's own counter, published for the dispatcher-side mutation case.
    counter: StdAtomicUsize,
}

impl Plain {
    fn new() -> Self {
        Self {
            a: SpinLock::new(0),
            b: SpinLock::new(0),
            c: SpinLock::new(0),
            seen: Mutex::new(Vec::new()),
            counter: StdAtomicUsize::new(0),
        }
    }

    fn seen(&self) -> Vec<(&'static str, Option<KSwitchRefusal>, u32)> {
        self.seen.lock().unwrap().clone()
    }
}

fn plain_fixture(arg: usize) -> &'static Plain {
    // SAFETY: every test keeps its fixture alive until its dispatcher returns.
    unsafe { &*(arg as *const Plain) }
}

fn note(ctx: &KThreadCtx, f: &Plain, what: &'static str, r: Result<(), KSwitchRefusal>) {
    let held = ctx.pool().plain_guards_held(ctx.id());
    f.seen.lock().unwrap().push((what, r.err(), held));
}

/// Test cleanup only: a poisoned thread can never exit (exit is refused, and refusing exit is
/// fatal), so a test that poisons its own counter on purpose clears it before returning.
fn clear_poison_for_test_exit(ctx: &KThreadCtx) {
    ctx.pool().plain_counters()[ctx.id().slot()].store(0, Ordering::Release);
}

fn plain_across_points(ctx: &KThreadCtx, arg: usize) {
    let f = plain_fixture(arg);
    let k = ctx.shared();
    let g = f.a.lock();
    let queue_before = queued(k, ctx.home());
    let refused_before = ctx.pool().counters().switch_refused;
    note(ctx, f, "yield", ctx.yield_now());
    note(ctx, f, "sched_point", ctx.sched_point().map(|_| ()));
    let q = KWaitQueue::new();
    let w = q.wait_until(ctx, || false);
    note(
        ctx,
        f,
        "block",
        match w {
            Err(KWaitError::Refused(r)) => Err(r),
            _ => Ok(()),
        },
    );
    assert_eq!(q.registered(), 0, "a refused wait leaves no registration");
    // `exit` makes this check first and refuses fatally; the check itself is what is tested.
    note(ctx, f, "exit", ctx.switch_check("exit"));
    // A refusal changes nothing: still running, still resident, the queue and current unchanged,
    // the guard still counted; only the refusal counter moved.
    assert_eq!(ctx.pool().state_of(ctx.id()), Some(KState::Running));
    assert_eq!(current(k, ctx.home()), Some(ctx.id().raw()));
    assert_eq!(queued(k, ctx.home()), queue_before);
    assert_eq!(
        ctx.pool().counters().switch_refused,
        refused_before + 4,
        "one refusal per attempt"
    );
    drop(g);
    note(ctx, f, "after_release", ctx.yield_now());
    note(ctx, f, "resumed", Ok(()));
}

#[test]
fn a_plain_lock_held_across_yield_block_or_exit_is_refused_and_changes_nothing() {
    let k = kernel();
    let f = Plain::new();
    k.kthreads
        .create(
            plain_across_points,
            &f as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    assert_eq!(
        run_cpu(&k, CPU),
        2,
        "one switch: the yield after the release"
    );
    let held = Some(KSwitchRefusal::PlainLockHeld);
    assert_eq!(
        f.seen(),
        std::vec![
            ("yield", held, 1),
            ("sched_point", held, 1),
            ("block", held, 1),
            ("exit", held, 1),
            ("after_release", None, 0),
            ("resumed", None, 0),
        ]
    );
    assert!(f.a.try_lock().is_some(), "released");
}

fn plain_nested(ctx: &KThreadCtx, arg: usize) {
    let f = plain_fixture(arg);
    let a = f.a.lock();
    let b = f.b.try_lock().expect("free");
    note(ctx, f, "two", ctx.yield_now());
    drop(a);
    note(ctx, f, "partial", ctx.yield_now());
    drop(b);
    note(ctx, f, "none", ctx.yield_now());
    // The other order: inner first.
    let a = f.a.lock();
    let b = f.b.lock();
    drop(b);
    note(ctx, f, "inner_released", ctx.yield_now());
    drop(a);
    note(ctx, f, "outer_released", ctx.yield_now());
}

#[test]
fn nested_and_partial_releases_keep_the_thread_ineligible_until_the_last_guard() {
    let k = kernel();
    let f = Plain::new();
    k.kthreads
        .create(
            plain_nested,
            &f as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    assert_eq!(run_cpu(&k, CPU), 3);
    let held = Some(KSwitchRefusal::PlainLockHeld);
    assert_eq!(
        f.seen(),
        std::vec![
            ("two", held, 2),
            ("partial", held, 1),
            ("none", None, 0),
            ("inner_released", held, 1),
            ("outer_released", None, 0),
        ]
    );
}

/// An interrupt handler that runs on the interrupted thread's stack (as every port's kernel-thread
/// vector does) and takes a plain lock of its own.
fn handler_on_this_stack(ctx: &KThreadCtx, f: &Plain) -> u32 {
    let _g = f.c.lock();
    ctx.pool().plain_guards_held(ctx.id())
}

fn plain_with_handlers(ctx: &KThreadCtx, arg: usize) {
    let f = plain_fixture(arg);
    // Balanced handler activity while no guard is held, and while one is.
    let inside = handler_on_this_stack(ctx, f);
    note(ctx, f, "handler_unheld", Ok(()));
    f.seen
        .lock()
        .unwrap()
        .push(("handler_unheld_inside", None, inside));
    let g = f.a.lock();
    let inside = handler_on_this_stack(ctx, f);
    f.seen
        .lock()
        .unwrap()
        .push(("handler_held_inside", None, inside));
    note(ctx, f, "after_handler", ctx.yield_now());
    // Activity on other stacks (another host thread taking and releasing plain locks, as a
    // handler on a trap stack or another CPU would) never touches this thread's count.
    let stop = StdAtomicBool::new(false);
    let mut stable = true;
    std::thread::scope(|s| {
        s.spawn(|| {
            while !stop.load(Ordering::Acquire) {
                let _x = f.b.lock();
                let _y = f.c.try_lock();
            }
        });
        for _ in 0..20_000 {
            stable &= ctx.pool().plain_guards_held(ctx.id()) == 1;
        }
        stop.store(true, Ordering::Release);
    });
    f.seen
        .lock()
        .unwrap()
        .push(("other_stack_activity", None, u32::from(stable)));
    drop(g);
    note(ctx, f, "released", ctx.yield_now());
    // A handler that leaks its guard on this stack leaves the thread ineligible, attributed here.
    let leaked = f.c.lock();
    core::mem::forget(leaked);
    note(ctx, f, "handler_leak", ctx.yield_now());
    clear_poison_for_test_exit(ctx);
}

#[test]
fn interrupt_handlers_on_the_thread_stack_balance_their_guards_and_other_stacks_never_count() {
    let k = kernel();
    let f = Plain::new();
    k.kthreads
        .create(
            plain_with_handlers,
            &f as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    assert_eq!(run_cpu(&k, CPU), 2);
    let held = Some(KSwitchRefusal::PlainLockHeld);
    assert_eq!(
        f.seen(),
        std::vec![
            ("handler_unheld", None, 0),
            ("handler_unheld_inside", None, 1),
            ("handler_held_inside", None, 2),
            ("after_handler", held, 1),
            ("other_stack_activity", None, 1),
            ("released", None, 0),
            ("handler_leak", held, 1),
        ]
    );
}

struct TwoCpus {
    x: Plain,
    y: Plain,
    x_held: StdAtomicBool,
    y_held: StdAtomicBool,
    x_released: StdAtomicBool,
}

fn plain_on_cpu_x(ctx: &KThreadCtx, arg: usize) {
    // SAFETY: the test keeps the fixture alive.
    let t = unsafe { &*(arg as *const TwoCpus) };
    let g = t.x.a.lock();
    t.x_held.store(true, Ordering::Release);
    while !t.y_held.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    note(ctx, &t.x, "x_held", ctx.yield_now());
    drop(g);
    t.x_released.store(true, Ordering::Release);
    note(ctx, &t.x, "x_released", ctx.yield_now());
}

fn plain_on_cpu_y(ctx: &KThreadCtx, arg: usize) {
    // SAFETY: the test keeps the fixture alive.
    let t = unsafe { &*(arg as *const TwoCpus) };
    let g = t.y.b.lock();
    t.y_held.store(true, Ordering::Release);
    while !t.x_held.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    note(ctx, &t.y, "y_held", ctx.yield_now());
    // X's release on its own CPU does not make Y eligible.
    while !t.x_released.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    note(ctx, &t.y, "y_after_x_release", ctx.yield_now());
    drop(g);
    note(ctx, &t.y, "y_released", ctx.yield_now());
}

#[test]
fn different_cpus_holding_different_plain_locks_are_accounted_independently() {
    let k = kernel();
    let t = TwoCpus {
        x: Plain::new(),
        y: Plain::new(),
        x_held: StdAtomicBool::new(false),
        y_held: StdAtomicBool::new(false),
        x_released: StdAtomicBool::new(false),
    };
    k.kthreads
        .create(
            plain_on_cpu_x,
            &t as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    k.kthreads
        .create(
            plain_on_cpu_y,
            &t as *const _ as usize,
            OTHER,
            TaskPriority::Normal,
        )
        .expect("create");
    let (nx, ny) = std::thread::scope(|s| {
        let x = s.spawn(|| run_cpu(&k, CPU));
        let y = s.spawn(|| run_cpu(&k, OTHER));
        (x.join().unwrap(), y.join().unwrap())
    });
    assert_eq!((nx, ny), (2, 2));
    let held = Some(KSwitchRefusal::PlainLockHeld);
    assert_eq!(
        t.x.seen(),
        std::vec![("x_held", held, 1), ("x_released", None, 0)]
    );
    assert_eq!(
        t.y.seen(),
        std::vec![
            ("y_held", held, 1),
            ("y_after_x_release", held, 1),
            ("y_released", None, 0)
        ]
    );
}

fn plain_accounting_failures(ctx: &KThreadCtx, arg: usize) {
    let f = plain_fixture(arg);
    let mine = || &ctx.pool().plain_counters()[ctx.id().slot()];
    let faults = plain_accounting_faults();
    // 1. Underflow: a release with nothing counted latches the poison.
    plain_guard_released(Some(NonNull::from(mine())));
    note(ctx, f, "underflow", ctx.yield_now());
    // Nothing restores eligibility: another balanced guard, released, leaves it poisoned.
    drop(f.a.lock());
    note(ctx, f, "underflow_then_balanced", ctx.yield_now());
    clear_poison_for_test_exit(ctx);
    // 2. Overflow: one more guard than the counter can hold latches the poison.
    mine().store(PLAIN_POISON - 1, Ordering::Release);
    let g = f.a.lock();
    drop(g);
    note(ctx, f, "overflow", ctx.yield_now());
    clear_poison_for_test_exit(ctx);
    // 3. A guard taken off every kernel-thread stack but released on this one.
    plain_guard_released(None);
    note(ctx, f, "released_here_taken_elsewhere", ctx.yield_now());
    clear_poison_for_test_exit(ctx);
    assert!(plain_accounting_faults() >= faults + 3);
    // 4. Publish this counter, then switch away clean: the dispatcher releases a guard "taken
    //    here" from its own stack.
    f.counter
        .store(mine() as *const AtomicU32 as usize, Ordering::Release);
    note(ctx, f, "clean", ctx.yield_now());
    note(ctx, f, "taken_here_released_elsewhere", ctx.yield_now());
    clear_poison_for_test_exit(ctx);
    note(ctx, f, "cleared_for_exit", ctx.yield_now());
}

#[test]
fn accounting_failures_latch_and_never_restore_eligibility() {
    let k = kernel();
    let f = Plain::new();
    k.kthreads
        .create(
            plain_accounting_failures,
            &f as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    // Run until the thread has published its counter and switched away clean.
    let mut n = 0;
    while f.counter.load(Ordering::Acquire) == 0 || n == 0 {
        let sel = k
            .with_cpu_sched_split(CPU, |c| {
                c.dispatch_next_accepted_selection(|t| k.kthreads.admit_dispatch(CPU, t))
            })
            .expect("cpu");
        let AcceptedSelection::Selected(DispatchSelection::Dequeued { tid }) = sel else {
            panic!("the thread is runnable");
        };
        let _ = k.kthreads.dispatch_selected(CPU, tid);
        n += 1;
    }
    // On the dispatcher's own stack: a release naming the thread's counter.
    // SAFETY: the counter belongs to this test's live pool.
    let c = unsafe { &*(f.counter.load(Ordering::Acquire) as *const AtomicU32) };
    plain_guard_released(Some(NonNull::from(c)));
    assert_eq!(c.load(Ordering::Acquire), PLAIN_POISON);
    run_cpu(&k, CPU);
    let acct = Some(KSwitchRefusal::PlainLockAccounting);
    assert_eq!(
        f.seen(),
        std::vec![
            ("underflow", acct, PLAIN_POISON),
            ("underflow_then_balanced", acct, PLAIN_POISON),
            ("overflow", acct, PLAIN_POISON),
            ("released_here_taken_elsewhere", acct, PLAIN_POISON),
            // The yield succeeded clean; by the time the thread resumed, the dispatcher's
            // release had poisoned its counter.
            ("clean", None, PLAIN_POISON),
            ("taken_here_released_elsewhere", acct, PLAIN_POISON),
            ("cleared_for_exit", None, 0),
        ]
    );
}

#[test]
fn plain_guards_off_kernel_thread_stacks_are_not_counted() {
    let k = kernel();
    let f = Plain::new();
    let id = k
        .kthreads
        .create(nop, 0, CPU, TaskPriority::Normal)
        .expect("create");
    // The dispatcher (this test thread) holds a plain lock: no kernel-thread counter moves.
    let g = f.a.lock();
    assert_eq!(k.kthreads.plain_guards_held(id), 0);
    drop(g);
    assert_eq!(run_cpu(&k, CPU), 1);
    assert_eq!(k.kthreads.plain_guards_held(id), 0);
}

#[test]
#[should_panic(expected = "KTHREAD_PLAIN_LOCK_INVARIANT")]
fn a_slot_reused_with_a_live_count_fails_closed() {
    let k = kernel();
    let id = k
        .kthreads
        .create(nop, 0, CPU, TaskPriority::Normal)
        .expect("create");
    run_cpu(&k, CPU);
    // The free list is LIFO: the next creation takes the same slot.
    k.kthreads.plain_counters()[id.slot()].store(1, Ordering::Release);
    let _ = k.kthreads.create(nop, 0, CPU, TaskPriority::Normal);
}

struct PlainCost {
    lock: SpinLock<u64>,
    on_stack_ns: StdAtomicUsize,
}

const COST_ROUNDS: usize = 2_000_000;

fn cost_round(l: &SpinLock<u64>) -> u128 {
    let t = std::time::Instant::now();
    for _ in 0..COST_ROUNDS {
        let mut g = l.lock();
        *g = g.wrapping_add(1);
    }
    t.elapsed().as_nanos()
}

fn plain_cost_on_stack(_ctx: &KThreadCtx, arg: usize) {
    // SAFETY: the test keeps the fixture alive.
    let f = unsafe { &*(arg as *const PlainCost) };
    let ns = cost_round(&f.lock);
    f.on_stack_ns.store(ns as usize, Ordering::Release);
}

/// KTHREAD-CLOSURE1 §B — the accounting's cost per plain acquire/release pair, measured on the
/// host (run explicitly: `cargo test --release --lib -- --ignored plain_lock_cost`). Three
/// situations: no kernel-thread stack registered (every production boot that creates no kernel
/// thread), a registered pool and a guard off its stacks, and a guard on a kernel-thread stack.
#[test]
#[ignore]
fn plain_lock_cost() {
    let l = SpinLock::new(0u64);
    // Registry state is process-wide: run this benchmark alone.
    let unregistered = PLAIN_HIGH.load(Ordering::Acquire) == 0;
    let empty = cost_round(&l);
    let k = kernel();
    let f = PlainCost {
        lock: SpinLock::new(0),
        on_stack_ns: StdAtomicUsize::new(0),
    };
    k.kthreads
        .create(
            plain_cost_on_stack,
            &f as *const _ as usize,
            CPU,
            TaskPriority::Normal,
        )
        .expect("create");
    let off_stack = cost_round(&l);
    run_cpu(&k, CPU);
    let on_stack = f.on_stack_ns.load(Ordering::Acquire) as u128;
    let per = |ns: u128| ns as f64 / COST_ROUNDS as f64;
    std::println!(
        "PLAIN_LOCK_COST rounds={} registry_empty={} empty_ns={:.2} registered_off_stack_ns={:.2} on_kthread_stack_ns={:.2}",
        COST_ROUNDS,
        unregistered,
        per(empty),
        per(off_stack),
        per(on_stack)
    );
}

// ── KTHREAD-CLOSURE1 §C: the protected window's arithmetic ─────────────────────────────────────

#[test]
fn the_window_view_puts_each_stack_alone_at_the_bottom_of_its_stride() {
    use crate::kernel::kthread::{KSTACK_STRIDE, StackView};
    const STACK: usize = 16 * 1024; // the production stack size the window is built for
    let w = 0xFFFF_FF00_0000_0000usize;
    let v = StackView::window(w);
    assert_eq!(
        v.base,
        w + KSTACK_STRIDE,
        "slot 0's gap is the window's first stride"
    );
    for slot in [0usize, 1, 7, KTHREAD_CAPACITY - 1] {
        let (low, high) = v.bounds(slot);
        assert_eq!(low, w + (slot + 1) * KSTACK_STRIDE);
        assert_eq!(high - low, KTHREAD_STACK_BYTES);
        assert_eq!(low % KSTACK_STRIDE, 0, "a stack starts a stride");
        assert_eq!(v.slot_of(low), Some(slot));
        assert_eq!(v.slot_of(high - 1), Some(slot));
        assert_eq!(
            v.slot_of(high),
            None,
            "above a stack is the next slot's gap"
        );
        assert_eq!(v.slot_of(low - 1), None);
        // The gap below the stack belongs to this slot, all the way down to the previous top.
        assert_eq!(v.gap_owner(low - 1), Some(slot));
        assert_eq!(v.gap_owner(low - 4096), Some(slot));
        assert_eq!(
            v.gap_owner(low - (KSTACK_STRIDE - KTHREAD_STACK_BYTES)),
            Some(slot)
        );
        assert_eq!(v.gap_owner(low), None, "the stack itself is no gap");
    }
    // Nothing outside the window is a stack or a gap.
    assert_eq!(v.slot_of(w + (KTHREAD_CAPACITY + 1) * KSTACK_STRIDE), None);
    assert_eq!(
        v.gap_owner(w + (KTHREAD_CAPACITY + 1) * KSTACK_STRIDE),
        None
    );
    assert_eq!(v.gap_owner(w - 1), None);
    assert_eq!(v.slot_of(0x1000), None);
    // The production gap leaves room for two maximal frames plus a trap frame (the image check's
    // bound) with a 16 KiB stack.
    let bound = (KSTACK_STRIDE - STACK - 4096) / 2;
    assert!(2 * bound + 4096 <= KSTACK_STRIDE - STACK);
}

#[test]
fn the_contiguous_view_has_no_gaps_and_finds_every_stack() {
    use crate::kernel::kthread::StackView;
    let base = 0x10_0000usize;
    let v = StackView::contiguous(base);
    for slot in 0..KTHREAD_CAPACITY {
        let (low, high) = v.bounds(slot);
        assert_eq!(v.slot_of(low), Some(slot));
        assert_eq!(v.slot_of(high - 1), Some(slot));
        assert_eq!(v.gap_owner(low.wrapping_sub(1)), None);
    }
    assert_eq!(
        v.slot_of(base + KTHREAD_CAPACITY * KTHREAD_STACK_BYTES),
        None
    );
    assert_eq!(StackView::contiguous(0).slot_of(0x40), None);
}
