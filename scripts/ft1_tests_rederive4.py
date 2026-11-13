"""FUTEX-TIME1: the resume-boundary and NR 9 route guards (fourth part)."""
import pathlib, sys
p=pathlib.Path(sys.argv[1]) / "src/kernel/boot/tests.rs"; s=p.read_text()
def rep(old,new,count=1):
    global s
    assert s.count(old)==count,(old[:90],s.count(old)); s=s.replace(old,new)
rep('''        let encodes: alloc::vec::Vec<usize> = writer
            .match_indices("encode_blocked_completion_result(frame, done.result)")''','''        // FUTEX-TIME1 re-derivation: the send-class consumer now encodes `result_register()` (it
        // also carries a timed FutexWait's `1`), the receive consumer still `result`; both call
        // the one owner.
        let encodes: alloc::vec::Vec<usize> = writer
            .match_indices("encode_blocked_completion_result(frame, done.result")''')
rep('''                let last_take = before
                    .rfind("BlockedSyscallClass::IpcSend")
                    .or_else(|| before.rfind("take_send_completion_split"));''','''                // FUTEX-TIME1 re-derivation: `take_resume_completion` is the in-lock take, scoped to
                // `IpcSend` and the timed `FutexWait` class that resumes through the same boundary.
                let last_take = before
                    .rfind("BlockedSyscallClass::IpcSend")
                    .or_else(|| before.rfind("take_send_completion_split"))
                    .or_else(|| before.rfind("take_resume_completion("));''')
rep('''            tail.contains("frame.set_user_gpr(10, done.result as usize);")
                && tail.contains("frame.set_user_gpr(11, 0);"),
            "the a0/a1 mirror is what a post-switch resume reads"''','''            // FUTEX-TIME1 re-derivation: `a0` is the class's result register (`1` for a timed
            // FutexWait a wake won, the error code otherwise, `0` for a send's success).
            tail.contains("frame.set_user_gpr(10, done.result_register());")
                && tail.contains("frame.set_user_gpr(11, 0);"),
            "the a0/a1 mirror is what a post-switch resume reads"''')
rep('''            "src/kernel/boot/exec_state.rs futex_wait_dispatch_try_defer( futex_wait_current",''','''            "src/kernel/boot/exec_state.rs futex_wait_dispatch_try_defer( futex_wait_current_inner",''', 3)
rep('''        // registration's `CapacityExhausted` refusal is a sixth, decided before any write.
        assert_eq!(
            after.matches("futex_wait_dispatch_clear(cpu_idx)").count(),
            6,
            "the six non-parking park outcomes each release the reservation"''','''        // registration's `CapacityExhausted` refusal is a sixth, decided before any write.
        // FUTEX-TIME1: seven — `TimedOut` (the deadline reached when the word matched) is a
        // seventh, decided before any write.
        assert_eq!(
            after.matches("futex_wait_dispatch_clear(cpu_idx)").count(),
            7,
            "the seven non-parking park outcomes each release the reservation"''')
p.write_text(s); print("rederived4")
