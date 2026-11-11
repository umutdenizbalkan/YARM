#!/usr/bin/env python3
"""FUTEX-WAKE-CAP1: re-derive the guards that pinned the batch futex claim.
Usage: fwc_testfix.py <worktree>"""
import pathlib, sys
T = pathlib.Path(sys.argv[1]) / "src/kernel/boot/tests.rs"
s = T.read_text()
def once(old, new, count=1):
    global s
    assert s.count(old) == count, (old[:120], s.count(old))
    s = s.replace(old, new)

# 191b: one keyed claim, reached by both wakes — now the exact claim behind a reservation.
once('''        assert!(
            EXEC_SRC.contains("super::futex::claim_futex_waiters_locked(")
                && RUNTIME_SRC.contains("crate::kernel::boot::futex::claim_futex_waiters_locked("),
            "the broad and split wakes must both reach the one keyed claim"
        );''', '''        // FUTEX-WAKE-CAP1: both wakes reach THE exact claim, and only after a reserved placement.
        for (name, src) in [("exec_state.rs", EXEC_SRC), ("runtime.rs", RUNTIME_SRC)] {
            let reserve = src
                .find("te::reserve_enqueue_on_cpu_locked(")
                .unwrap_or_else(|| panic!("{name}: the wake reserves a placement"));
            let claim = src
                .find("fx::claim_futex_waiter_locked(")
                .unwrap_or_else(|| panic!("{name}: the wake reaches the one keyed claim"));
            let commit = src
                .find("te::commit_reserved_enqueue_on_cpu_locked(")
                .unwrap_or_else(|| panic!("{name}: the wake fills the reserved slot"));
            assert!(
                reserve < claim && claim < commit,
                "{name}: reserve, then claim, then the reserved commit"
            );
            assert!(
                !src.contains("claim_futex_waiters_locked("),
                "{name}: the claim-everything-then-enqueue batch may not come back"
            );
        }''')

# AP-SCHED1: the reserved commit is an enqueue commit, and it is announced.
once('''            for pat in [".commit_enqueue_split(&plan)", ".enqueue_with_priority("] {''',
     '''            // FUTEX-WAKE-CAP1: the futex wake's reserved commit is one too.
            for pat in [
                ".commit_enqueue_split(&plan)",
                ".enqueue_with_priority(",
                "te::commit_reserved_enqueue_on_cpu_locked(c, &plan)",
            ] {''')

# 199D census: the claim's new name and neighbourhood.
once('''            "src/kernel/boot/futex.rs",
            "claim_futex_waiters_locked",
            1,
            Verdict::Cannot,''', '''            "src/kernel/boot/futex.rs",
            "claim_futex_waiter_locked",
            1,
            Verdict::Cannot,''')
once('''            "src/kernel/boot/futex.rs",
            "claim_futex_waiters_locked",
            "tcb.status",
            "TaskStatus::Runnable",
            "Some(tcb) => {",
            "out[n] = rec.tid;",''', '''            // FUTEX-WAKE-CAP1: one exact claim per waiter, after its placement is reserved: the
            // write follows the token-exact TCB match and precedes the record's retirement.
            "src/kernel/boot/futex.rs",
            "claim_futex_waiter_locked",
            "tcb.status",
            "TaskStatus::Runnable",
            "};",
            "table.retire(slot);",''')
once('''                "futex.rs",
                "claim_futex_waiters_locked",
                "t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))",
                "let token = table.token(slot);",''', '''                // FUTEX-WAKE-CAP1: the TID now arrives as a candidate, but the claim acts only
                // when the candidate names the CURRENT incarnation of a record on the woken key,
                // and that record's TCB holds exactly its token.
                "futex.rs",
                "claim_futex_waiter_locked",
                "t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))",
                "table.token(slot) != candidate.token",''')
T.write_text(s)
G = pathlib.Path(sys.argv[1]) / "tests/sched_lock1_grader_cli.rs"
g = G.read_text()
old = '''    assert_eq!(claims, 5, "claim sites");'''
if old in g:
    g = g.replace(old, '''    // FUTEX-WAKE-CAP1: 5 -> 6. The futex wake no longer commits through
    // `commit_enqueue_split`; its reserved commit is a commit site of its own and claims for
    // itself, so the witness still sees every futex wake's placement.
    assert_eq!(claims, 6, "claim sites");''')
    G.write_text(g)
print("fwc testfix applied")
