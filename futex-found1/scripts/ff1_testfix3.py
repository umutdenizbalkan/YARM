#!/usr/bin/env python3
"""FUTEX-FOUND1: re-derive the remaining tests that pinned the address-only futex contract.
Usage: ff1_testfix3.py <worktree>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1])
T = wt / "src/kernel/boot/tests.rs"
s = T.read_text()
def once(old, new, count=1):
    global s
    assert s.count(old) == count, (old[:120], s.count(old))
    s = s.replace(old, new)

# ── futex_wait_blocks_current_and_wake_requeues_waiter ───────────────────────────────────────
if 'a private wake from another address space must not reach this waiter' not in s: once('''    assert_eq!(
        state.task_status(1),
        Some(TaskStatus::Blocked(WaitReason::Futex(
            crate::kernel::boot::futex::FutexWaitToken {
                slot: 0,
                generation: 0
            }
        )))
    );
    assert_eq!(
        state
            .futex_wake(0x1000, 1, crate::kernel::boot::futex::FutexKind::Private)
            .expect("wake"),
        1
    );
    assert_eq!(state.task_status(1), Some(TaskStatus::Runnable));
}
''', '''    assert!(state.futex_parked_at_va_for_test(1, 0x1000));
    // FUTEX-FOUND1 re-derivation. This woke task 1 from the idle task — a DIFFERENT address
    // space that happens to map the same object at the same VA — with a private wake, and
    // expected it to succeed: an address-only match, which is the cross-process wake defect. A
    // private key is the address space's own, so that wake finds nobody and task 1 stays parked.
    assert_eq!(
        state
            .futex_wake(0x1000, 1, crate::kernel::boot::futex::FutexKind::Private)
            .expect("wake"),
        0,
        "a private wake from another address space must not reach this waiter"
    );
    assert!(state.futex_parked_at_va_for_test(1, 0x1000));
    // A wake in task 1's OWN namespace reaches it, and requeues it.
    assert_eq!(state.futex_wake_on_exit(1, 0x1000).expect("wake"), 1);
    assert_eq!(state.task_status(1), Some(TaskStatus::Runnable));
    assert_eq!(state.futex_waiter_count(0x1000), 0);
}
''')
T.write_text(s)
print("testfix3 applied")
s = T.read_text()

# ── 191b: the split and broad wakes share ONE claim ─────────────────────────────────────────
once('''        // Same wake predicate + Runnable transition as legacy futex_wake_inner.
        assert!(
            EXEC_SRC.contains(
                "if tcb.status != TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)))"
            ) && RUNTIME_SRC.contains(
                "if tcb.status != TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)))"
            ),
            "the split wake scan predicate must match legacy futex_wake_inner"
        );''', '''        // FUTEX-FOUND1 re-derivation. This pinned two copies of an address-only predicate
        // (`Blocked(Futex(VirtAddr(addr)))`) and checked that they resembled each other; that
        // predicate is the cross-process wake. Both owners now reach THE keyed claim, so the
        // woken set, count and order are the same by construction, not by resemblance.
        assert!(
            EXEC_SRC.contains("super::futex::claim_futex_waiters_locked(")
                && RUNTIME_SRC.contains("crate::kernel::boot::futex::claim_futex_waiters_locked("),
            "the broad and split wakes must both reach the one keyed claim"
        );
        for (name, src) in [("exec_state.rs", EXEC_SRC), ("runtime.rs", RUNTIME_SRC)] {
            assert!(
                !src.contains("WaitReason::Futex(VirtAddr("),
                "{name}: the address-only futex predicate may not come back"
            );
        }''')

# ── 191d value check: one range policy, one predicate, no `observed` ─────────────────────────
once('''        const SCHED_SRC: &str = include_str!("../syscall/sched.rs");
        assert!(
            SCHED_SRC.contains("pub(crate) fn futex_word_range_check(")
                && SCHED_SRC.contains("return Err(KernelError::WrongObject);")
                && SCHED_SRC.contains(">= crate::kernel::vm::KERNEL_SPACE_BASE"),
            "the one futex-word range policy must exist and keep the canonical answers"
        );''', '''        const SCHED_SRC: &str = include_str!("../syscall/sched.rs");
        const FUTEX_SRC: &str = include_str!("futex.rs");
        // FUTEX-FOUND1: the policy lives with the key resolver; the syscall layer delegates.
        assert!(
            SCHED_SRC.contains("pub(crate) fn futex_word_range_check(")
                && SCHED_SRC.contains("crate::kernel::boot::futex::futex_word_range_check(addr)")
                && FUTEX_SRC.contains("pub fn futex_word_range_check(")
                && FUTEX_SRC.contains("return Err(KernelError::WrongObject);")
                && FUTEX_SRC.contains(">= crate::kernel::vm::KERNEL_SPACE_BASE"),
            "the one futex-word range policy must exist and keep the canonical answers"
        );''')
once('''        // BL3b re-derivation. This pinned the decision as the caller's two arguments compared
        // with each other, which is exactly the lost wake: a waker that stores and wakes between
        // the caller's read and its `FutexWait` finds no waiter, and the kernel then parks the
        // caller on a word that has already moved. The decision is now the ONE predicate both
        // owners call — the word the kernel reads must still hold `expected` — and the caller's
        // own fast path (`expected != observed` returns at once) is kept inside it.
        assert!(
            SCHED_SRC.contains(
                "pub(crate) fn futex_should_park(expected: u32, observed: u32, word: u32) -> bool {"
            ) && SCHED_SRC.contains("expected == observed && word == expected"),
            "the one park predicate compares the word the kernel read against `expected`"
        );
        for (name, src) in [("runtime.rs", RUNTIME_SRC), ("exec_state.rs", EXEC_SRC)] {
            assert!(
                src.contains("futex_should_park(expected, observed, word)"),
                "{name}: the decision must be the shared predicate over the word read"
            );''', '''        // BL3b re-derivation, and FUTEX-FOUND1 after it. The decision is the ONE predicate both
        // owners call — the word the kernel reads must still hold `expected` — and the ABI no
        // longer carries a caller-supplied `observed` at all, so there is no second value for a
        // caller copy to compare.
        assert!(
            SCHED_SRC.contains("pub(crate) fn futex_should_park(expected: u32, word: u32) -> bool {")
                && SCHED_SRC.contains("    word == expected\\n"),
            "the one park predicate compares the word the kernel read against `expected`"
        );
        for (name, src) in [("runtime.rs", RUNTIME_SRC), ("exec_state.rs", EXEC_SRC)] {
            assert!(
                src.contains("futex_should_park(expected, word)"),
                "{name}: the decision must be the shared predicate over the word read"
            );
        }
        for (name, src, owner) in [
            ("runtime.rs", RUNTIME_SRC, "pub fn futex_wait_decide_split_read("),
            ("runtime.rs", RUNTIME_SRC, "pub(crate) fn futex_wait_park_exact_split("),
            ("exec_state.rs", EXEC_SRC, "pub fn futex_wait_current("),
        ] {
            let body = src
                .split(owner)
                .nth(1)
                .and_then(|b| b.split("\n    }\n").next())
                .expect("the futex owner");
            assert!(
                !body.contains("observed"),
                "{name}: no futex owner may consult a caller-supplied `observed`"
            );''')

# ── 191d block publish: the keyed registration, undo and read-back ──────────────────────────
once('''        // Same Blocked(Futex(addr)) transition as legacy futex_wait_current. The exact
        // transaction BINDS it once and uses the same value three times — to register, to
        // recognize its own registration when undoing it, and to read back whether a waker moved
        // the TCB off it — so the three can never come to disagree about what "parked" means.
        assert!(
            EXEC_SRC.contains(
                "tcb.status = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)));"
            ),
            "legacy futex_wait_current must keep its transition"
        );
        assert!(
            RUNTIME_SRC.contains(
                "let blocked = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)));"
            ) && park.contains("tcb.status = blocked;"),
            "the park transaction's transition must match legacy futex_wait_current"
        );
        // BL3b: the undo is now ONE closure shared by the clear refusal and the value re-check,
        // so the count is unchanged — the undo and the read-back.
        assert_eq!(
            park.matches("status == blocked").count(),
            2,
            "and the SAME value must be what the undo and the read-back compare against"
        );''', '''        // FUTEX-FOUND1 re-derivation. The transition is no longer written by either route: both
        // reach THE registration owner, which writes the keyed record and
        // `Blocked(Futex(token))` in one task-lock critical section. The exact transaction then
        // uses that one TOKEN twice — to undo its own registration, and to read back whether a
        // waker claimed it — so the three can never disagree about what "parked" means.
        const FUTEX_SRC: &str = include_str!("futex.rs");
        assert_eq!(
            FUTEX_SRC
                .matches("tcb.status = TaskStatus::Blocked(WaitReason::Futex(token));")
                .count(),
            1,
            "one registration owner writes the futex-blocked status"
        );
        assert!(
            EXEC_SRC.contains("super::futex::register_futex_waiter_locked(")
                && park.contains(
                    "fx::register_futex_waiter_locked(tcbs, table, tid, asid, key, addr as u64)"
                ),
            "both routes must register through the one owner"
        );
        assert!(
            park.contains("fx::undo_futex_registration_locked(tcbs, table, tid, token)")
                && park.contains("fx::futex_still_parked_locked(tcbs, tid, token)"),
            "and the SAME token must be what the undo and the read-back compare against"
        );
        assert_eq!(
            park.matches("undo_futex_registration_locked(").count(),
            1,
            "BL3b: ONE undo closure, shared by the clear refusal and the value re-check"
        );''')

# ── u9fw / u9qa: the park call carries the decoded kind ─────────────────────────────────────
once('''            .find("futex_wait_park_exact_split(cpu, tid, asid, addr, expected)")''',
     '''            .find("futex_wait_park_exact_split(cpu, tid, asid, addr, expected, args.kind)")''', 2)
once('''            .find("FUTEX_WAIT_BROAD_ENTRY")
            .expect("the census is inside the handler");
        let first_read = handler
            .find("frame.arg(SYSCALL_ARG_CAP)")
            .expect("the handler's first argument read");''', '''                    .find("FUTEX_WAIT_BROAD_ENTRY")
            .expect("the census is inside the handler");
        // FUTEX-FOUND1: the first argument read is THE shared decode.
        let first_read = handler
            .find("decode_futex_args(frame)")
            .expect("the handler's first argument read");''')
once('''        // BL3b: five, not three — the value re-check adds `ValueChanged` and `WordUnreadable`
        // beside `WokenDuringPublication`, `VictimChanged` and `IncarnationMoved`, and each is a
        // non-parking outcome that must release what it reserved.
        assert_eq!(
            after.matches("futex_wait_dispatch_clear(cpu_idx)").count(),
            5,
            "the five non-parking park outcomes each release the reservation"
        );''', '''        // BL3b: five, not three — the value re-check adds `ValueChanged` and `WordUnreadable`
        // beside `WokenDuringPublication`, `VictimChanged` and `IncarnationMoved`, and each is a
        // non-parking outcome that must release what it reserved. FUTEX-FOUND1: six — the keyed
        // registration's `CapacityExhausted` refusal is a sixth, decided before any write.
        assert_eq!(
            after.matches("futex_wait_dispatch_clear(cpu_idx)").count(),
            6,
            "the six non-parking park outcomes each release the reservation"
        );''')

# ── bl3a: the oracle hand-offs, in the scoped ABI ───────────────────────────────────────────
once('''        for bare in [
            "futex_wait(handshake, hv, hv)",
            "futex_wait(started, sv, sv)",
            "futex_wake(handshake, 1)",
            "futex_wake(started, 1)",
            "let first_wake = yarm_user_rt::syscall::futex_wake(addr, 1)",
        ] {''', '''        // FUTEX-FOUND1: the calls carry a scope now, so the bare forms are pinned by their prefix
        // (any scope), and the bodies below are compared with whitespace removed.
        for bare in [
            "futex_wait(handshake,",
            "futex_wait(started,",
            "futex_wake(handshake,",
            "futex_wake(started,",
            "let first_wake = yarm_user_rt::syscall::futex_wake(addr",
        ] {''')
once('''        let ask = "let woke = yarm_user_rt::syscall::futex_wake(word, 1)";''',
     '''        let ask = "yarm_user_rt::syscall::futex_wake(word, 1, yarm_user_rt::syscall::FutexScope::Private)";''')
once('''        let store = publish.find("word.store(ready").expect("store");
        let wake = publish.find("futex_wake(word.as_ptr(), 1)").expect("wake");''', '''        let publish: alloc::string::String = publish.split_whitespace().collect();
        let store = publish.find("word.store(ready").expect("store");
        let wake = publish
            .find("futex_wake(word.as_ptr(),1,yarm_user_rt::syscall::FutexScope::Private")
            .expect("wake");''')
once('''        assert!(
            await_body.contains("if v == ready {")
                && await_body.contains("futex_wait(word.as_ptr(), v, v)"),
            "the waiter parks only on the value it just saw unpublished"
        );''', '''        let await_body: alloc::string::String = await_body.split_whitespace().collect();
        assert!(
            await_body.contains("ifv==ready{")
                && await_body
                    .contains("futex_wait(word.as_ptr(),v,yarm_user_rt::syscall::FutexScope::Private"),
            "the waiter parks only on the value it just saw unpublished"
        );''')

# ── 195c / 196e: the oracle children in the scoped ABI ──────────────────────────────────────
once('''        assert!(
            INIT_SRC.contains("futex_wait(addr, observed, observed)"),
            "the oracle child must block through the legacy FutexWait path"
        );''', '''        // FUTEX-FOUND1: `expected` is the value the child observed, in its own (private) scope.
        let flat: alloc::string::String = INIT_SRC.split_whitespace().collect();
        assert!(
            flat.contains("futex_wait(addr,observed,yarm_user_rt::syscall::FutexScope::Private"),
            "the oracle child must block through the legacy FutexWait path"
        );''')
once('''        assert!(
            INIT_SRC.contains("yarm_user_rt::syscall::futex_wake(word, 1)"),
            "the child must wake A through split FutexWake"
        );''', '''        let flat: alloc::string::String = INIT_SRC.split_whitespace().collect();
        assert!(
            flat.contains(
                "yarm_user_rt::syscall::futex_wake(word,1,yarm_user_rt::syscall::FutexScope::Private)"
            ),
            "the child must wake A through split FutexWake"
        );''')

# ── 197: FutexWait publishes Blocked(Futex) — through the registration owner ────────────────
once('''        assert!(
            EXEC_STATE_SRC
                .contains("TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)))"),
            "FutexWait must publish Blocked(Futex)"
        );''', '''        // FUTEX-FOUND1: through THE registration owner, which writes the keyed token.
        assert!(
            EXEC_STATE_SRC.contains("super::futex::register_futex_waiter_locked(")
                && include_str!("futex.rs")
                    .contains("tcb.status = TaskStatus::Blocked(WaitReason::Futex(token));"),
            "FutexWait must publish Blocked(Futex)"
        );''')
T.write_text(s)
print("testfix3 guards applied")
