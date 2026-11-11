#!/usr/bin/env python3
"""FUTEX-FOUND1 production patch, part A: plumbing (module, storage, accessors, AS generation,
WaitReason). Usage: ff1_patch_a.py <worktree>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1])
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:80], s.count(old))
    p.write_text(s.replace(old, new))

# ── module + storage ────────────────────────────────────────────────────────────────────────
patch("src/kernel/boot/mod.rs", "pub(crate) mod sender_waiter;\n",
      "pub(crate) mod sender_waiter;\n/// FUTEX-FOUND1: futex keys, the keyed waiter table and its rank-local owners.\npub(crate) mod futex;\n")
patch("src/kernel/boot/mod.rs", "    robust_futex: KernelStorage<[Option<RobustFutexRecord>; MAX_TASKS]>,\n    memory: KernelStorage<MemorySubsystem>,",
      "    robust_futex: KernelStorage<[Option<RobustFutexRecord>; MAX_TASKS]>,\n"
      "    /// FUTEX-FOUND1: the bounded keyed futex waiter table. Task domain (rank 2): every record\n"
      "    /// is written in the same critical section as its TCB's `Blocked(Futex(token))`.\n"
      "    futex_waiters: KernelStorage<futex::FutexWaiterTable>,\n"
      "    memory: KernelStorage<MemorySubsystem>,")
patch("src/kernel/boot/bootstrap_state.rs", """            core::ptr::addr_of_mut!((*state_ptr).robust_futex)
                .write(store_kernel_value([None; MAX_TASKS]));
""", """            core::ptr::addr_of_mut!((*state_ptr).robust_futex)
                .write(store_kernel_value([None; MAX_TASKS]));
            core::ptr::addr_of_mut!((*state_ptr).futex_waiters)
                .write(store_kernel_value(super::futex::FutexWaiterTable::new()));
""")

# ── accessors: broad + raw projector ────────────────────────────────────────────────────────
patch("src/kernel/boot/orchestrator_state.rs", """    pub(crate) fn with_tcbs_mut<R>(
        &mut self,
        f: impl FnOnce(&mut [Option<ThreadControlBlock>; MAX_TASKS]) -> R,
    ) -> R {
        // Lock-order domain: task
        Self::debug_lock_order_note("task");
        let _task_guard = self.task_state_lock.lock();
        f(kernel_mut(&mut self.tcbs))
    }
""", """    pub(crate) fn with_tcbs_mut<R>(
        &mut self,
        f: impl FnOnce(&mut [Option<ThreadControlBlock>; MAX_TASKS]) -> R,
    ) -> R {
        // Lock-order domain: task
        Self::debug_lock_order_note("task");
        let _task_guard = self.task_state_lock.lock();
        f(kernel_mut(&mut self.tcbs))
    }

    /// FUTEX-FOUND1 — the TCB array and the futex waiter table under ONE task-lock acquisition
    /// (rank 2). The broad twin of `SharedKernel::with_task_futex_split_mut`.
    pub(crate) fn with_task_futex_mut<R>(
        &mut self,
        f: impl FnOnce(
            &mut [Option<ThreadControlBlock>; MAX_TASKS],
            &mut super::futex::FutexWaiterTable,
        ) -> R,
    ) -> R {
        // Lock-order domain: task
        Self::debug_lock_order_note("task");
        let _task_guard = self.task_state_lock.lock();
        let table = core::ptr::addr_of_mut!(self.futex_waiters);
        // SAFETY: `tcbs` and `futex_waiters` are disjoint fields serialized by the one guard.
        f(kernel_mut(&mut self.tcbs), kernel_mut(unsafe { &mut *table }))
    }

    /// FUTEX-FOUND1 — raw field pointers for the split twin: `(task_state_lock, tcbs, table)`.
    pub(crate) unsafe fn task_futex_split_ptrs_from_raw(
        state: *mut KernelState,
    ) -> (
        *const crate::kernel::lock::SpinLockIrq<()>,
        *mut KernelStorage<[Option<ThreadControlBlock>; MAX_TASKS]>,
        *mut KernelStorage<super::futex::FutexWaiterTable>,
    ) {
        // SAFETY: callers pass the raw pointer returned by `SharedKernel`'s private `state_ptr()`
        // (BLR1); `addr_of!`/`addr_of_mut!` derive raw field pointers without creating references
        // to the whole KernelState.
        unsafe {
            (
                core::ptr::addr_of!((*state).task_state_lock),
                core::ptr::addr_of_mut!((*state).tcbs),
                core::ptr::addr_of_mut!((*state).futex_waiters),
            )
        }
    }
""")
patch("src/runtime.rs", """    /// Stage 199D: task (rank 2) split-mut seam exposing the TCB array and the TLS-restore
    /// table together — see `KernelState::task_return_split_mut_ptrs_from_raw`.
