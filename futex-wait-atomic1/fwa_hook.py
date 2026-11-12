#!/usr/bin/env python3
"""FUTEX-WAIT-ATOMIC1 test harness: a one-shot cfg(test) hook between the wait's comparison and
its registration. Usage: fwa_hook.py <worktree> <candidate tests.rs>. Test-only; no production behaviour changes."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1])
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:80], s.count(old))
    p.write_text(s.replace(old, new))

patch("src/kernel/boot/futex.rs", '''    pub(crate) fn run_after_register() {
        let hook = AFTER_REGISTER.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }
''', '''    pub(crate) fn run_after_register() {
        let hook = AFTER_REGISTER.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }

    /// FUTEX-WAIT-ATOMIC1 — the wait's own boundary, for tests: runs once the wait has decided
    /// to park and before its registration is attempted. One-shot.
    std::thread_local! {
        static BEFORE_REGISTER: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }
    pub(crate) fn arm_before_register(hook: Hook) {
        BEFORE_REGISTER.with(|h| *h.borrow_mut() = Some(hook));
    }
    pub(crate) fn run_before_register() {
        let hook = BEFORE_REGISTER.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }
    std::thread_local! {
        static AFTER_CLEAR: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }
    pub(crate) fn arm_after_clear(hook: Hook) {
        AFTER_CLEAR.with(|h| *h.borrow_mut() = Some(hook));
    }
    pub(crate) fn run_after_clear() {
        let hook = AFTER_CLEAR.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }
''')
patch("src/runtime.rs", '''        if !crate::kernel::syscall::sched::futex_should_park(expected, word) {
            return O::ValueChanged;
        }

        // (1) rank 2 — REGISTER, exactly''', '''        if !crate::kernel::syscall::sched::futex_should_park(expected, word) {
            return O::ValueChanged;
        }
        #[cfg(test)]
        fx::futex_phase_hook::run_before_register();

        // (1) rank 2 — REGISTER, exactly''')
patch("src/runtime.rs", '''        // (3) rank 2 — READ BACK. A waker that entered the window between (1) and (2) has already''', '''        #[cfg(test)]
        fx::futex_phase_hook::run_after_clear();

        // (3) rank 2 — READ BACK. A waker that entered the window between (1) and (2) has already''')
# the permanent suite, verbatim from the candidate
cand = pathlib.Path(sys.argv[2]).read_text()
start = cand.index("    /// FUTEX-WAIT-ATOMIC1 — is `FutexWait` one operation?")
end = cand.index("    mod atomic1 {\n        use super::*;\n")
suite = cand[start:end]
p = wt / "src/kernel/boot/tests.rs"; s = p.read_text()
anchor = "    mod atomic1 {\n        use super::*;\n"
assert s.count(anchor) == 1
p.write_text(s.replace(anchor, suite + anchor))
print("hooks and suite applied")
