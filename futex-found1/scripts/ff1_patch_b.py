#!/usr/bin/env python3
"""FUTEX-FOUND1 production patch, part B: the syscall decode and the keyed wait/wake bodies.
Usage: ff1_patch_b.py <worktree>"""
import pathlib, re, sys
wt = pathlib.Path(sys.argv[1])
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:80], s.count(old))
    p.write_text(s.replace(old, new))
def patch_between(rel, start, end, new):
    """Replace from `start` (inclusive) up to `end` (exclusive); each must occur once."""
    p = wt / rel; s = p.read_text()
    assert s.count(start) == 1, (rel, start[:80], s.count(start))
    i = s.index(start); j = s.index(end, i)
    p.write_text(s[:i] + new + s[j:])

# ════════════════════════════════════════════════════════════════════════════════════════════
# B1 — src/kernel/syscall/sched.rs: THE futex ABI decode, compare, outcomes and broad handlers.
# ════════════════════════════════════════════════════════════════════════════════════════════
SCHED = "src/kernel/syscall/sched.rs"
patch_between(SCHED, "/// U9-FUTEX-WAIT-FINAL §2 — THE futex-word range policy, in one place.",
              "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub(crate) enum FutexWaitDecision {", '''/// THE futex-word range policy — one owner, `boot::futex::futex_word_range_check`.
///
/// Both acquisition adapters (the broad owner and the split route) call this; only the
/// reachability of the address space differs between them, never the rule. A null word is
/// `WrongObject`; an address whose last byte overflows `usize` or lands at or above
/// `KERNEL_SPACE_BASE` is `UserMemoryFault`.
pub(crate) fn futex_word_range_check(addr: usize) -> Result<(), crate::kernel::boot::KernelError> {
    crate::kernel::boot::futex::futex_word_range_check(addr)
}

/// FUTEX-FOUND1 — the decoded futex syscall arguments (NR 9 and NR 10 share the layout).
///
/// | slot | NR 9 `FutexWait` | NR 10 `FutexWake` |
/// |---|---|---|
/// | 0 | word address (4-byte aligned) | word address (4-byte aligned) |
/// | 1 | `expected` (`u32`) | `count` (`u32`) |
/// | 2 | flags: `FUTEX_FLAG_SHARED` (bit 0); every other bit reserved, must be 0 | same |
///
/// The kernel compares `expected` with the word IT reads (there is no caller-supplied `observed`).
/// Later operations — an absolute deadline, a requeue target, a wake mask — are each selected by a
/// flag bit that is reserved today, and that bit is what gives meaning to slots 3 and up; so a
/// reserved bit set now is `InvalidArgs`, and slots 3.. are not consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FutexArgs {
    pub(crate) addr: usize,
    pub(crate) value: u32,
    pub(crate) kind: crate::kernel::boot::futex::FutexKind,
}

/// THE decode, shared by the broad handlers and the split routes: the value as `u32`, the flags,
/// then the word's alignment. Its errors are all `InvalidArgs` and are answered before the address
/// range or the caller is consulted.
pub(crate) fn decode_futex_args(frame: &TrapFrame) -> Result<FutexArgs, SyscallError> {
    let addr = frame.arg(SYSCALL_ARG_CAP);
    let value = u32::try_from(frame.arg(SYSCALL_ARG_PTR)).map_err(|_| SyscallError::InvalidArgs)?;
    let kind = crate::kernel::boot::futex::FutexKind::from_flags(frame.arg(SYSCALL_ARG_LEN))
        .ok_or(SyscallError::InvalidArgs)?;
    if !crate::kernel::boot::futex::futex_word_aligned(addr) {
        return Err(SyscallError::InvalidArgs);
    }
    Ok(FutexArgs { addr, value, kind })
}

/// THE check of compare-and-park: does the word the KERNEL read still hold `expected`?
///
/// The caller's own read is not consulted — it can be stale by the time the kernel parks. The
/// park transaction applies this to its decision read and again to the read it makes after the
/// waiter is registered, both from the same mapping the key was resolved from.
pub(crate) fn futex_should_park(expected: u32, word: u32) -> bool {
    word == expected
}

''')
patch(SCHED, '''    /// `expected` matches both the caller's `observed` and the word the kernel read: park.
    Park,''', '''    /// The word the kernel read equals `expected`: park.
    Park,''')
