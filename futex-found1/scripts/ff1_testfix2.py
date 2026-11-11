#!/usr/bin/env python3
"""FUTEX-FOUND1: the non-mechanical test migrations (run after ff1_testfix.py).
Usage: ff1_testfix2.py <worktree>"""
import pathlib, re, sys
wt = pathlib.Path(sys.argv[1])
T = wt / "src/kernel/boot/tests.rs"
O = wt / "src/kernel/boot/overtaken_tests/tests.rs"
PRIV = "crate::kernel::boot::futex::FutexKind::Private"
s = T.read_text()

def once(a, b):
    global s
    assert s.count(a) == 1, (a[:90], s.count(a))
    s = s.replace(a, b)

# G1 — `assert_eq!(<x>.task_status(T), Some(Blocked(Futex(VirtAddr(E)))) [, msg])`
#      -> `assert!(<x>.futex_parked_at_va_for_test(T, E) [, msg])`.
def split_top(body):
    args = []; cur = ""; d = 0
    for c in body:
        if c in "([{": d += 1
        if c in ")]}": d -= 1
        if c == "," and d == 0:
            args.append(cur); cur = ""
        else:
            cur += c
    if cur.strip(): args.append(cur)
    return args
pat_status = re.compile(r"^\s*Some\(\s*TaskStatus::Blocked\(\s*WaitReason::Futex\(\s*VirtAddr\(\s*(.+?)\s*,?\s*\)\s*\)\s*\)\s*\)\s*$", re.S)
out = []; i = 0
while True:
    j = s.find("assert_eq!(", i)
    if j < 0:
        out.append(s[i:]); break
    line_start = s.rfind("\n", 0, j) + 1
    if "WaitReason::Futex(VirtAddr(" not in s[j:j + 400] or s[line_start:j].count('"') % 2 == 1:
        out.append(s[i:j + len("assert_eq!(")]); i = j + len("assert_eq!("); continue
    k = j + len("assert_eq!("); d = 1; m = k
    while d:
        c = s[m]
        if c in "([{": d += 1
        elif c in ")]}": d -= 1
        m += 1
    args = split_top(s[k:m - 1])
    mt = pat_status.match(args[1]) if len(args) >= 2 else None
    if mt and ".task_status(" in args[0]:
        expr = mt.group(1)
        first = args[0]
        a = first.index(".task_status(") + len(".task_status(")
        dd = 1; b = a
        while dd:
            if first[b] == "(": dd += 1
            elif first[b] == ")": dd -= 1
            b += 1
        new_first = first[:first.index(".task_status(")] + ".futex_parked_at_va_for_test(" + \
            first[a:b - 1] + ", " + expr + ")" + first[b:]
        rest = ",".join(args[2:])
        out.append(s[i:j]); out.append("assert!(" + new_first + ("," + rest if rest.strip() else "") + ")")
    else:
        out.append(s[i:m])
    i = m
s = "".join(out)

# G2 — the boolean comparison inside a filter.
s, n = re.subn(r"k\.task_status\(t\)\s*==\s*Some\(TaskStatus::Blocked\(WaitReason::Futex\(VirtAddr\(\s*addr as u64,?\s*\)\)\)\)",
               "k.futex_parked_at_va_for_test(t, addr as u64)", s)
assert n == 1, n

# G3 — in-closure injection loops -> the table-backed fixture.
s, n = re.subn(r"state\.with_tcbs_mut\(\|tcbs\| \{\s*for tcb in tcbs\.iter_mut\(\)\.flatten\(\) \{\s*if tcb\.tid\.0 == (\w+) \{\s*tcb\.status = TaskStatus::Blocked\(WaitReason::Futex\(VirtAddr\(addr as u64\)\)\);\s*\}\s*\}\s*\}\);",
               r"state.inject_futex_waiter_for_test(\1, addr);", s)
assert n == 3, n

# futex_wake_split_semantics_are_correct: inject the four waiters, keep the Poll task, wake by key.
once('''        state.with_tcbs_mut(|tcbs| {
            for tcb in tcbs.iter_mut().flatten() {
                if waiters.contains(&tcb.tid.0) {
                    tcb.status = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)));
                } else if tcb.tid.0 == 10200 {
                    tcb.status = TaskStatus::Blocked(WaitReason::Poll);
                }
            }
        });
        let before_current = state.current_tid();''', '''        // FUTEX-FOUND1: waiters are published through the keyed table, in arrival order.
        for &t in &waiters {
            state.inject_futex_waiter_for_test(t, addr);
        }
        state.set_task_status_for_test(10200, TaskStatus::Blocked(WaitReason::Poll));
        let key = state.private_futex_key_for_test(waiters[0], addr);
        let before_current = state.current_tid();''')