""", """    /// FUTEX-FOUND1 — task (rank 2) split-mut seam: the TCB array and the futex waiter table
    /// under ONE acquisition of the task lock. Callers must not already hold a lock of rank ≥ 2.
    pub(crate) fn with_task_futex_split_mut<R>(
        &self,
        f: impl FnOnce(
            &mut [Option<crate::kernel::task::ThreadControlBlock>],
            &mut crate::kernel::boot::futex::FutexWaiterTable,
        ) -> R,
    ) -> R {
        // SAFETY: same pattern as `with_task_tcbs_split_mut` — the task lock serializes both
        // storages, which live in the same domain.
        let (task_lock, tcbs, table) =
            unsafe { KernelState::task_futex_split_ptrs_from_raw(self.state_ptr()) };
        let task_lock = unsafe { &*task_lock };
        let _guard = task_lock.lock();
        let tcbs = unsafe { &mut *tcbs };
        let table = unsafe { &mut *table };
        f(kernel_mut(tcbs).as_mut_slice(), kernel_mut(table))
    }

    /// Stage 199D: task (rank 2) split-mut seam exposing the TCB array and the TLS-restore
    /// table together — see `KernelState::task_return_split_mut_ptrs_from_raw`.
""")

# ── the address-space incarnation ───────────────────────────────────────────────────────────
V = "src/kernel/vm.rs"
patch(V, """#[derive(Debug)]
struct AsEntry {
    asid: Asid,
    aspace: AddressSpace,
}""", """#[derive(Debug)]
struct AsEntry {
    asid: Asid,
    /// FUTEX-FOUND1: this address space's INCARNATION. `asid` is a recycled `u16`; the generation
    /// is stamped once at creation from a counter that only advances, so no two address spaces —
    /// live, destroyed or replacing one another under a reused ASID — ever share it.
    generation: u64,
    aspace: AddressSpace,
}""")
patch(V, """pub struct AddressSpaceManager {
    next_asid: u16,
    entries: [Option<AsEntry>; MAX_ADDRESS_SPACES],
    retired: [Option<RetiredAsid>; MAX_ADDRESS_SPACES],
}

impl Default for AddressSpaceManager {
    fn default() -> Self {
        Self {
            next_asid: 1,
            entries: [const { None }; MAX_ADDRESS_SPACES],
            retired: [None; MAX_ADDRESS_SPACES],
        }
    }
}""", """pub struct AddressSpaceManager {
    next_asid: u16,
    /// FUTEX-FOUND1: the next address-space incarnation. Starts at 1 and only advances.
    next_generation: u64,
    entries: [Option<AsEntry>; MAX_ADDRESS_SPACES],
    retired: [Option<RetiredAsid>; MAX_ADDRESS_SPACES],
}

impl Default for AddressSpaceManager {
    fn default() -> Self {
        Self {
            next_asid: 1,
            next_generation: 1,
            entries: [const { None }; MAX_ADDRESS_SPACES],
            retired: [None; MAX_ADDRESS_SPACES],
        }
    }
}""")
patch(V, """        let asid = self.allocate_asid()?;
        arch_register_asid(asid)?;
        self.entries[slot_idx] = Some(AsEntry {
            asid,
            aspace: AddressSpace::new_user_with_asid(asid),
        });
        Ok(asid)
    }
""", """        let asid = self.allocate_asid()?;
        arch_register_asid(asid)?;
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.entries[slot_idx] = Some(AsEntry {
            asid,
            generation,
            aspace: AddressSpace::new_user_with_asid(asid),
        });
        Ok(asid)
    }

    /// FUTEX-FOUND1: the incarnation of the live address space `asid`, if it is live.
    pub fn generation(&self, asid: Asid) -> Option<u64> {
        self.entries
            .iter()
            .flatten()
            .find(|entry| entry.asid == asid)
            .map(|entry| entry.generation)
    }
""")

# ── WaitReason ──────────────────────────────────────────────────────────────────────────────
T = "src/kernel/task.rs"
patch(T, """    EndpointSend(CapId),
    Futex(VirtAddr),
    Join(ThreadId),""", """    EndpointSend(CapId),
    /// FUTEX-FOUND1: the exact wait cycle — the waiter table's record slot and its incarnation.
    /// The record holds the key; the TCB and the record agree iff the tokens are equal.
    Futex(crate::kernel::boot::futex::FutexWaitToken),
    Join(ThreadId),""")
patch(T, """        let _ = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(0x1000)));""",
      """        let _ = TaskStatus::Blocked(WaitReason::Futex(
            crate::kernel::boot::futex::FutexWaitToken {
                slot: 0,
                generation: 0,
            },
        ));""")
print("patch A applied")
