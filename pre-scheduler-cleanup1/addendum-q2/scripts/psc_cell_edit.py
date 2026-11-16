import sys
orig=open(sys.argv[1]).read(); out=sys.argv[2]
a=orig.index('pub(super) mod sched_time_witness {')
b=orig.index('\n}\n', a)+3
s=orig[a:b]
assert 'pub(super) fn run_once' in s and s.count('fn helper_saturate()')==1
def rep(old,new):
    global s
    assert s.count(old)==1,(old[:80],s.count(old))
    s=s.replace(old,new)
rep('''    /// About this long in wall time, by the calibrated tick (at least 3 ticks).
    const R_TARGET_NS: u64 = 4_000 * MS;''','''    /// About this long in wall time, by the calibrated tick (at least 3 ticks). Long enough that
    /// the slots W's and R's waits open are refilled before either falls due (SCHED-SAT-HANDOFF1).
    const R_TARGET_NS: u64 = 10_000 * MS;''')
rep('''    /// Fillers use stacks `0..FILLER_LIMIT`; the case-10 spinners use the last two.
    const FILLER_LIMIT: usize = FILLERS - 2;''','''    /// Fillers use stacks `0..SPAWN_LIMIT`, the spares `SPAWN_LIMIT..FILLER_LIMIT`; the case-10
    /// spinners use the last two.
    const FILLER_LIMIT: usize = FILLERS - 2;
    /// SCHED-SAT-HANDOFF1 — spare fillers, spawned while the queues are still empty and parked
    /// each on its own futex word. A balanced spawn cannot choose its CPU (the load mirrors count
    /// user threads, admission counts every queue entry, so a full CPU can look lighter), which is
    /// why a spawn top-up could leave one slot of a waiter's home queue open (W3b: 63 of 64 at R's
    /// deadline, every top-up refused). A futex wake places its waiter on the waiter's HOME CPU
    /// and is refused, leaving it parked, when that home is full (FUTEX-WAKE-CAP1): waking every
    /// spare in turn fills exactly the homes that have room.
    const SPARES: usize = 24;
    const SPAWN_LIMIT: usize = FILLER_LIMIT - SPARES;
    /// The spare pass's period once a pass placed nothing (it is a syscall per spare).
    const SPARE_IDLE_EVERY_NS: u64 = 1_000 * MS;
    /// The handoff line is logged once both waiters armed this long ago (their park completes).
    const HANDOFF_SETTLE_NS: u64 = 500 * MS;''')
rep('''    /// Case 8: long enough for H to saturate every queue on the slowest port before it ends. H
    /// gets the CPU once per rotation through every yielding filler, so spawning ~130 of them takes
    /// tens of seconds under RISC-V TCG (a 20 s deadline passed before the queues were full in a
    /// development run); 120 s leaves margin on every port.
    const Q_NS: u64 = 120_000 * MS;''','''    /// Case 8 (SCHED-SAT-HANDOFF1): W's sleep is armed only once H has VERIFIED saturation, and
    /// falls due this long after W saw it. A deadline fixed before saturation cannot be aligned:
    /// saturating took 56 s to 175 s on one RISC-V TCG image, and a fixed 120 s deadline passed
    /// first twice (W3, W3b). The span covers the handoff — W's and R's slots refilled by the
    /// spare pass — with margin.
    const W_SLEEP_NS: u64 = 10_000 * MS;''')
rep('''    static Q_DEADLINE: AtomicU64 = AtomicU64::new(0);
    /// Case 8: W is about to enter its sleep (H tops up only after this).
    static Q_ENTERING: AtomicU32 = AtomicU32::new(0);''','''    /// Case 8: W's absolute sleep deadline (0 until W arms it, after saturation).
    static Q_DEADLINE: AtomicU64 = AtomicU64::new(0);
    /// When W and R armed their waits (0 until then).
    static W_ARM_AT: AtomicU64 = AtomicU64::new(0);
    static R_ARM_AT: AtomicU64 = AtomicU64::new(0);
    static HANDOFF_LOGGED: AtomicU32 = AtomicU32::new(0);
    /// Spares: set while H spawns them (a filler starting then is a spare), the next spare index,
    /// how many reached their park, per spare its word (1 = go) and state (0 parked, 1 placed,
    /// 2 a wake in flight), and how many were placed.
    static SPARE_MODE: AtomicU32 = AtomicU32::new(0);
    static SPARE_NEXT: AtomicUsize = AtomicUsize::new(0);
    static SPARES_PARKING: AtomicU32 = AtomicU32::new(0);
    static SPARE_GO: [AtomicU32; SPARES] = [const { AtomicU32::new(0) }; SPARES];
    static SPARE_STATE: [AtomicU32; SPARES] = [const { AtomicU32::new(0) }; SPARES];
    static SPARES_PLACED: AtomicU32 = AtomicU32::new(0);
    static LAST_SPARE_PASS: AtomicU64 = AtomicU64::new(0);
    static SPARE_PASS_IDLE: AtomicU32 = AtomicU32::new(0);''')