once("let woke = shared.futex_wake_split_mut(CpuId(0), addr, 2);", "let woke = shared.futex_wake_key_split(CpuId(0), key, 2);")
once("let woke2 = shared.futex_wake_split_mut(CpuId(0), addr, u32::MAX);\n        assert_eq!(woke2, 2,",
     "let woke2 = shared.futex_wake_key_split(CpuId(0), key, u32::MAX);\n        assert_eq!(woke2, 2,")
once("let woke3 = shared.futex_wake_split_mut(CpuId(0), addr, u32::MAX);", "let woke3 = shared.futex_wake_key_split(CpuId(0), key, u32::MAX);")
# zero-max: inject, wake by key with 0.
once('''        state.inject_futex_waiter_for_test(10301, addr);
        let shared = SharedKernel::new(state);
        assert_eq!(shared.futex_wake_split_mut(CpuId(0), addr, 0), 0);''', '''        state.inject_futex_waiter_for_test(10301, addr);
        let key = state.private_futex_key_for_test(10301, addr);
        let shared = SharedKernel::new(state);
        assert_eq!(shared.futex_wake_key_split(CpuId(0), key, 0), 0);''')
# park-then-wake: the waker is the waiter's own address space (another thread of its process).
once('''        let woke = shared.futex_wake_split_mut(CpuId(0), addr, u32::MAX);
        assert_eq!(
            woke, 1,''', f'''        let woke = shared
            .futex_wake_split_mut(CpuId(0), asid, addr, u32::MAX, {PRIV})
            .expect("wake");
        assert_eq!(
            woke, 1,''')
once('''        let woke2 = shared.futex_wake_split_mut(CpuId(0), addr, u32::MAX);
        assert_eq!(woke2, 0, "no duplicate wake once the waiter is Runnable");''', f'''        let woke2 = shared
            .futex_wake_split_mut(CpuId(0), asid, addr, u32::MAX, {PRIV})
            .expect("wake");
        assert_eq!(woke2, 0, "no duplicate wake once the waiter is Runnable");''')
# waiter woken by split wake after dispatch: wake by key.
once('''        assert_eq!(shared.futex_wake_split_mut(CpuId(0), addr, u32::MAX), 1);
        shared.with(|k| assert_eq!(k.task_status(waiter), Some(TaskStatus::Runnable)));''', '''        let key = shared.with(|k| k.private_futex_key_for_test(waiter, addr));
        assert_eq!(shared.futex_wake_key_split(CpuId(0), key, u32::MAX), 1);
        shared.with(|k| assert_eq!(k.task_status(waiter), Some(TaskStatus::Runnable)));''')
# The repeated-cycle test.
once('''        state
            .with_tcbs_mut(|tcbs| {
                let tcb = tcbs.iter_mut().flatten().find(|t| t.tid.0 == tid).unwrap();
                tcb.status = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(futex_addr as u64)));
                Ok::<_, KernelError>(())
            })
            .expect("block");''', '''        state.inject_futex_waiter_for_test(tid, futex_addr);''')
# The robust test, re-derived: the waiter is a THREAD of the exiting task's process.
once('''    // Spawn the waiter task (TID 220) in asid_waiter.
    state
        .reserve_and_spawn_user_task_from_image_for_test(UserImageSpec {
            tid: 220,
            entry: 0x4000,
            asid: Some(asid_waiter),''', '''    // FUTEX-FOUND1 re-derivation: the waiter (TID 220) is a sibling THREAD of the victim, in the
    // victim's address space. A robust cleanup wakes the exiting task's OWN namespace; a waiter in
    // another address space at the same VA must not be woken (`futex_found1::f14`).
    let _ = asid_waiter;
    state
        .reserve_and_spawn_user_task_from_image_for_test(UserImageSpec {
            tid: 220,
            entry: 0x4000,
            asid: Some(asid_victim),''')
once('''    state
        .with_tcbs_mut(|tcbs| {
            let tcb = tcbs.iter_mut().flatten().find(|t| t.tid.0 == 220).unwrap();
            tcb.status = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(futex_addr as u64)));
            Ok::<_, KernelError>(())
        })
        .expect("block waiter");''', '''    state.inject_futex_waiter_for_test(220, futex_addr);''')
