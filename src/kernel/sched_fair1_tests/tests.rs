// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-LOCK-FAIR1 — fair admission of the ACTUAL scheduler locks (1L and 1G), observed through
//! the primitive's own tickets.
//!
//! The order checks never use log or wall-clock order:
//! * ARRIVAL order is fixed by construction. Each entrant is started only after the previous one
//!   is observed queued, through the lock's outstanding-ticket count.
//! * SERVICE order is recorded by each holder inside its section, so it is the order the lock
//!   granted.
//!
//! A shared checker compares the two. A negative control shows the checker rejects a sequence in
//! which a known earlier entrant is overtaken.

use std::string::String;
use std::sync::{Arc, Mutex};
use std::vec::Vec;

use crate::kernel::scheduler::{CpuId, SchedDomain};
use crate::kernel::ticket_lock::{TicketLockIrq, test_irq};

/// Who acquired, with which ticket, in the order the lock served them (recorded under the lock).
type Service = Arc<Mutex<Vec<(u32, &'static str)>>>;

/// THE order check. `arrival` is the order entrants drew their tickets (known by construction);
/// `service` is what the lock did.
///
/// FIFO admission means two things:
/// * service order equals arrival order, entrant by entrant;
/// * the tickets served are consecutive. A gap would mean a ticket was skipped, or a holder was
///   admitted without its ticket being served.
fn check_fifo(arrival: &[&'static str], service: &[(u32, &'static str)]) -> Result<(), String> {
    if arrival.len() != service.len() {
        return Err(std::format!(
            "{} entrants arrived, {} were served",
            arrival.len(),
            service.len()
        ));
    }
    for (i, (want, (_, got))) in arrival.iter().zip(service).enumerate() {
        if want != got {
            return Err(std::format!(
                "position {i}: {want} arrived here but {got} was served — an earlier entrant was overtaken"
            ));
        }
    }
    for w in service.windows(2) {
        if w[1].0 != w[0].0.wrapping_add(1) {
            return Err(std::format!(
                "ticket {} served after {}: not consecutive",
                w[1].0,
                w[0].0
            ));
        }
    }
    Ok(())
}

fn domain() -> SchedDomain {
    let mut d = SchedDomain::new(0b1111, 1);
    let mut all = d.exclusive();
    for c in 1..4 {
        all.bring_up_cpu(CpuId(c)).expect("online");
    }
    drop(all);
    d
}

fn wait_until(mut f: impl FnMut() -> bool) {
    let mut spins = 0u64;
    while !f() {
        std::thread::yield_now();
        spins += 1;
        assert!(spins < 50_000_000, "a queued entrant never appeared");
    }
}

// ── 1L: three queued peers, and the releasing holder re-acquiring at once ───────────────────

/// The holder of 1L[1] waits until three peers are queued, one after another, in a known order.
/// It then releases and IMMEDIATELY re-acquires. The peers are served in arrival order, and the
/// releasing holder only after all of them, with consecutive tickets.
#[test]
fn fair1_cpu_lock_serves_queued_peers_before_a_rapid_reacquire() {
    let d = Arc::new(domain());
    let served: Service = Arc::new(Mutex::new(Vec::new()));
    let cpu = CpuId(1);
    let first = d.lock_cpu(cpu).expect("1L");
    let t0 = first.witness_ticket();
    let mut peers = Vec::new();
    for (i, name) in ["P1", "P2", "P3"].into_iter().enumerate() {
        let (d2, s2) = (Arc::clone(&d), Arc::clone(&served));
        peers.push(std::thread::spawn(move || {
            let g = d2.lock_cpu(cpu).expect("1L");
            s2.lock().unwrap().push((g.witness_ticket(), name));
            drop(g);
        }));
        // The next peer starts only once this one holds a ticket: arrival order is fixed.
        wait_until(|| d.witness_cpu_outstanding(1) == 2 + i as u32);
    }
    drop(first);
    // Rapid re-acquisition by the releasing holder, while three peers wait.
    let again = d.lock_cpu(cpu).expect("1L");
    served.lock().unwrap().push((again.witness_ticket(), "H"));
    drop(again);
    for p in peers {
        p.join().expect("peer");
    }
    let service = served.lock().unwrap().clone();
    check_fifo(&["P1", "P2", "P3", "H"], &service).expect("FIFO");
    assert_eq!(service[0].0, t0.wrapping_add(1));
    assert_eq!(d.witness_cpu_outstanding(1), 0, "every ticket was served");
}

