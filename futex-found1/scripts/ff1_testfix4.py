#!/usr/bin/env python3
"""FUTEX-FOUND1: re-derive the 199D status-writer censuses for the keyed futex owners.
Usage: ff1_testfix4.py <worktree>"""
import pathlib, sys
T = pathlib.Path(sys.argv[1]) / "src/kernel/boot/tests.rs"
s = T.read_text()
def once(old, new, count=1):
    global s
    assert s.count(old) == count, (old[:140], s.count(old))
    s = s.replace(old, new)

WHY = '''FUTEX-FOUND1: the five futex status writes — the broad
            // registration and wake (exec_state.rs), and the split registration, its undo and the
            // split wake (runtime.rs) — are now THREE, in the one keyed owner module both routes
            // reach: `register_futex_waiter_locked`, `undo_futex_registration_locked` and
            // `claim_futex_waiters_locked` (futex.rs). Nothing multiplied: the two copies of the
            // registration and of the wake became one each.'''

# ── WA2A: per-file counts ───────────────────────────────────────────────────────────────────
once('''            ("src/kernel/boot/exec_state.rs", 4),''', f'''            // {WHY} exec_state.rs 4 -> 2.
            ("src/kernel/boot/exec_state.rs", 2),''')
once('''            ("src/kernel/boot/exit_claim.rs", 3),''', '''            ("src/kernel/boot/exit_claim.rs", 3),
            // FUTEX-FOUND1: the keyed futex owners (see exec_state.rs above).
            ("src/kernel/boot/futex.rs", 3),''')
once('''            ("src/runtime.rs", 12),
        ];''', '''            // FUTEX-FOUND1: 12 -> 9 — the split registration, its undo and the split wake moved
            // into the keyed owners in futex.rs.
            ("src/runtime.rs", 9),
        ];''')
once('''        assert_eq!(
            found.iter().map(|(_, n)| n).sum::<usize>(),
            41,''', '''        assert_eq!(
            found.iter().map(|(_, n)| n).sum::<usize>(),
            // FUTEX-FOUND1: 41 -> 39 — five futex writes became the three keyed owners.
            39,''')

# ── WA2B CENSUS rows ────────────────────────────────────────────────────────────────────────
once('''        (
            "src/kernel/boot/exec_state.rs",
            "futex_wait_current",
            1,
            Verdict::IntoBlocked,
        ),
        (
            "src/kernel/boot/exec_state.rs",
            "futex_wake_inner",
            1,
            Verdict::Cannot,
        ),
''', '''        // FUTEX-FOUND1: `futex_wait_current` and `futex_wake_inner` no longer write the status;
        // they reach the keyed owners in futex.rs (rows below).
''')
once('''        ("src/runtime.rs", "futex_wake_split_mut", 1, Verdict::Cannot),
''', '')
once('''        (
            // U9-FUTEX-WAIT-FINAL §3 — two sites, and the second is the exact undo of the first:
            // a registration this transaction cannot complete is taken back rather than left
            // standing. Both are `IntoBlocked`'s cohort — they enter and leave the futex-waiter
            // state and wake nobody.
            "src/runtime.rs",
            "futex_wait_park_exact_split",
            2,
            Verdict::IntoBlocked,
        ),
    ];''', '''        // FUTEX-FOUND1: `futex_wait_park_exact_split`'s registration and undo, and
        // `futex_wake_split_mut`'s wake, moved into the keyed owners in futex.rs (rows below).
        // ── futex.rs (3) ────────────────────────────────────────────────────────────────────
        // THE registration: `Running -> Blocked(Futex(token))`, for the exact `{tid, asid}`
        // incarnation, with its keyed record in the same task-lock critical section. Both routes
        // reach it. It moves a task INTO a futex wait and can write nothing else.
        (
            "src/kernel/boot/futex.rs",
            "register_futex_waiter_locked",
            1,
            Verdict::IntoBlocked,
        ),
        // Its exact UNDO (U9-FUTEX-WAIT-FINAL §3's, now keyed): `Blocked(Futex(token)) ->
        // Running`, only for the TCB still holding this registration's own token. Same class as
        // the registration it reverses — it enters and leaves the futex-waiter state and wakes
        // nobody.
        (
            "src/kernel/boot/futex.rs",
            "undo_futex_registration_locked",
            1,
            Verdict::IntoBlocked,
        ),
        // THE wake claim: `Blocked(Futex(token)) -> Runnable`, for a TCB holding exactly the
        // token of a record on the woken key. It can never act on a `Blocked(EndpointReceive)`
        // task (the proof is in `every_cannot_row_pins_its_guard_and_its_caller_closure`).
        (
            "src/kernel/boot/futex.rs",
            "claim_futex_waiters_locked",
            1,
            Verdict::Cannot,
        ),
    ];''')

# ── WA2B fingerprints ───────────────────────────────────────────────────────────────────────
once('''        (
            "src/kernel/boot/exec_state.rs",
            "futex_wait_current",
            "tcb.status",
            "TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)))",
            ".ok_or(KernelError::TaskMissing)?;",
            "Ok::<_, KernelError>(())",
        ),
        (
            "src/kernel/boot/exec_state.rs",
            "futex_wake_inner",
            "tcb.status",
            "TaskStatus::Runnable",
            "}",
            "wake_tids[wake_count] = Some(tcb.tid.0);",
        ),
''', '')
once('''        (
            "src/runtime.rs",
            "futex_wake_split_mut",
            "tcb.status",
            "TaskStatus::Runnable",
            "}",
            "woken[n] = (tcb.tid.0, tcb.cpu_affinity);",
        ),
''', '')
once('''        (
            // The REGISTRATION: exact incarnation, from `Running` only.
            "src/runtime.rs",
            "futex_wait_park_exact_split",
            "tcb.status",
            "blocked",
            "{",
            "true",
        ),
        (
            // Its exact UNDO, taken only when the rank-1 compare-and-clear refuses and only for a
            // TCB still holding this transaction's own registration.
            "src/runtime.rs",
            "futex_wait_park_exact_split",
            "tcb.status",
            "TaskStatus::Running",
            "Some(tcb) if tcb.status == blocked => {",
            "true",
        ),
    ];''', '''    ];''')
