import pathlib
p=pathlib.Path("src/kernel/boot/tests.rs"); s=p.read_text()
def rep(old,new,count=1):
    global s
    assert s.count(old)==count,(old[:90],s.count(old)); s=s.replace(old,new)
rep('''            assert!(parked.trim_end().ends_with("frame.set_ok(1, 0, 0);"));''','''            // FUTEX-TIME1 re-derivation: `1` is the parked answer of an UNTIMED wait only — its
            // one outcome is a wake. A TIMED wait parks a fail-closed provisional (`Internal`,
            // never `1`), and its terminal claim parks the real result as its completion.
            assert!(parked.trim_end().ends_with(
                "if deadline.is_some() {\\n                frame.set_err(SyscallError::Internal.code());\\n            } else {\\n                frame.set_ok(1, 0, 0);\\n            }"
            ));''')
rep('''            .find("futex_wait_park_exact_split(cpu, tid, asid, addr, expected, args.kind)")''','''            // FUTEX-TIME1 re-derivation: THE park transaction is the timed one, carrying the
            // deadline the route converted at admission (`None` for an untimed wait).
            .find(
                "futex_wait_park_exact_timed_split(cpu, tid, asid, addr, expected, args.kind, deadline)",
            )''', 2)
rep('''                && EXEC_SRC.contains("return Ok(true);"),
            "futex_wait_current must defer the queue-advancing dispatch when eligible"''','''                // FUTEX-TIME1 re-derivation: the shared body answers the typed blocked outcome
                // (`futex_wait_current` maps it to `Ok(true)`), timed or not.
                && EXEC_SRC.contains("return Ok(super::FutexWaitBroadOutcome::Blocked { timed });"),
            "futex_wait_current must defer the queue-advancing dispatch when eligible"''')
p.write_text(s); print("ok")