patch(SCHED, '''pub(crate) enum FutexParkOutcome {
    /// PARKED. The exact incarnation is `Blocked(Futex(addr))` and current on no CPU, and the''',
'''pub(crate) enum FutexParkOutcome {
    /// PARKED. The exact incarnation is `Blocked(Futex(token))` with its keyed record published,
    /// and current on no CPU, and the''')
patch(SCHED, '''    /// The TCB is not the entering incarnation, or is gone, or is not `Running`. **Nothing was
    /// written** — this is the canonical `TaskMissing` population.
    IncarnationMoved,
}''', '''    /// The TCB is not the entering incarnation, or is gone, or is not `Running`. **Nothing was
    /// written** — this is the canonical `TaskMissing` population.
    IncarnationMoved,
    /// FUTEX-FOUND1 — every waiter slot holds a live record. **Nothing was written** and the
    /// caller is still current. Unreachable in production (one slot per possible task).
    CapacityExhausted,
}''')
s = (wt / SCHED).read_text()
i = s.index("pub(super) fn handle_futex_wait(")
s = s[:i] + '''pub(super) fn handle_futex_wait(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    // U9-FUTEX-WAIT-FINAL §4 — the LIVE half of the closure claim, measured where arrivals are:
    // `dispatch`'s `Syscall::FutexWait` arm is this function's only caller, and it is reached only
    // when the split route declined.
    crate::yarm_log!(
        "FUTEX_WAIT_BROAD_ENTRY nr=9 tid={} result=broad_entry",
        kernel.current_tid().unwrap_or(u64::MAX)
    );
    let args = decode_futex_args(frame)?;
    let blocked = kernel
        .futex_wait_current(args.addr, args.value, args.kind)
        .map_err(SyscallError::from)?;
    frame.set_ok(usize::from(blocked), 0, 0);
    Ok(())
}

pub(super) fn handle_futex_wake(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let args = decode_futex_args(frame)?;
    let woke = kernel
        .futex_wake(args.addr, args.value, args.kind)
        .map_err(SyscallError::from)?;
    frame.set_ok(woke as usize, 0, 0);
    Ok(())
}
'''
(wt / SCHED).write_text(s)

# ════════════════════════════════════════════════════════════════════════════════════════════
# B2 — src/kernel/boot/exec_state.rs: the BROAD owners.
# ════════════════════════════════════════════════════════════════════════════════════════════
EX = "src/kernel/boot/exec_state.rs"
patch(EX, '''    pub fn futex_wait_current(
        &mut self,
        addr: usize,
        expected: u32,
        observed: u32,
    ) -> Result<bool, KernelError> {
        // BL3b: compare against the word THIS owner read, not only the caller's copy of it.
        // The broad acquisition is held from the read to the park, so no waker can interleave.
        let word = self.validate_current_user_futex_word(addr)?;
        if !crate::kernel::syscall::sched::futex_should_park(expected, observed, word) {
            return Ok(false);
        }
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)));
            Ok::<_, KernelError>(())
        })?;
        let _ = self.block_current_cpu();''', '''    pub fn futex_wait_current(
        &mut self,
        addr: usize,
        expected: u32,
        kind: super::futex::FutexKind,
    ) -> Result<bool, KernelError> {
        // FUTEX-FOUND1: the key and the word in one vm-then-memory acquisition, compared with
        // `expected`, then the keyed registration. The broad acquisition is held from the read to
        // the park, so no waker can interleave.
        let (tid, asid, key, word) = self.resolve_current_futex_word(addr, kind)?;
        if !crate::kernel::syscall::sched::futex_should_park(expected, word) {
            return Ok(false);
        }
        self.with_task_futex_mut(|tcbs, table| {
            super::futex::register_futex_waiter_locked(
                tcbs.as_mut_slice(),
                table,
                tid,
                asid,
                key,
                addr as u64,
            )
        })
        .map_err(|refusal| match refusal {
            super::futex::FutexRegisterRefusal::IncarnationMoved => KernelError::TaskMissing,
            super::futex::FutexRegisterRefusal::Full => KernelError::TaskTableFull,
        })?;
        let _ = self.block_current_cpu();''')