// ── 1G: the same, on the admission lock, with the peers on the production 1G → 1L path ────────

/// The same arrangement on 1G. The holder takes 1G alone. The peers take the production
/// admission path (1G, then that CPU's 1L) and record under both. The releasing holder's
/// re-acquisition of 1G is served after every queued peer.
#[test]
fn fair1_admission_lock_serves_queued_peers_before_a_rapid_reacquire() {
    let d = Arc::new(domain());
    let served: Service = Arc::new(Mutex::new(Vec::new()));
    let first = d.witness_lock_admission();
    let mut peers = Vec::new();
    for (i, name) in ["P1", "P2", "P3"].into_iter().enumerate() {
        let (d2, s2) = (Arc::clone(&d), Arc::clone(&served));
        peers.push(std::thread::spawn(move || {
            let g = d2.lock_cpu_admission(CpuId(2)).expect("1G + 1L");
            s2.lock().unwrap().push((0, name));
            drop(g);
        }));
        wait_until(|| d.witness_admission_outstanding() == 2 + i as u32);
    }
    let t0 = first.ticket();
    drop(first);
    let again = d.witness_lock_admission();
    let t_again = again.ticket();
    served.lock().unwrap().push((0, "H"));
    drop(again);
    for p in peers {
        p.join().expect("peer");
    }
    let service = served.lock().unwrap().clone();
    let names: Vec<&str> = service.iter().map(|(_, n)| *n).collect();
    assert_eq!(names, ["P1", "P2", "P3", "H"], "1G served in arrival order");
    assert_eq!(
        t_again,
        t0.wrapping_add(4),
        "the re-draw came after the three peers' tickets"
    );
    assert_eq!(d.witness_admission_outstanding(), 0);
}

/// The coherent view (1G, then every 1L ascending) queues fairly too. While a peer holds 1L[2],
/// a `lock_all` entrant and then a second peer queue on it. The `lock_all` entrant is served on
/// 1L[2] before the later peer. Releasing it leaves no ticket outstanding on any lock.
#[test]
fn fair1_the_coherent_view_queues_in_order_and_releases_every_ticket() {
    let d = Arc::new(domain());
    let served: Service = Arc::new(Mutex::new(Vec::new()));
    let first = d.lock_cpu(CpuId(2)).expect("1L");
    let (d2, s2) = (Arc::clone(&d), Arc::clone(&served));
    let all = std::thread::spawn(move || {
        let g = d2.lock_all();
        s2.lock().unwrap().push((0, "ALL"));
        drop(g);
    });
    wait_until(|| d.witness_cpu_outstanding(2) == 2);
    let (d3, s3) = (Arc::clone(&d), Arc::clone(&served));
    let late = std::thread::spawn(move || {
        let g = d3.lock_cpu(CpuId(2)).expect("1L");
        s3.lock().unwrap().push((0, "LATE"));
        drop(g);
    });
    wait_until(|| d.witness_cpu_outstanding(2) == 3);
    drop(first);
    all.join().expect("all");
    late.join().expect("late");
    let names: Vec<&str> = served.lock().unwrap().iter().map(|(_, n)| *n).collect();
    assert_eq!(names, ["ALL", "LATE"]);
    for c in 0..crate::kernel::scheduler::MAX_CPUS {
        assert_eq!(d.witness_cpu_outstanding(c), 0, "1L[{c}] fully released");
    }
    assert_eq!(d.witness_admission_outstanding(), 0, "1G released");
}

// ── negative control ─────────────────────────────────────────────────────────────────────────

/// The order check REJECTS a service order in which a known earlier entrant is overtaken —
/// exactly what a TTAS release-and-recompete permits — and a gap in the tickets.
#[test]
fn fair1_negative_control_the_order_check_rejects_an_overtaken_entrant() {
    let arrival = ["P1", "P2", "P3", "H"];
    // The releasing holder barges ahead of P2 and P3.
    let overtaken = [(5, "P1"), (6, "H"), (7, "P2"), (8, "P3")];
    let err = check_fifo(&arrival, &overtaken).expect_err("an overtake must be rejected");
    assert!(err.contains("overtaken"), "{err}");
    // A skipped ticket is rejected even with the right names.
    let gap = [(5, "P1"), (6, "P2"), (8, "P3"), (9, "H")];
    assert!(check_fifo(&arrival, &gap).is_err());
    // And the true order passes.
    assert!(check_fifo(&arrival, &[(5, "P1"), (6, "P2"), (7, "P3"), (8, "H")]).is_ok());
}