# futex.rs sorts after exit_claim.rs: insert before the first ipc_state.rs fingerprint.
FIRST_IPC = '''        (
            "src/kernel/boot/ipc_state.rs",'''
i = s.index("const FINGERPRINTS: &[(&str, &str, &str, &str, &str, &str)] = &[")
j = s.index(FIRST_IPC, i)
s = s[:j] + '''        // FUTEX-FOUND1: the keyed owners. The registration writes the token it just minted,
        // after resolving the exact incarnation; the undo writes only after finding the TCB on
        // exactly that token; the claim writes only for a record's own token, and records the
        // claimed TID next.
        (
            "src/kernel/boot/futex.rs",
            "register_futex_waiter_locked",
            "tcb.status",
            "TaskStatus::Blocked(WaitReason::Futex(token))",
            "if let Some(tcb) = tcbs[tcb_idx].as_mut() {",
            "}",
        ),
        (
            "src/kernel/boot/futex.rs",
            "undo_futex_registration_locked",
            "tcb.status",
            "TaskStatus::Running",
            "};",
            "let slot = usize::from(token.slot);",
        ),
        (
            "src/kernel/boot/futex.rs",
            "claim_futex_waiters_locked",
            "tcb.status",
            "TaskStatus::Runnable",
            "Some(tcb) => {",
            "out[n] = rec.tid;",
        ),
''' + s[j:]

# ── WA2B totals ─────────────────────────────────────────────────────────────────────────────
once('''        assert_eq!(
            sites.len(),
            37,
            "36 raw writes (''', '''        assert_eq!(
            sites.len(),
            35,
            "FUTEX-FOUND1: 37 -> 35 — the five futex writes (two broad, three split) became the \\
             three keyed owners in futex.rs, one registration, its undo and one wake claim. \\
             36 raw writes (''')
once('''        assert_eq!(
            CENSUS.iter().map(|(_, _, c, _)| c).sum::<usize>(),
            47,
            "U9-FUTEX-WAIT-FINAL''', '''        assert_eq!(
            CENSUS.iter().map(|(_, _, c, _)| c).sum::<usize>(),
            45,
            "FUTEX-FOUND1: 47 -> 45, the five futex writes became three keyed owners. \\
             U9-FUTEX-WAIT-FINAL''')
once('''                    .sum::<usize>(),
            47,
            "U9-FUTEX-WAIT-FINAL §3 added the futex registration's exact undo, 46 -> 47 — the \\''',
     '''                    .sum::<usize>(),
            45,
            "FUTEX-FOUND1: 47 -> 45, the five futex writes became three keyed owners. \\
             U9-FUTEX-WAIT-FINAL §3 added the futex registration's exact undo, 46 -> 47 — the \\''')
once('''        assert_eq!(
            can + cannot + into_blocked + fresh + non_production + unproven,
            47,''', '''        // FUTEX-FOUND1: 47 -> 45 — CANNOT 19 -> 18 (the two wake copies are one claim) and
        // INTO_BLOCKED 10 -> 9 (the two registrations are one; the undo stays).
        assert_eq!(
            can + cannot + into_blocked + fresh + non_production + unproven,
            45,''')
once('''            (15, 19, 10, 2, 1)
        );''', '''            //
            // FUTEX-FOUND1: CANNOT 19 -> 18 and INTO_BLOCKED 10 -> 9. The broad and split wake
            // copies (both CANNOT) are one keyed claim, and the broad and split registrations
            // (both INTO_BLOCKED) are one keyed registration; the undo is unchanged in class.
            (15, 18, 9, 2, 1)
        );''')

# ── WA2B Cannot proofs ──────────────────────────────────────────────────────────────────────
once('''            (
                "exec_state.rs",
                "futex_wake_inner",
                "if tcb.status != TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64))) {",
                // value-level filter over every TCB: no parameter selects a victim
                "for tcb in tcbs.iter_mut().flatten() {",
            ),
''', '''            // FUTEX-FOUND1: the broad and split wake rows are one keyed claim. Its guard is the
            // exact token match; the closure fact is that the TID it acts on comes only from a
            // record on the woken key, never from a parameter, and that a token is written only by
            // the registration of that same record (`table.token(slot)`), so the TCB it moves is
            // in exactly the futex wait that record describes.
            (
                "futex.rs",
                "claim_futex_waiters_locked",
                "t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))",
                "let token = table.token(slot);",
            ),
''')
once('''            (
                "runtime.rs",
                "futex_wake_split_mut",
                "if tcb.status != TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64))) {",
                "for tcb in tcbs.iter_mut().flatten() {",
            ),
''', '')
once('''                "sender_waiter.rs" => include_str!("sender_waiter.rs"),''',
     '''                "sender_waiter.rs" => include_str!("sender_waiter.rs"),
                "futex.rs" => include_str!("futex.rs"),''')
T.write_text(s)
print("testfix4 applied")