patch_between(EX, "    pub fn futex_wake(&mut self, addr: usize, max_wake: u32) -> Result<u32, KernelError> {",
              "    /// Stage 190B (CONTROLLED AP WORKLOAD)", '''    pub fn futex_wake(
        &mut self,
        addr: usize,
        count: u32,
        kind: super::futex::FutexKind,
    ) -> Result<u32, KernelError> {
        let (_tid, _asid, key, _word) = self.resolve_current_futex_word(addr, kind)?;
        Ok(self.futex_wake_key(key, count))
    }

    /// The robust-futex cleanup wake, in the EXITING task's own namespace.
    ///
    /// `exit_task` may be driven by another task, so the key is resolved in `exiting_tid`'s
    /// address space — never the current CPU's. The word is woken under its private key and, when
    /// an allocator-owned object backs it, its shared key too (a robust lock may live in either).
    /// Every waiter is woken. Returns how many.
    pub(crate) fn futex_wake_on_exit(
        &mut self,
        exiting_tid: u64,
        addr: usize,
    ) -> Result<u32, KernelError> {
        super::futex::futex_word_range_check(addr)?;
        if !super::futex::futex_word_aligned(addr) {
            return Err(KernelError::WrongObject);
        }
        let asid = self
            .task_asid(exiting_tid)
            .ok_or(KernelError::UserMemoryFault)?;
        let private = self.with_vm_then_memory_mut(|spaces, memory| {
            super::futex::resolve_futex_word_locked(
                spaces,
                memory,
                asid,
                addr,
                super::futex::FutexKind::Private,
            )
        });
        let shared = self.with_vm_then_memory_mut(|spaces, memory| {
            super::futex::resolve_futex_word_locked(
                spaces,
                memory,
                asid,
                addr,
                super::futex::FutexKind::Shared,
            )
        });
        let (private_key, _) = private?;
        let mut woke = self.futex_wake_key(private_key, u32::MAX);
        if let Ok((shared_key, _)) = shared {
            woke = woke.saturating_add(self.futex_wake_key(shared_key, u32::MAX));
        }
        Ok(woke)
    }

    /// THE broad wake of one key: claim up to `count` live waiters, oldest first, then give each
    /// exactly one enqueue. The claim makes each `Runnable` and retires its record in one rank-2
    /// critical section; the enqueue follows at rank 1.
    ///
    /// An enqueue can only be refused for a waiter that is still some CPU's `current` — a split
    /// park caught between its registration and its compare-and-clear. That waiter's own park
    /// transaction owns its placement (it reads back, finds itself `Runnable` and recovers), so the
    /// refusal is reported and not propagated: no later waiter is left `Runnable` and unqueued.
    fn futex_wake_key(&mut self, key: super::futex::FutexKey, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let mut claimed = [0u64; super::futex::MAX_FUTEX_WAITERS];
        let n = self.with_task_futex_mut(|tcbs, table| {
            super::futex::claim_futex_waiters_locked(
                tcbs.as_mut_slice(),
                table,
                key,
                count as usize,
                &mut claimed,
            )
        });
        for &tid in &claimed[..n] {
            if let Err(err) = self.enqueue_task(tid) {
                crate::yarm_log!(
                    "FUTEX_WAKE_ENQUEUE_DEFERRED tid={} err={:?} owner=park_transaction",
                    tid,
                    err
                );
            }
        }
        n as u32
    }

    /// FUTEX-FOUND1 — the BROAD key resolution for the current task: range, caller, address
    /// space, then the key and the word in one vm-then-memory acquisition.
    fn resolve_current_futex_word(
        &mut self,
        addr: usize,
        kind: super::futex::FutexKind,
    ) -> Result<(u64, crate::kernel::vm::Asid, super::futex::FutexKey, u32), KernelError> {
        super::futex::futex_word_range_check(addr)?;
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        let (key, word) = self.with_vm_then_memory_mut(|spaces, memory| {
            super::futex::resolve_futex_word_locked(spaces, memory, asid, addr, kind)
        })?;
        Ok((tid, asid, key, word))
    }

''')

