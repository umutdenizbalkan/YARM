# SCRATCH ONLY (never committed), v3: in SCHED-LOCK1's before_release, after a section of the NEXT
# round's observer CPU (pair(ROUND)) has reserved its records and while no hold is open, wait
# (bounded) until the holder opens one -- so HOLD_BEGIN follows the section's records: the exact
# pre-hold-commit boundary. At most 4 hits and 16 timeouts; dumped as SLD_SCRATCH.
import sys
p=sys.argv[1]+'/src/kernel/schedlock1_witness.rs'
s=open(p).read()
anchor='    // An OWNER COMMIT by the observer on its own CPU, during an open hold'
assert s.count(anchor)==1
s=s.replace(anchor,'''    // SCRATCH DELAY v3 (evidence only).
    if exec == idx
        && sec_seq != 0
        && recording()
        && open_hold().is_none()
        && ROUND.load(Ordering::Acquire) < total_rounds()
        && pair(ROUND.load(Ordering::Acquire), NCPU.load(Ordering::Acquire)).1 == idx
        && SLD_LEFT.load(Ordering::Acquire) > 0
        && SLD_MISS.load(Ordering::Acquire) < 16
    {
        let mut spins = 0u64;
        while open_hold().is_none() && spins < 50_000_000 {
            core::hint::spin_loop();
            spins += 1;
        }
        if open_hold().is_some() {
            SLD_HITS.fetch_add(1, Ordering::AcqRel);
            SLD_LEFT.fetch_sub(1, Ordering::AcqRel);
        } else {
            SLD_MISS.fetch_add(1, Ordering::AcqRel);
        }
    }
'''+anchor)
a2='static HOLD_ACTIVE: AtomicU64 = AtomicU64::new(0);'
assert s.count(a2)==1
s=s.replace(a2,a2+'\nstatic SLD_LEFT: AtomicU64 = AtomicU64::new(4);\nstatic SLD_HITS: AtomicU64 = AtomicU64::new(0);\nstatic SLD_MISS: AtomicU64 = AtomicU64::new(0);')
a3='''    crate::kernel::apsched_witness::emit_twice(format_args!(
        "SL1W_END count'''
assert s.count(a3)==1
s=s.replace(a3,'''    crate::kernel::apsched_witness::emit_twice(format_args!(
        "SLD_SCRATCH hits={} left={} miss={}",
        SLD_HITS.load(Ordering::Acquire),
        SLD_LEFT.load(Ordering::Acquire),
        SLD_MISS.load(Ordering::Acquire)
    ));
'''+a3)
open(p,'w').write(s); print("delay v3 inserted")