rep('''        command(PH_CAL);
        await_ack();
        Q_DEADLINE.store(mono() + Q_NS, SeqCst);
        command(PH_SAT);
        Q_ENTERING.store(1, SeqCst);
        sl("full", CLOCK_MONOTONIC, 0, Q_NS);
        await_ack();''','''        command(PH_CAL);
        await_ack();
        // SCHED-SAT-HANDOFF1: W stays runnable (yield-polling in the rotation, as R does) until H
        // has verified saturation, then arms its sleep with a deadline derived from that moment.
        command(PH_SAT);
        let mut nw = 0u32;
        while SAT_READY.load(SeqCst) == 0 && nw < BOUND {
            let _ = yield_now();
            nw += 1;
        }
        if SAT_READY.load(SeqCst) == 0 {
            fail("w_not_saturated");
        }
        let deadline = mono() + W_SLEEP_NS;
        Q_DEADLINE.store(deadline, SeqCst);
        W_ARM_AT.store(mono(), SeqCst);
        sl("full", CLOCK_MONOTONIC, TIMER_ABSTIME, deadline);
        await_ack();''')
rep('''            let i = NEXT_FILLER.load(SeqCst);
            if i >= FILLER_LIMIT {
                break;
            }''','''            let i = NEXT_FILLER.load(SeqCst);
            if i >= SPAWN_LIMIT {
                break;
            }''')
rep('''        let now = mono();
        let until = HOLD_UNTIL.load(SeqCst);
        if until != 0 && now >= until {
            end_hold();
            return;
        }
        let last = LAST_TOP.load(SeqCst);''','''        let now = mono();
        let mut until = HOLD_UNTIL.load(SeqCst);
        if until == 0 {
            // Both waits armed: hold past the later of W's deadline and R's due-by time.
            let (wd, rd) = (Q_DEADLINE.load(SeqCst), R_DUE_BY.load(SeqCst));
            if wd != 0 && rd != 0 {
                let u = wd.max(rd) + HOLD_MARGIN_NS;
                let _ = HOLD_UNTIL.compare_exchange(0, u, SeqCst, SeqCst);
                until = HOLD_UNTIL.load(SeqCst);
            }
        }
        if until != 0 && now >= until {
            end_hold();
            return;
        }
        maybe_spare_pass(now);
        let last = LAST_TOP.load(SeqCst);''')
NEW_FNS = open(sys.argv[3]).read()
rep('''    /// Case 8 for H: saturate, top up once W has left its CPU, hand R its turn, then hold the''', NEW_FNS + '''    /// Case 8 for H: saturate, top up once W has left its CPU, hand R its turn, then hold the''')
i=s.index('    fn helper_saturate() {'); j=s.index("        // Hold until past both W's and R's deadlines", i)
s=s[:i]+open(sys.argv[4]).read()+s[j:]
rep('''        // Hold until past both W's and R's deadlines (published, so any cell thread can end it).
        let until = deadline.max(R_DUE_BY.load(SeqCst)) + HOLD_MARGIN_NS;
        HOLD_UNTIL.store(until, SeqCst);
        let mut n = 0u32;
        let mut bounded = false;
        while RELEASE.load(SeqCst) == 0 {
            maybe_topup();
            if mono() >= until + HOLD_CAP_NS || n >= BOUND {''','''        // Hold until past both W's and R's deadlines (`maybe_topup` publishes the end, so any
        // cell thread can end it).
        let cap_from = deadline.max(R_DUE_BY.load(SeqCst)) + HOLD_MARGIN_NS;
        let mut n = 0u32;
        let mut bounded = false;
        while RELEASE.load(SeqCst) == 0 {
            maybe_topup();
            if mono() >= cap_from + HOLD_CAP_NS || n >= BOUND {''')
rep('''        // The pool is closed (TOPUP_ON is 0 and no round is in flight): the count is final.
        let spawned = NEXT_FILLER.load(SeqCst);''','''        // The pool is closed (TOPUP_ON is 0 and no round is in flight): the count is final.
        // Every spare still parked is woken now (bounded): the released queues have room.
        let mut sw = 0u32;
        while (SPARES_PLACED.load(SeqCst) as usize) < SPARES_PARKING.load(SeqCst) as usize
            && sw < BOUND
        {
            spare_pass();
            let _ = yield_now();
            sw += 1;
        }
        let spawned = NEXT_FILLER.load(SeqCst) + SPARES_PARKING.load(SeqCst) as usize;''')
rep('''        R_DUE_BY.store(due_by, SeqCst);
        R_DEADLINE.store(earliest, SeqCst);''','''        R_DUE_BY.store(due_by, SeqCst);
        R_DEADLINE.store(earliest, SeqCst);
        R_ARM_AT.store(mono(), SeqCst);''')
rep('''    pub(super) extern "C" fn filler_body() -> ! {
        while RELEASE.load(SeqCst) == 0 {''','''    pub(super) extern "C" fn filler_body() -> ! {
        // A filler started while H spawns the spares is spare `k`: it parks on its own word until
        // a spare pass places it (or the release frees it), then fills like any other.
        if SPARE_MODE.load(SeqCst) != 0 {
            let k = SPARE_NEXT.fetch_add(1, SeqCst);
            if k < SPARES {
                SPARES_PARKING.fetch_add(1, SeqCst);
                while SPARE_GO[k].load(SeqCst) == 0 {
                    let _ = futex_wait(SPARE_GO[k].as_ptr(), 0, P);
                }
            }
        }
        while RELEASE.load(SeqCst) == 0 {''')
assert 'Q_ENTERING' not in s and 'Q_NS' not in s
res=orig[:a]+s+orig[b:]
assert res[:a]==orig[:a] and res[a+len(s):]==orig[b:]
open(out,'w').write(res)
print("ok, module", len(orig[a:b]), "->", len(s))