# ════════════════════════════════════════════════════════════════════════════════════════════
# B3 — src/runtime.rs: the SPLIT owners.
# ════════════════════════════════════════════════════════════════════════════════════════════
RT = "src/runtime.rs"
patch_between(RT, "    pub fn futex_wait_decide_split_read(", "    /// U9-FUTEX-WAIT-FINAL §3 — THE exact `FutexWait` park transaction.", '''    pub fn futex_wait_decide_split_read(
        &self,
        tid: u64,
        addr: usize,
        expected: u32,
        kind: crate::kernel::boot::futex::FutexKind,
    ) -> Result<crate::kernel::syscall::sched::FutexWaitDecision, crate::kernel::boot::KernelError>
    {
        use crate::kernel::syscall::sched::FutexWaitDecision;
        let (_key, word) = self.resolve_futex_word_split(tid, addr, kind)?;
        Ok(
            if crate::kernel::syscall::sched::futex_should_park(expected, word) {
                FutexWaitDecision::Park
            } else {
                FutexWaitDecision::Proceed
            },
        )
    }

    /// FUTEX-FOUND1 — the SPLIT key resolution for `tid`: range, address space, then the key and
    /// the word in ONE vm-then-memory acquisition (ranks 5 → 6, no other lock held).
    #[cfg_attr(feature = "hosted-dev", allow(dead_code))]
    pub(crate) fn resolve_futex_word_split(
        &self,
        tid: u64,
        addr: usize,
        kind: crate::kernel::boot::futex::FutexKind,
    ) -> Result<(crate::kernel::boot::futex::FutexKey, u32), crate::kernel::boot::KernelError> {
        crate::kernel::boot::futex::futex_word_range_check(addr)?;
        // The canonical owner resolves the ASID through `task_asid(tid).ok_or(UserMemoryFault)`;
        // `task_asid_for_tid_split_read` answers the reserved `0` for "no address space".
        let asid = self.task_asid_for_tid_split_read(tid);
        if asid == 0 {
            return Err(crate::kernel::boot::KernelError::UserMemoryFault);
        }
        self.resolve_futex_word_in_split(crate::kernel::vm::Asid(asid as u16), addr, kind)
    }

    /// FUTEX-FOUND1 — the key and the word for `addr` in the address space `asid`.
    pub(crate) fn resolve_futex_word_in_split(
        &self,
        asid: crate::kernel::vm::Asid,
        addr: usize,
        kind: crate::kernel::boot::futex::FutexKind,
    ) -> Result<(crate::kernel::boot::futex::FutexKey, u32), crate::kernel::boot::KernelError> {
        self.with_vm_then_memory_split_mut(|spaces, memory| {
            crate::kernel::boot::futex::resolve_futex_word_locked(spaces, memory, asid, addr, kind)
        })
    }

''')
# The park transaction: signature, then the body from "(1) rank 2" to "(2) rank 1".
patch(RT, '''    pub(crate) fn futex_wait_park_exact_split(
        &self,
        cpu: CpuId,
        tid: u64,
        asid: crate::kernel::vm::Asid,
        addr: usize,
        expected: u32,
    ) -> crate::kernel::syscall::sched::FutexParkOutcome {
        use crate::kernel::syscall::sched::FutexParkOutcome as O;
        use crate::kernel::task::{TaskStatus, WaitReason};
        use crate::kernel::vm::VirtAddr;
        let blocked = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)));
''', '''    pub(crate) fn futex_wait_park_exact_split(
        &self,
        cpu: CpuId,
        tid: u64,
        asid: crate::kernel::vm::Asid,
        addr: usize,
        expected: u32,
        kind: crate::kernel::boot::futex::FutexKind,
    ) -> crate::kernel::syscall::sched::FutexParkOutcome {
        use crate::kernel::boot::futex::{self as fx, FutexRegisterRefusal};
        use crate::kernel::syscall::sched::FutexParkOutcome as O;

        // (0) vm 5 → memory 6 — THE KEY AND THE WORD, from one mapping. Nothing is written yet.
        let (key, word) = match self.resolve_futex_word_in_split(asid, addr, kind) {
            Ok(found) => found,
            Err(err) => return O::WordUnreadable(err),
        };
        if !crate::kernel::syscall::sched::futex_should_park(expected, word) {
            return O::ValueChanged;
        }
''')
patch_between(RT, '''        // (1) rank 2 — REGISTER, exactly. From `Running` only: a caller that is not this CPU's''',
              '''        // (2) rank 1 — COMPARE-AND-CLEAR.''', '''        // (1) rank 2 — REGISTER, exactly: the keyed record and `Blocked(Futex(token))` in ONE
        // task-lock critical section, from `Running` only and for the exact `{tid, asid}`. A
        // waker scans the same table under the same lock, so from here on it finds this waiter.
        let registered = self.with_task_futex_split_mut(|tcbs, table| {
            fx::register_futex_waiter_locked(tcbs, table, tid, asid, key, addr as u64)
        });
        let token = match registered {
            Ok(token) => token,
            Err(FutexRegisterRefusal::IncarnationMoved) => {
                crate::yarm_log!(
                    "FUTEX_WAIT_PARK_REFUSED tid={} asid={} addr={} reason=incarnation_moved",
                    tid,
                    asid.0,
                    addr
                );
                return O::IncarnationMoved;
            }
            Err(FutexRegisterRefusal::Full) => {
                crate::yarm_log!(
                    "FUTEX_WAIT_PARK_REFUSED tid={} asid={} addr={} reason=capacity_exhausted",
                    tid,
                    asid.0,
                    addr
                );
                return O::CapacityExhausted;
            }
        };
        // The ONE undo of that registration: the record and the status together, and only from
        // exactly this token — a TCB a waker already moved is the waker's.
        let undo_registration = || {
            self.with_task_futex_split_mut(|tcbs, table| {
                fx::undo_futex_registration_locked(tcbs, table, tid, token)
            })
        };
        #[cfg(test)]
        fx::futex_phase_hook::run_after_register();

        // (1b) RE-READ, NOW THAT THE WAITER IS VISIBLE — the key AND the word, again from one
        // acquisition. A waker stores the word before it takes the task lock to claim; the
        // registration above and that claim are serialized by the task lock. Either the claim
        // follows the registration and finds this waiter, or it precedes it — and then the
        // waker's store precedes the waker's release of the task lock, which precedes this
        // transaction's acquisition of it, which precedes this read: the value below is the new
        // one. A changed key (the mapping moved) is a changed word for the same reason.
        let reread = self.resolve_futex_word_in_split(asid, addr, kind);
        let unchanged = matches!(reread, Ok((k, w)) if k == key && w == expected);
        if !unchanged && undo_registration() {
            crate::yarm_log!(
                "FUTEX_WAIT_PARK_REFUSED tid={} asid={} addr={} reason={} expected={}",
                tid,
                asid.0,
                addr,
                if reread.is_err() {
                    "word_unreadable"
                } else {
                    "value_changed"
                },
                expected
            );
            return match reread {
                Err(err) => O::WordUnreadable(err),
                Ok(_) => O::ValueChanged,
            };
        }

''')
patch(RT, '''        let Some(priority) = removed else {
            // Undo the registration exactly: this transaction published a block it could not
            // complete, and leaving the TCB `Blocked` would strand a task the scheduler still
            // believes is elsewhere.''', '''        let Some(priority) = removed else {
            // Undo the registration exactly: this transaction published a block it could not
            // complete, and leaving the TCB `Blocked` would strand a task the scheduler still
            // believes is elsewhere. (A waker that claimed it meanwhile owns it; the undo then
            // changes nothing.)''')