// ── interrupts ───────────────────────────────────────────────────────────────────────────────

/// Interrupts are masked BEFORE the ticket is drawn, stay masked across the whole wait, and the
/// saved state is restored on release — enabled stays enabled, masked stays masked — including
/// for the 65 nested guards of the coherent view.
#[test]
fn fair1_interrupts_are_masked_before_the_draw_and_restored_on_release() {
    let d = Arc::new(domain());
    test_irq::set_enabled(true);
    test_irq::DRAWS_WITH_IRQS_ENABLED.with(|c| c.set(0));
    {
        let g = d.lock_cpu(CpuId(0)).expect("1L");
        assert!(!test_irq::enabled(), "masked while held");
        drop(g);
    }
    assert!(test_irq::enabled(), "enabled state restored");
    test_irq::set_enabled(false);
    {
        let g = d.lock_cpu(CpuId(0)).expect("1L");
        drop(g);
    }
    assert!(!test_irq::enabled(), "masked state restored as masked");
    test_irq::set_enabled(true);
    {
        let all = d.lock_all();
        assert!(!test_irq::enabled());
        drop(all);
    }
    assert!(
        test_irq::enabled(),
        "the coherent view's 65 guards restore in reverse"
    );
    assert_eq!(
        test_irq::DRAWS_WITH_IRQS_ENABLED.with(|c| c.get()),
        0,
        "no ticket was ever drawn with interrupts enabled"
    );

    // A contended wait spins with interrupts masked, every iteration.
    let holder = d.lock_cpu(CpuId(3)).expect("1L");
    let d2 = Arc::clone(&d);
    let waiter = std::thread::spawn(move || {
        test_irq::set_enabled(true);
        test_irq::WAIT_SPINS.with(|c| c.set(0));
        test_irq::WAIT_SPINS_IRQS_ENABLED.with(|c| c.set(0));
        let g = d2.lock_cpu(CpuId(3)).expect("1L");
        drop(g);
        (
            test_irq::WAIT_SPINS.with(|c| c.get()),
            test_irq::WAIT_SPINS_IRQS_ENABLED.with(|c| c.get()),
            test_irq::DRAWS_WITH_IRQS_ENABLED.with(|c| c.get()),
            test_irq::enabled(),
        )
    });
    wait_until(|| d.witness_cpu_outstanding(3) == 2);
    for _ in 0..1000 {
        std::thread::yield_now();
    }
    drop(holder);
    let (spins, spins_enabled, draws_enabled, restored) = waiter.join().expect("waiter");
    assert!(spins > 0, "the waiter actually waited");
    assert_eq!(spins_enabled, 0, "every wait spin ran masked");
    assert_eq!(draws_enabled, 0);
    assert!(restored, "the waiter's enabled state came back on release");
}

// ── wraparound ───────────────────────────────────────────────────────────────────────────────

/// Counters start three tickets before 2³² and wrap mid-queue: service stays FIFO and
/// consecutive (modulo 2³²) across the wrap, and the lock is free afterwards.
#[test]
fn fair1_tickets_wrap_without_reordering() {
    let lock = Arc::new(TicketLockIrq::with_ticket_base(0u32, u32::MAX - 2));
    let served: Service = Arc::new(Mutex::new(Vec::new()));
    for round in 0..3 {
        let first = lock.lock();
        let mut peers = Vec::new();
        for (i, name) in ["P1", "P2", "P3", "P4"].into_iter().enumerate() {
            let (l2, s2) = (Arc::clone(&lock), Arc::clone(&served));
            peers.push(std::thread::spawn(move || {
                let mut g = l2.lock();
                *g += 1;
                s2.lock().unwrap().push((g.ticket(), name));
            }));
            wait_until(|| lock.outstanding() == 2 + i as u32);
        }
        let t0 = first.ticket();
        drop(first);
        for p in peers {
            p.join().expect("peer");
        }
        let mut s = served.lock().unwrap();
        check_fifo(&["P1", "P2", "P3", "P4"], &s).expect("FIFO across the wrap");
        assert_eq!(s[0].0, t0.wrapping_add(1));
        if round == 0 {
            assert!(
                s.iter().any(|(t, _)| *t == 0),
                "the wrap to 0 happened inside this round"
            );
        }
        s.clear();
        assert_eq!(lock.outstanding(), 0);
    }
    assert_eq!(*lock.lock(), 12);
}

