#!/usr/bin/env python3
"""FUTEX-FOUND1 production patch, part C: lifecycle cleanup, witnesses, and the split routes.
Usage: ff1_patch_c.py <worktree>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1])
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:80], s.count(old))
    p.write_text(s.replace(old, new))

RS = "src/kernel/boot/restart_state.rs"
# Robust cleanup in the EXITING task's namespace.
patch(RS, '''            // Use futex_wake_on_exit: the addresses come from the task's own
            // robust list and are trusted user-space, but current_tid() may be
            // a different task (e.g. supervisor) when exit is externally driven.
            let stride = core::mem::size_of::<usize>();
            let mut offset = 0usize;
            while offset < robust.len {
                let addr = robust.head.saturating_add(offset.saturating_mul(stride));
                let _ = self.futex_wake_on_exit(addr);
                offset += 1;
            }''', '''            // FUTEX-FOUND1: each robust word is woken in the EXITING task's own namespace — its
            // address space's private key (and the shared key when an object backs the word) —
            // never the namespace of whichever task drives the exit.
            let stride = core::mem::size_of::<usize>();
            let mut offset = 0usize;
            while offset < robust.len {
                let addr = robust.head.saturating_add(offset.saturating_mul(stride));
                let _ = self.futex_wake_on_exit(tid, addr);
                offset += 1;
            }''')
# Eager retirement of the exiting / dying task's own futex record.
patch(RS, '''            crate::kernel::boot::exit_claim::apply_self_exit_writes_locked(
                tcb,
                code,
                RestartToken(token),
            );
            Ok::<_, KernelError>(())
        })?;''', '''            crate::kernel::boot::exit_claim::apply_self_exit_writes_locked(
                tcb,
                code,
                RestartToken(token),
            );
            Ok::<_, KernelError>(())
        })?;
        // FUTEX-FOUND1: the status write just ended any futex wait this task was in; retire its
        // record now rather than leaving it to be reclaimed as stale.
        self.with_task_futex_mut(|_tcbs, table| {
            super::futex::retire_futex_waiters_for_task_locked(table, tid)
        });''')
patch(RS, '''            tcb.status = TaskStatus::Dead;
            tcb.restart.token = None;
            Ok::<_, KernelError>(())
        })?;
        // Stage 199A2B1: authoritative-identity reply-record cleanup at task death.''', '''            tcb.status = TaskStatus::Dead;
            tcb.restart.token = None;
            Ok::<_, KernelError>(())
        })?;
        // FUTEX-FOUND1: a dead task waits on nothing; retire any futex record naming it.
        self.with_task_futex_mut(|_tcbs, table| {
            super::futex::retire_futex_waiters_for_task_locked(table, tid)
        });
        // Stage 199A2B1: authoritative-identity reply-record cleanup at task death.''')

# Test counter.
patch("src/kernel/boot/ipc_state.rs", '''    /// Count tasks blocked with WaitReason::Futex on `addr` for test assertions.
    #[cfg(test)]
    pub(crate) fn futex_waiter_count(&self, addr: usize) -> usize {
        use crate::kernel::task::{TaskStatus, WaitReason};
        use crate::kernel::vm::VirtAddr;
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .filter(|tcb| {
                    tcb.status == TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)))
                })
                .count()
        })
    }''', '''    /// Count LIVE futex waiters that named `addr` (any key), for test assertions.
    #[cfg(test)]
    pub(crate) fn futex_waiter_count(&mut self, addr: usize) -> usize {
        self.with_task_futex_mut(|tcbs, table| {
            super::futex::live_futex_waiters_at_va_locked(tcbs.as_slice(), table, addr as u64)
        })
    }''')

# The overtaken witness's hold: where the task is parked, from its record.
patch("src/kernel/overtaken_witness.rs", '''fn futex_addr_of(shared: &crate::runtime::SharedKernel, tid: u64) -> Option<u64> {
    shared.with_task_tcbs_split_mut(|tcbs| {
        tcbs.iter()
            .flatten()
            .find(|t| t.tid.0 == tid)
            .and_then(|t| match t.status {
                TaskStatus::Blocked(WaitReason::Futex(VirtAddr(a))) => Some(a),
                _ => None,
            })
    })
}''', '''fn futex_addr_of(shared: &crate::runtime::SharedKernel, tid: u64) -> Option<u64> {
    shared.with_task_futex_split_mut(|tcbs, table| {
        tcbs.iter()
            .flatten()
            .find(|t| t.tid.0 == tid)
            .and_then(|t| match t.status {
                TaskStatus::Blocked(WaitReason::Futex(token)) => table.va_of(token),
                _ => None,
            })
    })
}''')

# ── split NR 9 ──────────────────────────────────────────────────────────────────────────────
SS = "src/kernel/syscall_split.rs"
patch(SS, '''    // (2) ABI, identical to `handle_futex_wait` — and ANSWERED, because `InvalidArgs` is what that
    // handler's `u32::try_from` produces and no later owner will produce it for us.
    let addr = frame.arg(SYSCALL_ARG_CAP);
    let (Ok(expected), Ok(observed)) = (
        u32::try_from(frame.arg(SYSCALL_ARG_PTR)),
        u32::try_from(frame.arg(SYSCALL_ARG_LEN)),
    ) else {
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_DONE tid=0 addr={} result=invalid_args cpu={}",
            addr,
            cpu.0
        );
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::InvalidArgs)));
    };''', '''    // (2) ABI — THE decode `handle_futex_wait` uses (FUTEX-FOUND1), and ANSWERED, because
    // `InvalidArgs` is what it produces and no later owner will produce it for us.
    let addr = frame.arg(SYSCALL_ARG_CAP);
    let args = match crate::kernel::syscall::sched::decode_futex_args(frame) {
        Ok(args) => args,
        Err(err) => {
            crate::yarm_log!(
                "FUTEX_WAIT_SPLIT_DONE tid=0 addr={} result=invalid_args cpu={}",
                addr,
                cpu.0
            );
            return D::Complete(Err(TrapHandleError::Syscall(err)));
        }
    };
    let expected = args.value;''')
patch(SS, '''    let decision = match shared.futex_wait_decide_split_read(tid, addr, expected, observed) {''',
      '''    let decision = match shared.futex_wait_decide_split_read(tid, addr, expected, args.kind) {''')
patch(SS, '''    match shared.futex_wait_park_exact_split(cpu, tid, asid, addr, expected) {''',
      '''    match shared.futex_wait_park_exact_split(cpu, tid, asid, addr, expected, args.kind) {''')
patch(SS, '''        FutexParkOutcome::IncarnationMoved => {
            crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);''', '''        // FUTEX-FOUND1 — pre-mutation by construction: the registration refused before writing.
        FutexParkOutcome::CapacityExhausted => {
            crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
            crate::yarm_log!(
                "FUTEX_WAIT_SPLIT_DONE tid={} addr={} result=capacity_exhausted cpu={}",
                tid,
                addr,
                cpu.0
            );
            D::Complete(Err(TrapHandleError::Syscall(SyscallError::from(
                crate::kernel::boot::KernelError::TaskTableFull,
            ))))
        }
        FutexParkOutcome::IncarnationMoved => {
            crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);''')

# ── split NR 10 ─────────────────────────────────────────────────────────────────────────────
patch(SS, '''    let Ok(max_wake) = u32::try_from(frame.arg(SYSCALL_ARG_PTR) as u64) else {
        crate::yarm_log!(
            "FUTEX_WAKE_SPLIT_REFUSED cpu={} addr=0x{:x} reason=max_wake_not_u32 \\
             settlement=invalid_args broad_lock=0",
            cpu.0,
            addr
        );
        return Some(Err(TrapHandleError::Syscall(SyscallError::InvalidArgs)));
    };''', '''    // FUTEX-FOUND1: THE decode `handle_futex_wake` uses (value, flags, reserved slot, alignment).
    let args = match crate::kernel::syscall::sched::decode_futex_args(frame) {
        Ok(args) => args,
        Err(err) => {
            crate::yarm_log!(
                "FUTEX_WAKE_SPLIT_REFUSED cpu={} addr=0x{:x} reason=invalid_args \\
                 settlement=invalid_args broad_lock=0",
                cpu.0,
                addr
            );
            return Some(Err(TrapHandleError::Syscall(err)));
        }
    };''')
patch(SS, '''    if shared
        .copy_from_user_asid_split_read(asid, addr, core::mem::size_of::<u32>())
        .is_none()
    {
        crate::yarm_log!(
            "FUTEX_WAKE_SPLIT_REFUSED cpu={} tid={} addr=0x{:x} reason=user_copy \\
             settlement=user_memory_fault broad_lock=0",
            cpu.0,
            tid,
            addr
        );
        return Some(Err(TrapHandleError::Syscall(SyscallError::from(
            crate::kernel::boot::KernelError::UserMemoryFault,
        ))));
    }
''', '''    // The waker's KEY, in the waker's address space — the same resolution the broad owner makes.
    let key = match shared.resolve_futex_word_in_split(
        crate::kernel::vm::Asid(asid as u16),
        addr,
        args.kind,
    ) {
        Ok((key, _word)) => key,
        Err(err) => {
            crate::yarm_log!(
                "FUTEX_WAKE_SPLIT_REFUSED cpu={} tid={} addr=0x{:x} reason=key err={:?} \\
                 broad_lock=0",
                cpu.0,
                tid,
                addr,
                err
            );
            return Some(Err(TrapHandleError::Syscall(SyscallError::from(err))));
        }
    };
''')
patch(SS, '''    let woke = shared.futex_wake_split_mut(cpu, addr, max_wake);''',
      '''    let woke = shared.futex_wake_key_split(cpu, key, args.value);''')
patch("src/kernel/boot/exec_state.rs",
      "use crate::kernel::task::{TaskStatus, ThreadGroupId, UserRegisterContext, WaitReason};",
      "use crate::kernel::task::{TaskStatus, ThreadGroupId, UserRegisterContext};")
print("patch C applied")
