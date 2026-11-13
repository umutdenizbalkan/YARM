"""FUTEX-TIME1: every re-derivation of an existing tests.rs guard, applied in one place."""
import pathlib, sys
p=pathlib.Path(sys.argv[1]) / "src/kernel/boot/tests.rs"; s=p.read_text()
def rep(old,new,count=1):
    global s
    assert s.count(old)==count,(old[:90],s.count(old)); s=s.replace(old,new)
# (0) the test helper's exhaustive class match
rep('''                    BlockedSyscallClass::IpcRecv => t.blocked_recv_generation,
                    BlockedSyscallClass::IpcSend => t.blocked_send_generation,
                };''','''                    BlockedSyscallClass::IpcRecv => t.blocked_recv_generation,
                    BlockedSyscallClass::IpcSend => t.blocked_send_generation,
                    BlockedSyscallClass::FutexWait => t.futex_wait_generation,
                };''')
# (1) the status-writer census: the timeout claim is a new CANNOT writer
rep('''            ("src/kernel/boot/futex.rs", 3),''','''            // FUTEX-TIME1: 3 -> 4 — the timeout claim (`claim_futex_timeout_locked`).
            ("src/kernel/boot/futex.rs", 4),''')
rep('''        (
            "src/kernel/boot/futex.rs",
            "claim_futex_waiter_locked",
            1,
            Verdict::Cannot,
        ),
    ];''','''        (
            "src/kernel/boot/futex.rs",
            "claim_futex_waiter_locked",
            1,
            Verdict::Cannot,
        ),
        // FUTEX-TIME1: THE timeout claim — `Blocked(Futex(token)) -> Runnable` for the exact
        // cycle a collector candidate names, behind the same token-exact TCB filter as the wake
        // claim, so it can never act on a `Blocked(EndpointReceive)` task either.
        (
            "src/kernel/boot/futex.rs",
            "claim_futex_timeout_locked",
            1,
            Verdict::Cannot,
        ),
    ];''')
rep('''            // (both INTO_BLOCKED) are one keyed registration; the undo is unchanged in class.
            (15, 18, 9, 2, 1)''','''            // (both INTO_BLOCKED) are one keyed registration; the undo is unchanged in class.
            //
            // FUTEX-TIME1: CANNOT 18 -> 19. The timeout claim is a new terminal owner of a futex
            // wait cycle, CANNOT for the same reason as the wake claim (token-exact filter).
            (15, 19, 9, 2, 1)''')
rep('''            "src/kernel/boot/futex.rs",
            "claim_futex_waiter_locked",
            "tcb.status",
            "TaskStatus::Runnable",
            "};",
            "table.retire(slot);",
        ),''','''            "src/kernel/boot/futex.rs",
            "claim_futex_waiter_locked",
            "tcb.status",
            "TaskStatus::Runnable",
            "};",
            "table.retire(slot);",
        ),
        (
            // FUTEX-TIME1: the timeout claim, the wake claim's twin for the deadline owner: the
            // write follows the token-exact TCB match and precedes the record's retirement, and
            // the `TimedOut` completion is published after both, in the same hold.
            "src/kernel/boot/futex.rs",
            "claim_futex_timeout_locked",
            "tcb.status",
            "TaskStatus::Runnable",
            "};",
            "table.retire(slot);",
        ),''')
rep('''            sites.len(),
            35,
            "FUTEX-FOUND1: 37 -> 35''','''            sites.len(),
            36,
            "FUTEX-TIME1: 35 -> 36 — the timeout claim `claim_futex_timeout_locked` is a genuinely \\
             new writer: a new terminal owner of a futex wait cycle. \\
             FUTEX-FOUND1: 37 -> 35''')
rep('''                "futex.rs",
                "claim_futex_waiter_locked",
                "t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))",
                "table.token(slot) != candidate.token",
            ),''','''                "futex.rs",
                "claim_futex_waiter_locked",
                "t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))",
                "table.token(slot) != candidate.token",
            ),
            // FUTEX-TIME1: the timeout claim. Same guard; the closure fact is that the candidate's
            // token was read by the collector from a live timed record, and the claim acts only
            // when it is still that record's current incarnation.
            (
                "futex.rs",
                "claim_futex_timeout_locked",
                "t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))",
                "table.token(slot) != candidate.token",
            ),''')
# (2) g04: the parked answer of a TIMED wait is the fail-closed provisional
rep('''            assert!(parked.trim_end().ends_with("frame.set_ok(1, 0, 0);"));''','''            // FUTEX-TIME1 re-derivation: `1` is the parked answer of an UNTIMED wait only — its
            // one outcome is a wake. A TIMED wait parks a fail-closed provisional (`Internal`,
            // never `1`), and its terminal claim parks the real result as its completion.
            assert!(parked.trim_end().ends_with(
                "if deadline.is_some() {\\n                frame.set_err(SyscallError::Internal.code());\\n            } else {\\n                frame.set_ok(1, 0, 0);\\n            }"
            ));''')
# (3) both admit -> reserve -> publish guards: THE park transaction is the timed one
rep('''            .find("futex_wait_park_exact_split(cpu, tid, asid, addr, expected, args.kind)")''','''            // FUTEX-TIME1 re-derivation: THE park transaction is the timed one, carrying the
            // deadline the route converted at admission (`None` for an untimed wait).
            .find(
                "futex_wait_park_exact_timed_split(cpu, tid, asid, addr, expected, args.kind, deadline)",
            )''', 2)
# (4) 192A: the shared broad body answers the typed blocked outcome
rep('''                && EXEC_SRC.contains("return Ok(true);"),
            "futex_wait_current must defer the queue-advancing dispatch when eligible"''','''                // FUTEX-TIME1 re-derivation: the shared body answers the typed blocked outcome
                // (`futex_wait_current` maps it to `Ok(true)`), timed or not.
                && EXEC_SRC.contains("return Ok(super::FutexWaitBroadOutcome::Blocked { timed });"),
            "futex_wait_current must defer the queue-advancing dispatch when eligible"''')
p.write_text(s); print("rederived")