# The caller fast path no longer exists: `observed` is gone, and the kernel compares only the word.
once('''        // And the caller's own fast path is unchanged: an `expected` that differs from what it
        // observed still returns at once, whatever the word holds.
        assert_eq!(
            kernel.futex_wait_decide_split_read(tid, addr, 10, crate::kernel::boot::futex::FutexKind::Private),
            Ok(FutexWaitDecision::Proceed)
        );
''', '''        // FUTEX-FOUND1: there is no caller fast path any more — no `observed` argument — so the
        // only comparison is the kernel's own read of the word.
''')
# The robust waker takes the namespace owner's TID (in these tests, the waiter itself).
s = re.sub(r"futex_wake_on_exit\((addr|unmapped)\)", r"futex_wake_on_exit(tid, \1)", s)

# The wake-during-publication test: now the REAL interleaving, through the phase hook.
start = s.index("    fn u9fw_a_wake_during_publication_never_loses_the_caller() {")
end = s.index("\n    }\n", start) + len("\n    }\n")
s = s[:start] + '''    fn u9fw_a_wake_during_publication_never_loses_the_caller() {
        use crate::kernel::syscall::sched::FutexParkOutcome;
        let kernel: &'static SharedKernel = alloc::boxed::Box::leak(alloc::boxed::Box::new(
            SharedKernel::new(Bootstrap::init().expect("init")),
        ));
        let cpu = CpuId(0);
        let tid = 30003u64;
        let asid = futex_caller(kernel, tid, cpu);
        let addr = 0x5100usize;
        set_futex_word(kernel, tid, addr, PARK_WORD);

        // FUTEX-FOUND1: THE WAKER lands between the registration and the compare-and-clear, through
        // the production owner, exactly where a concurrent CPU lands (the phase hook).
        let k: &'static SharedKernel = kernel;
        crate::kernel::boot::futex::futex_phase_hook::arm_after_register(alloc::boxed::Box::new(
            move || {
                let woke = k.with(|s| s.futex_wake_on_exit(tid, addr));
                assert_eq!(woke, Ok(1), "the waker finds the registration");
            },
        ));
        let outcome = kernel.futex_wait_park_exact_split(
            cpu,
            tid,
            asid,
            addr,
            PARK_WORD,
            crate::kernel::boot::futex::FutexKind::Private,
        );
        let FutexParkOutcome::WokenDuringPublication { recovered, .. } = outcome else {
            panic!("a wake in the window is detected, never parked through: {outcome:?}");
        };
        assert!(recovered.may_resume_entering_frame(), "{recovered:?}");
        assert_eq!(
            kernel.with(|s| s.task_status(tid)),
            Some(TaskStatus::Running),
            "the caller is not lost: it runs again"
        );
        assert_eq!(kernel.with(|s| s.current_tid_on_cpu(cpu)), Some(tid));
        assert_eq!(
            kernel.with(|s| s.futex_waiter_count(addr)),
            0,
            "and no record is left behind"
        );
        // NEVER lost later either: a park after that, then a wake, reaches it.
        assert!(matches!(
            kernel.futex_wait_park_exact_split(
                cpu,
                tid,
                asid,
                addr,
                PARK_WORD,
                crate::kernel::boot::futex::FutexKind::Private
            ),
            FutexParkOutcome::Parked { .. }
        ));
        assert_eq!(kernel.with(|s| s.futex_wake_on_exit(tid, addr)), Ok(1));
        assert_eq!(
            kernel.with(|s| s.task_status(tid)),
            Some(TaskStatus::Runnable)
        );
    }
''' + s[end:]
T.write_text(s)

o = O.read_text()
a = '''    assert_eq!(
        fx.k.futex_wake_split_mut(CPU1, FUTEX_ADDR, 1),
        1,
        "one waiter woken"
    );'''
assert o.count(a) == 1
o = o.replace(a, f'''    // FUTEX-FOUND1: the waker is a thread of W's process, so it names W's private key.
    assert_eq!(
        fx.k.futex_wake_split_mut(CPU1, fx.w_asid, FUTEX_ADDR, 1, {PRIV}),
        Ok(1),
        "one waiter woken"
    );''')
O.write_text(o)
print("testfix2 applied")
