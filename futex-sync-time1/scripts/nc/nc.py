#!/usr/bin/env python3
"""FUTEX-SYNC-TIME1 negative controls: apply one mutation to crates/yarm-user-rt/src/lib.rs
(cwd = the worktree). Each replacement must occur exactly the stated number of times; the caller
restores the file with git checkout."""
import sys
P = "crates/yarm-user-rt/src/lib.rs"
NC = {
    # NC1 — the deadline is reset (pushed 1 s further) on every retry after a non-timeout return,
    # in the timed lock and in the predicate loop.
    "nc1_reset_deadline_on_retry": [
        ("""        pub fn lock_until_with<O: FutexOps + ?Sized>(
            &self,
            ops: &O,
            deadline_ns: u64,
        ) -> LockOutcome {""", """        pub fn lock_until_with<O: FutexOps + ?Sized>(
            &self,
            ops: &O,
            mut deadline_ns: u64,
        ) -> LockOutcome {""", 1),
        ("""                    Ok(_) => waited = true,""", """                    Ok(_) => {
                        waited = true;
                        deadline_ns = deadline_ns.saturating_add(1_000_000_000);
                    }""", 1),
        ("""            mutex: &FutexMutex,
            deadline_ns: u64,
            mut keep_waiting: F,
        ) -> WaitWhileOutcome {""", """            mutex: &FutexMutex,
            mut deadline_ns: u64,
            mut keep_waiting: F,
        ) -> WaitWhileOutcome {""", 1),
        ("""                    WaitOutcome::Notified => {
                        if !keep_waiting() {
                            return WaitWhileOutcome::Satisfied;
                        }""", """                    WaitOutcome::Notified => {
                        if !keep_waiting() {
                            return WaitWhileOutcome::Satisfied;
                        }
                        deadline_ns = deadline_ns.saturating_add(1_000_000_000);""", 1),
    ],
    # NC2 — a condition-variable timeout returns WITHOUT reacquiring the mutex.
    "nc2_timeout_without_reacquire": [
        ("""            let result = ops.wait(&self.seq, seen, self.scope, Some(deadline_ns));""",
         """            let result = ops.wait(&self.seq, seen, self.scope, Some(deadline_ns));
            if matches!(result, Ok(FutexTimedWait::TimedOut)) {
                return WaitOutcome::TimedOut;
            }""", 1),
    ],
    # NC3 — CONTENDED / wake responsibility dropped: every slow-path acquisition (the untimed
    # contended lock, the timed lock, the timed lock's final swap) leaves the word LOCKED instead
    # of CONTENDED, so the unlock of an owner that came through a wait wakes nobody and the
    # waiters behind it are stranded. The waiting loop itself is unchanged (no busy polling).
    "nc3_drop_contended": [
        ("""            while self.word.swap(CONTENDED, Ordering::Acquire) != UNLOCKED {
                let _ = ops.wait(&self.word, CONTENDED, self.scope, None);
            }
""", """            while self.word.swap(CONTENDED, Ordering::Acquire) != UNLOCKED {
                let _ = ops.wait(&self.word, CONTENDED, self.scope, None);
            }
            self.word.store(LOCKED, Ordering::Relaxed);
""", 1),
        ("""                if self.word.swap(CONTENDED, Ordering::Acquire) == UNLOCKED {
                    return LockOutcome::Acquired;""", """                if self.word.swap(CONTENDED, Ordering::Acquire) == UNLOCKED {
                    self.word.store(LOCKED, Ordering::Relaxed);
                    return LockOutcome::Acquired;""", 1),
        ("""                        return if self.word.swap(CONTENDED, Ordering::Acquire) == UNLOCKED {
                            LockOutcome::Acquired""", """                        return if self.word.swap(CONTENDED, Ordering::Acquire) == UNLOCKED {
                            self.word.store(LOCKED, Ordering::Relaxed);
                            LockOutcome::Acquired""", 1),
    ],
    # NC4 — the predicate recheck is suppressed: a notification is taken as satisfaction.
    "nc4_no_predicate_recheck": [
        ("""                    WaitOutcome::Notified => {
                        if !keep_waiting() {
                            return WaitWhileOutcome::Satisfied;
                        }""", """                    WaitOutcome::Notified => {
                        if true {
                            return WaitWhileOutcome::Satisfied;
                        }""", 1),
    ],
}
name = sys.argv[1]
s = open(P).read()
for a, b, n in NC[name]:
    assert s.count(a) == n, (name, a[:60], s.count(a))
    s = s.replace(a, b)
open(P, "w").write(s)
print(f"applied {name}")