patch(RT, '''        // (3) rank 2 — READ BACK. A waker that entered the window between (1) and (2) has already
        // moved this TCB off `Blocked(Futex(addr))`, and that is the ONLY thing that can have.
        let still_parked = self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|t| t.tid.0 == tid)
                .is_some_and(|t| t.status == blocked)
        });''', '''        // (3) rank 2 — READ BACK. A waker that entered the window between (1) and (2) has already
        // claimed this exact token — its record retired and the TCB `Runnable` — and that is the
        // ONLY thing that can have moved it.
        let still_parked = self.with_task_futex_split_mut(|tcbs, _table| {
            fx::futex_still_parked_locked(tcbs, tid, token)
        });''')
# The split wake.
patch_between(RT, "    pub fn futex_wake_split_mut(&self, cpu: CpuId, addr: usize, max_wake: u32) -> u32 {",
              "    // ── Stage 198E3B2A part 2: shared-region OFF-LOCK seams", '''    pub fn futex_wake_split_mut(
        &self,
        cpu: CpuId,
        asid: crate::kernel::vm::Asid,
        addr: usize,
        count: u32,
        kind: crate::kernel::boot::futex::FutexKind,
    ) -> Result<u32, crate::kernel::boot::KernelError> {
        // 1. The waker's key, in the WAKER's address space (vm 5 → memory 6).
        crate::kernel::boot::futex::futex_word_range_check(addr)?;
        let (key, _word) = self.resolve_futex_word_in_split(asid, addr, kind)?;
        Ok(self.futex_wake_key_split(cpu, key, count))
    }

    /// FUTEX-FOUND1 — THE split wake of one key.
    ///
    /// 1. rank 2 — the claim: up to `count` live waiters, oldest first; each is made `Runnable`
    ///    and its record retired in this one critical section, so none is claimed twice.
    /// 2. rank 1 — one enqueue per claimed waiter, through THE shared enqueue owner (its home
    ///    CPU, then the remote half of the commit). A refusal can only be for a waiter that is
    ///    still some CPU's `current` — a park caught between its registration and its
    ///    compare-and-clear, whose own read-back owns its placement — so it is reported, not
    ///    propagated, and no later waiter is skipped.
    ///
    /// Lock order: task (rank 2) then scheduler (rank 1), never nested.
    pub(crate) fn futex_wake_key_split(
        &self,
        cpu: CpuId,
        key: crate::kernel::boot::futex::FutexKey,
        count: u32,
    ) -> u32 {
        if count == 0 {
            return 0;
        }
        let mut claimed = [0u64; crate::kernel::boot::futex::MAX_FUTEX_WAITERS];
        let n = self.with_task_futex_split_mut(|tcbs, table| {
            crate::kernel::boot::futex::claim_futex_waiters_locked(
                tcbs,
                table,
                key,
                count as usize,
                &mut claimed,
            )
        });
        for &tid in &claimed[..n] {
            let Ok(mut plan) = self.with_task_enqueue_policy_split_mut(|tcbs, classes| {
                crate::kernel::task_enqueue::plan_balanced_enqueue_locked(tcbs, classes, tid, cpu)
            }) else {
                crate::yarm_log!(
                    "FUTEX_WAKE_ENQUEUE_DEFERRED tid={} reason=plan owner=park_transaction",
                    tid
                );
                continue;
            };
            // AP-SCHED1: the woken task's home, then the remote half of the commit.
            self.resolve_plan_home_split(&mut plan, cpu);
            match self.commit_enqueue_split(&plan) {
                Ok(target) => crate::arch::hal::notify_enqueued(target),
                Err(err) => crate::yarm_log!(
                    "FUTEX_WAKE_ENQUEUE_DEFERRED tid={} err={:?} owner=park_transaction",
                    tid,
                    err
                ),
            }
        }
        n as u32
    }

''')
print("patch B applied")
