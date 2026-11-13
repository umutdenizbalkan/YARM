"""FUTEX-TIME1: the census totals (second half of the tests.rs re-derivations)."""
import pathlib, sys, re
p=pathlib.Path(sys.argv[1]) / "src/kernel/boot/tests.rs"; s=p.read_text()
def rep(old,new,count=1):
    global s
    assert s.count(old)==count,(old[:90],s.count(old)); s=s.replace(old,new)
# the TCB constructor literal now carries the futex wait generation
rep("blocked_send_generation: 0, pending_syscall_completion: None, async_preempted: None,",
    "blocked_send_generation: 0, futex_wait_generation: 0, pending_syscall_completion: None, async_preempted: None,")
# the classified total and the partition: 45 -> 46
rep('''            CENSUS.iter().map(|(_, _, c, _)| c).sum::<usize>(),
            45,
            "FUTEX-FOUND1: 47 -> 45''','''            CENSUS.iter().map(|(_, _, c, _)| c).sum::<usize>(),
            46,
            "FUTEX-TIME1: 45 -> 46, the timeout claim is a new CANNOT writer. \\
             FUTEX-FOUND1: 47 -> 45''')
rep('''        assert_eq!(
            can + cannot + into_blocked + fresh + non_production + unproven,
            45,''','''        // FUTEX-TIME1: 45 -> 46 — CANNOT 18 -> 19 (the timeout claim).
        assert_eq!(
            can + cannot + into_blocked + fresh + non_production + unproven,
            46,''')
rep('''            // FUTEX-FOUND1: 41 -> 39 — five futex writes became the three keyed owners.
            39,
            "39 raw writes —''','''            // FUTEX-FOUND1: 41 -> 39 — five futex writes became the three keyed owners.
            // FUTEX-TIME1: 39 -> 40 — the timeout claim.
            40,
            "40 raw writes — FUTEX-TIME1 added the timeout claim `claim_futex_timeout_locked`, \\
             39 -> 40, a new terminal owner of a futex wait cycle — \\
             39 before it —''')
p.write_text(s); print("rederived2")