// ── try_lock ─────────────────────────────────────────────────────────────────────────────────

/// A try-lock never barges and never leaves an unserved ticket. It fails, publishing nothing,
/// while the lock is held or awaited. It succeeds only on a free lock, owning the ticket already
/// served. A queued entrant is served before any later try.
#[test]
fn fair1_try_lock_never_barges_and_never_strands_a_ticket() {
    let lock = Arc::new(TicketLockIrq::with_ticket_base(0u32, u32::MAX));
    let holder = lock.lock();
    assert!(lock.try_lock().is_none(), "held");
    assert_eq!(lock.outstanding(), 1, "a failed try published nothing");
    let l2 = Arc::clone(&lock);
    let waiter = std::thread::spawn(move || {
        let mut g = l2.lock();
        *g = 7;
        g.ticket()
    });
    wait_until(|| lock.outstanding() == 2);
    assert!(lock.try_lock().is_none(), "held and awaited");
    assert_eq!(lock.outstanding(), 2);
    let held_ticket = holder.ticket();
    drop(holder);
    let waiter_ticket = waiter.join().expect("waiter");
    assert_eq!(
        waiter_ticket,
        held_ticket.wrapping_add(1),
        "the queued entrant was served next"
    );
    let g = lock.try_lock().expect("free: the try succeeds");
    assert_eq!(*g, 7, "and sees the queued entrant's write");
    assert_eq!(
        g.ticket(),
        lock.serving(),
        "it owns the ticket being served"
    );
    drop(g);
    assert_eq!(lock.outstanding(), 0, "no ticket left unserved");
}

// ── the bound ────────────────────────────────────────────────────────────────────────────────

/// Eight threads hammer 1L[0], re-acquiring as fast as they can. Each found at most seven
/// entrants ahead of its ticket, so with N CPUs at most N − 1 earlier entrants. The service
/// sequence is exactly consecutive tickets, with no overtaking.
#[test]
fn fair1_contention_bound_is_n_minus_one_entrants_ahead() {
    const N: usize = 8;
    const ROUNDS: usize = 2_000;
    let d = Arc::new(domain());
    let served: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::with_capacity(N * ROUNDS)));
    let threads: Vec<_> = (0..N)
        .map(|_| {
            let (d2, s2) = (Arc::clone(&d), Arc::clone(&served));
            std::thread::spawn(move || {
                let mut max_ahead = 0u32;
                for _ in 0..ROUNDS {
                    let g = d2.lock_cpu(CpuId(0)).expect("1L");
                    max_ahead = max_ahead.max(test_irq::LAST_AHEAD.with(|c| c.get()));
                    s2.lock().unwrap().push(g.witness_ticket());
                    drop(g);
                }
                max_ahead
            })
        })
        .collect();
    let max_ahead = threads
        .into_iter()
        .map(|t| t.join().expect("thread"))
        .max()
        .unwrap();
    assert!(
        max_ahead < N as u32,
        "at most N-1 entrants ahead, saw {max_ahead}"
    );
    let s = served.lock().unwrap();
    assert_eq!(s.len(), N * ROUNDS);
    for w in s.windows(2) {
        assert_eq!(
            w[1],
            w[0].wrapping_add(1),
            "service in ticket order, no overtaking"
        );
    }
}

/// Footprint: the ticket lock keeps the TTAS lock's one-cache-line footprint (two `u32` counters
/// in a 64-byte-aligned line replace one padded flag), so per-CPU scheduler state does not grow
/// by the primitive change.
#[test]
fn fair1_the_ticket_lock_keeps_the_ttas_lock_footprint() {
    use crate::kernel::lock::SpinLockIrq;
    assert_eq!(
        core::mem::size_of::<TicketLockIrq<()>>(),
        core::mem::size_of::<SpinLockIrq<()>>()
    );
    assert_eq!(core::mem::size_of::<TicketLockIrq<()>>(), 64);
    assert_eq!(core::mem::align_of::<TicketLockIrq<()>>(), 64);
    assert_eq!(
        core::mem::size_of::<TicketLockIrq<u64>>(),
        core::mem::size_of::<SpinLockIrq<u64>>()
    );
}
