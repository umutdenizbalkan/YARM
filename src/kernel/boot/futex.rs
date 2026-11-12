// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! FUTEX-FOUND1 — generation-safe futex keys, the bounded keyed waiter table, and the rank-local
//! owners of compare-and-park and wake.
//!
//! # The key
//!
//! A futex names a synchronization word, not a number. [`FutexKey`] is one of:
//!
//! * **`Private { asid, aspace_generation, va }`** — the exact address-space INCARNATION and the
//!   aligned user VA. `Asid` is a recycled `u16`, so the incarnation is the generation the
//!   `AddressSpaceManager` stamps on every creation and never reissues
//!   (`AddressSpaceManager::generation`). Two address spaces never share a private key, threads of
//!   one space always do, and a replacement space that reuses an ASID number shares nothing with
//!   the one it replaced.
//! * **`Shared { object_id, offset }`** — the allocator-owned `MemoryObject` whose extent contains
//!   the word's frame (`frame_owner_slot_locked`: live extents are disjoint, so at most one object
//!   contains a frame), its id (ids are never reused: `next_memory_object_id` only advances), and
//!   the byte offset of the word inside it. Two processes that map one object at different VAs
//!   resolve to one key; different objects, or different offsets in one object, never alias; a
//!   later object in a reused registry slot has a new id. A word not backed by an allocator-owned
//!   object — a private frame, or a borrowed initramfs slice whose address cannot name ONE slice —
//!   is refused with `WrongObject`: a shared wait is never silently made private.
//!
//! The key and the word's value are read in ONE vm-then-memory acquisition (ranks 5 → 6), so the
//! value always belongs to the object the key names.
//!
//! # The waiter table (task domain, rank 2)
//!
//! [`FutexWaiterTable`] holds every futex waiter record. It lives beside `tcbs` under the TASK
//! lock, and every transition writes the record and the TCB status in the SAME critical section:
//!
//! * a record and `TaskStatus::Blocked(WaitReason::Futex(token))` are created together
//!   ([`register_futex_waiter_locked`]), and `FutexWait` creates them only inside the hold that
//!   resolved its key and compared its word ([`compare_and_register_futex_waiter_locked`]);
//! * a wake removes the record and makes the task `Runnable` together
//!   ([`claim_futex_waiter_locked`], after [`next_futex_waiter_locked`]);
//! * an undo removes the record and restores `Running` together
//!   ([`undo_futex_registration_locked`]).
//!
//! **The TCB is the authority** on whether a task waits. A record is LIVE only while its TCB is
//! `Blocked(Futex(token))` with the record's own token; a record whose TCB moved on (death, or any
//! status writer outside this module) is STALE: inert, never woken, never counted, and reclaimed by
//! the next claim or registration that meets it ([`retire_futex_waiters_for_task_locked`] also
//! reclaims a dying task's record eagerly). So a delayed operation of an earlier wait cycle can
//! never affect a later one: the later cycle has a different token.
//!
//! **Capacity.** `MAX_FUTEX_WAITERS == MAX_TASKS` slots. A task is in at most one wait cycle, so
//! live records never exceed live TCBs, and a registering task — `Running`, so any record naming it
//! is stale — always finds a slot once stale records are swept. Exhaustion is therefore
//! unreachable in production, and the refusal ([`FutexRegisterRefusal::Full`]) is still decided
//! before anything is written.
//!
//! **Order.** Records are kept in `FUTEX_BUCKETS` hash buckets, each a singly-linked FIFO in
//! arrival order. A wake walks only its key's bucket, front to back. Moving a record to another key
//! (the future requeue) is an unlink and an append, with the TCB untouched.

use super::{KernelError, MAX_TASKS, MemorySubsystem};
use crate::kernel::scheduler::{CpuId, MAX_CPUS, TaskPriority};
use crate::kernel::task::{TaskClass, TaskStatus, ThreadControlBlock, WaitReason};
use crate::kernel::vm::{AddressSpaceManager, Asid, PAGE_SIZE, VirtAddr};

/// NR 9 / NR 10 flag: resolve a SHARED key (backing object + offset) instead of a private one.
pub const FUTEX_FLAG_SHARED: usize = 1;
/// Every flag this ABI defines; any other bit set is `InvalidArgs`.
pub const FUTEX_FLAGS_DEFINED: usize = FUTEX_FLAG_SHARED;

/// Which namespace a futex word is resolved in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FutexKind {
    Private,
    Shared,
}

impl FutexKind {
    /// Decode the ABI flag word; `None` if any undefined bit is set.
    pub fn from_flags(flags: usize) -> Option<Self> {
        if flags & !FUTEX_FLAGS_DEFINED != 0 {
            return None;
        }
        Some(if flags & FUTEX_FLAG_SHARED != 0 {
            Self::Shared
        } else {
            Self::Private
        })
    }
}

/// The identity of one futex word. See the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FutexKey {
    Private {
        asid: Asid,
        aspace_generation: u64,
        va: u64,
    },
    Shared {
        object_id: u64,
        offset: u64,
    },
    /// FUTEX-REQUEUE-ATOMIC1 — the wake set of ONE compare-and-requeue, between the commit that
    /// selected it and the placement of each member. No key resolution produces it, so no
    /// FutexWait, FutexWake or compare-and-requeue of a user word can reach these waiters; only
    /// the operation that made the key, and task exit, touch them. `op` is unique per operation
    /// ([`next_requeue_op`]); `cpu` and `priority` are the placement whose reserved run-queue slot
    /// the commit counted for each waiter on this key.
    Pending {
        op: u64,
        cpu: u8,
        priority: u8,
    },
}

impl FutexKey {
    fn bucket(&self) -> usize {
        let h = match *self {
            Self::Private {
                asid,
                aspace_generation,
                va,
            } => (va >> 2) ^ aspace_generation.rotate_left(17) ^ u64::from(asid.0).rotate_left(41),
            Self::Shared { object_id, offset } => {
                (offset >> 2) ^ object_id.rotate_left(29) ^ 0x9E37_79B9_7F4A_7C15
            }
            Self::Pending { op, cpu, priority } => {
                op.rotate_left(7)
                    ^ (u64::from(cpu) << 2)
                    ^ u64::from(priority)
                    ^ 0x5851_F42D_4C95_7F2D
            }
        };
        (h.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize % FUTEX_BUCKETS
    }
}

/// THE futex-word address policy: non-null, 4-byte aligned, wholly below `KERNEL_SPACE_BASE`.
///
/// Null is `WrongObject` and a kernel-range or overflowing word is `UserMemoryFault`, the canonical
/// answers. Alignment is the syscall layer's (`InvalidArgs`), checked before this.
pub fn futex_word_range_check(addr: usize) -> Result<(), KernelError> {
    if addr == 0 {
        return Err(KernelError::WrongObject);
    }
    let end = addr.checked_add(core::mem::size_of::<u32>() - 1);
    if end.is_none_or(|end| end as u64 >= crate::kernel::vm::KERNEL_SPACE_BASE) {
        return Err(KernelError::UserMemoryFault);
    }
    Ok(())
}

/// Whether `addr` is a naturally aligned futex word address.
pub const fn futex_word_aligned(addr: usize) -> bool {
    addr.is_multiple_of(core::mem::size_of::<u32>())
}

/// THE key resolver and word read, under vm rank 5 then memory rank 6 (the caller holds both).
///
/// Resolves `addr` in the address space `asid` to its key of `kind`, and reads the word's current
/// value from the SAME mapping in the same acquisition. Errors are the canonical ones: a range
/// failure as [`futex_word_range_check`]; no such address space or no user-readable mapping is
/// `UserMemoryFault`; a shared request on a word that no allocator-owned object backs is
/// `WrongObject`.
pub fn resolve_futex_word_locked(
    spaces: &AddressSpaceManager,
    memory: &MemorySubsystem,
    asid: Asid,
    addr: usize,
    kind: FutexKind,
) -> Result<(FutexKey, u32), KernelError> {
    let (key, phys) = resolve_futex_key_locked(spaces, memory, asid, addr, kind)?;
    let word = read_word_locked(memory, asid, phys)?;
    Ok((key, word))
}

/// The key alone, and the frame the word lives in — for owners that need the identity but not the
/// value (the robust-futex cleanup). Same errors as [`resolve_futex_word_locked`] minus the read.
pub fn resolve_futex_key_locked(
    spaces: &AddressSpaceManager,
    memory: &MemorySubsystem,
    asid: Asid,
    addr: usize,
    kind: FutexKind,
) -> Result<(FutexKey, u64), KernelError> {
    futex_word_range_check(addr)?;
    let aspace = spaces.get(asid).ok_or(KernelError::UserMemoryFault)?;
    let aspace_generation = spaces
        .generation(asid)
        .ok_or(KernelError::UserMemoryFault)?;
    let page = addr & !(PAGE_SIZE - 1);
    let mapping = aspace
        .resolve(VirtAddr(page as u64))
        .ok_or(KernelError::UserMemoryFault)?;
    if !mapping.flags.user || !mapping.flags.read {
        return Err(KernelError::UserMemoryFault);
    }
    // Aligned and below the page end: the four bytes lie in this one page.
    let phys = mapping.phys.0 + (addr - page) as u64;
    let key = match kind {
        FutexKind::Private => FutexKey::Private {
            asid,
            aspace_generation,
            va: addr as u64,
        },
        FutexKind::Shared => {
            let slot = super::KernelState::frame_owner_slot_locked(
                memory,
                crate::kernel::vm::PhysAddr(phys),
            )
            .ok_or(KernelError::WrongObject)?;
            let object = memory.memory_objects[slot].ok_or(KernelError::WrongObject)?;
            FutexKey::Shared {
                object_id: object.id,
                offset: phys - object.phys.0,
            }
        }
    };
    Ok((key, phys))
}

/// The ONE word reader, given the frame. Freestanding: through the direct map. Hosted: the hosted
/// user-memory emulation, which is keyed by `(asid, phys)` — so two spaces sharing a frame do not
/// see each other's bytes there, and hosted tests of shared words write the word in each space.
#[cfg(not(feature = "hosted-dev"))]
fn read_word_locked(_memory: &MemorySubsystem, _asid: Asid, phys: u64) -> Result<u32, KernelError> {
    let ptr =
        super::KernelState::phys_to_direct_map_ptr(phys).ok_or(KernelError::UserMemoryFault)?;
    // FUTEX-REQUEUE-ATOMIC1: the word is a synchronization variable that other CPUs' user code
    // stores to with atomics while the kernel reads it, so the kernel's read is an ATOMIC load
    // (a volatile read is not one in Rust's memory model: racing it with a store is a data race).
    // Acquire, so the kernel's later decisions are ordered after the value it compared; the futex
    // protocol's ordering against registrations and wakes comes from the task lock.
    // Every syscall refuses a misaligned word before it gets here (`InvalidArgs`); the reader
    // refuses one too, because the atomic load below requires alignment.
    if ptr as usize % core::mem::align_of::<core::sync::atomic::AtomicU32>() != 0 {
        return Err(KernelError::UserMemoryFault);
    }
    // SAFETY: `phys` is inside a validated user-readable mapping held stable by the vm lock, the
    // direct-map pointer is bounds-checked, and it is aligned for `AtomicU32` (checked above).
    let word = unsafe { &*(ptr as *const core::sync::atomic::AtomicU32) };
    Ok(word.load(core::sync::atomic::Ordering::Acquire))
}

#[cfg(feature = "hosted-dev")]
fn read_word_locked(memory: &MemorySubsystem, asid: Asid, phys: u64) -> Result<u32, KernelError> {
    let mut bytes = [0u8; 4];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = memory
            .user_memory
            .get(&(asid.0, phys + i as u64))
            .copied()
            .ok_or(KernelError::UserMemoryFault)?;
    }
    Ok(u32::from_ne_bytes(bytes))
}

// ── The waiter table ─────────────────────────────────────────────────────────────────────────

/// One record slot per possible task: see "Capacity" in the module documentation.
pub const MAX_FUTEX_WAITERS: usize = MAX_TASKS;
/// Hash buckets. A bucket is a FIFO; its length is bounded by `MAX_FUTEX_WAITERS`.
pub const FUTEX_BUCKETS: usize = 64;
const NIL: u16 = u16::MAX;
const _: () = assert!(MAX_FUTEX_WAITERS < NIL as usize);

/// The exact identity of one futex wait cycle: which record slot, in which incarnation. Carried by
/// the waiting TCB as `Blocked(Futex(token))`; a record and a TCB agree iff the tokens are equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FutexWaitToken {
    pub slot: u16,
    pub generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FutexWaiterRecord {
    key: FutexKey,
    tid: u64,
    /// The waiter's TCB slot, so liveness is one comparison and not a TCB-array scan. A TCB never
    /// moves slots; a slot reused by another task fails the `tid` and token comparison.
    tcb: u16,
    /// The VA the waiter named (diagnostics and the overtaken witness's hold; never identity).
    va: u64,
    next: u16,
}

/// The bounded keyed waiter store. Task domain (rank 2): reached only with the TCB array.
#[derive(Debug)]
pub struct FutexWaiterTable {
    records: [Option<FutexWaiterRecord>; MAX_FUTEX_WAITERS],
    generations: [u32; MAX_FUTEX_WAITERS],
    head: [u16; FUTEX_BUCKETS],
    tail: [u16; FUTEX_BUCKETS],
    /// Test-only capacity limit, so exhaustion can be exercised without `MAX_TASKS` tasks.
    #[cfg(test)]
    limit: usize,
}

impl Default for FutexWaiterTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a registration was refused. Nothing is written for either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FutexRegisterRefusal {
    /// The TCB is not the entering `{tid, asid}` incarnation, or is not `Running`.
    IncarnationMoved,
    /// Every slot holds a LIVE record (unreachable in production; see "Capacity").
    Full,
}

impl FutexWaiterTable {
    pub const fn new() -> Self {
        Self {
            records: [None; MAX_FUTEX_WAITERS],
            generations: [0; MAX_FUTEX_WAITERS],
            head: [NIL; FUTEX_BUCKETS],
            tail: [NIL; FUTEX_BUCKETS],
            #[cfg(test)]
            limit: MAX_FUTEX_WAITERS,
        }
    }

    /// Test-only: cap the usable slots at `limit`.
    #[cfg(test)]
    pub fn set_capacity_for_test(&mut self, limit: usize) {
        self.limit = limit.min(MAX_FUTEX_WAITERS);
    }

    fn capacity(&self) -> usize {
        #[cfg(test)]
        {
            self.limit
        }
        #[cfg(not(test))]
        {
            MAX_FUTEX_WAITERS
        }
    }

    fn token(&self, slot: usize) -> FutexWaitToken {
        FutexWaitToken {
            slot: slot as u16,
            generation: self.generations[slot],
        }
    }

    /// Append `slot` (already holding its record) to its key's bucket.
    fn link_tail(&mut self, slot: usize) {
        let Some(rec) = self.records[slot].as_mut() else {
            return;
        };
        rec.next = NIL;
        let b = rec.key.bucket();
        match self.tail[b] {
            NIL => self.head[b] = slot as u16,
            t => {
                if let Some(prev) = self.records[t as usize].as_mut() {
                    prev.next = slot as u16;
                }
            }
        }
        self.tail[b] = slot as u16;
    }

    /// Unlink `slot` from its bucket, free it and advance its incarnation. Returns the record.
    fn retire(&mut self, slot: usize) -> Option<FutexWaiterRecord> {
        let rec = self.records[slot]?;
        let b = rec.key.bucket();
        let mut prev = NIL;
        let mut cur = self.head[b];
        while cur != NIL && cur as usize != slot {
            prev = cur;
            cur = self.records[cur as usize].map_or(NIL, |r| r.next);
        }
        self.retire_after(slot, prev)
    }

    /// Unlink `slot`, whose bucket predecessor is `prev` (`NIL` at the head), free it and advance
    /// its incarnation. O(1).
    fn retire_after(&mut self, slot: usize, prev: u16) -> Option<FutexWaiterRecord> {
        let rec = self.records[slot]?;
        self.unlink_after(slot, prev);
        self.records[slot] = None;
        self.generations[slot] = self.generations[slot].wrapping_add(1);
        Some(rec)
    }

    /// FUTEX-REQUEUE1 — take `slot` out of its bucket chain (predecessor `prev`, `NIL` at the
    /// head) WITHOUT freeing it: the record and its incarnation, and so its waiter's token, are
    /// unchanged. O(1). The caller relinks it ([`Self::link_tail`]) under the same task lock.
    fn unlink_after(&mut self, slot: usize, prev: u16) {
        let Some(rec) = self.records[slot] else {
            return;
        };
        let b = rec.key.bucket();
        if prev == NIL {
            if self.head[b] as usize == slot {
                self.head[b] = rec.next;
            }
        } else if let Some(p) = self.records[prev as usize].as_mut() {
            p.next = rec.next;
        }
        if self.tail[b] as usize == slot {
            self.tail[b] = prev;
        }
    }

    /// Whether `slot`'s record is LIVE: its TCB waits on exactly this token.
    fn live(&self, tcbs: &[Option<ThreadControlBlock>], slot: usize) -> bool {
        let Some(rec) = self.records[slot] else {
            return false;
        };
        let token = self.token(slot);
        tcbs.get(usize::from(rec.tcb))
            .and_then(Option::as_ref)
            .is_some_and(|t| {
                t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))
            })
    }

    /// The key a LIVE token's record names.
    #[cfg(test)]
    pub fn key_of(&self, token: FutexWaitToken) -> Option<FutexKey> {
        let slot = usize::from(token.slot);
        (slot < MAX_FUTEX_WAITERS && self.generations[slot] == token.generation)
            .then(|| self.records[slot].map(|r| r.key))
            .flatten()
    }

    /// The VA a token's record named (diagnostics and the overtaken witness).
    #[cfg(any(
        test,
        feature = "aarch64-overtaken-witness",
        feature = "x86-overtaken-witness"
    ))]
    pub fn va_of(&self, token: FutexWaitToken) -> Option<u64> {
        let slot = usize::from(token.slot);
        (slot < MAX_FUTEX_WAITERS && self.generations[slot] == token.generation)
            .then(|| self.records[slot].map(|r| r.va))
            .flatten()
    }

    /// How many records are held (live or stale). Test and census use.
    #[cfg(test)]
    pub fn occupied(&self) -> usize {
        self.records.iter().flatten().count()
    }

    /// FUTEX-REQUEUE1 — the LIVE waiters of `key`, in FIFO order (bucket-chain order), as TIDs.
    #[cfg(test)]
    pub fn waiters_in_order(
        &self,
        tcbs: &[Option<ThreadControlBlock>],
        key: FutexKey,
    ) -> alloc::vec::Vec<u64> {
        let mut out = alloc::vec::Vec::new();
        let mut cur = self.head[key.bucket()];
        while cur != NIL {
            let slot = usize::from(cur);
            let Some(rec) = self.records[slot] else {
                break;
            };
            if rec.key == key && self.live(tcbs, slot) {
                out.push(rec.tid);
            }
            cur = rec.next;
        }
        out
    }

    /// FUTEX-REQUEUE1 — the chains are exact: every held record is on exactly one chain, the one
    /// its key hashes to, each chain ends at its `tail`, and no free slot is linked.
    #[cfg(test)]
    pub fn assert_chains_consistent(&self) {
        let mut seen = [false; MAX_FUTEX_WAITERS];
        let mut linked = 0;
        for b in 0..FUTEX_BUCKETS {
            let (mut prev, mut cur) = (NIL, self.head[b]);
            while cur != NIL {
                let slot = usize::from(cur);
                assert!(!seen[slot], "slot {slot} linked twice");
                seen[slot] = true;
                let rec = self.records[slot].expect("a free slot is linked");
                assert_eq!(rec.key.bucket(), b, "slot {slot} on the wrong chain");
                linked += 1;
                prev = cur;
                cur = rec.next;
                assert!(linked <= MAX_FUTEX_WAITERS, "a cycle in bucket {b}");
            }
            assert_eq!(
                self.tail[b], prev,
                "bucket {b}: tail is not the last record"
            );
        }
        assert_eq!(linked, self.occupied(), "a held record is on no chain");
    }
}

impl FutexKey {
    /// The bucket a key hashes to. Test use (collisions, same-bucket moves).
    #[cfg(test)]
    pub fn bucket_for_test(&self) -> usize {
        self.bucket()
    }
}

/// rank 2 — REGISTER: the record and `Blocked(Futex(token))` in one critical section.
///
/// Only from `Running`, and only for the exact `{tid, asid}` incarnation (a kernel task's `None`
/// compares equal to the reserved `Asid(0)`, the placement owners' normalization). Writes nothing
/// when it refuses.
pub fn register_futex_waiter_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    table: &mut FutexWaiterTable,
    tid: u64,
    asid: Asid,
    key: FutexKey,
    va: u64,
) -> Result<FutexWaitToken, FutexRegisterRefusal> {
    let Some(tcb_idx) = tcbs.iter().position(|t| {
        t.as_ref().is_some_and(|t| {
            t.tid.0 == tid
                && t.asid.unwrap_or(Asid(0)) == asid
                && matches!(t.status, TaskStatus::Running)
        })
    }) else {
        return Err(FutexRegisterRefusal::IncarnationMoved);
    };
    let cap = table.capacity();
    let mut free = (0..cap).find(|&i| table.records[i].is_none());
    if free.is_none() {
        // Sweep stale records (their TCB moved on); see "Capacity".
        for i in 0..cap {
            if !table.live(tcbs, i) {
                table.retire(i);
            }
        }
        free = (0..cap).find(|&i| table.records[i].is_none());
    }
    let Some(slot) = free else {
        return Err(FutexRegisterRefusal::Full);
    };
    table.records[slot] = Some(FutexWaiterRecord {
        key,
        tid,
        tcb: tcb_idx as u16,
        va,
        next: NIL,
    });
    table.link_tail(slot);
    let token = table.token(slot);
    if let Some(tcb) = tcbs[tcb_idx].as_mut() {
        tcb.status = TaskStatus::Blocked(WaitReason::Futex(token));
    }
    Ok(token)
}

/// FUTEX-WAIT-ATOMIC1 — why [`compare_and_register_futex_waiter_locked`] published nothing. Every
/// refusal is decided before anything is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FutexCompareRegisterRefusal {
    /// The caller is not the `Running` `{tid, asid}` incarnation. Also the answer, whatever the
    /// word, when the word cannot be read and the caller is not that incarnation.
    IncarnationMoved,
    /// Every waiter slot holds a live record (unreachable in production; see "Capacity").
    Full,
    /// The word did not hold `expected`.
    ValueChanged,
    /// The address did not resolve to a readable word (its own error).
    Word(KernelError),
}

/// FUTEX-WAIT-ATOMIC1 — ranks 2 → 5 → 6, ONE hold: THE compare-and-register of `FutexWait`.
///
/// Resolves `addr`'s key in `asid` (the address space's current generation, or the shared object
/// the mapping names), reads the word once (the atomic acquire load of [`read_word_locked`]),
/// compares it with `expected` ([`futex_should_park`](crate::kernel::syscall::sched::futex_should_park)),
/// and on a match registers the waiter ([`register_futex_waiter_locked`]: the keyed record and
/// `Blocked(Futex(token))` together, for the exact `Running` `{tid, asid}`).
///
/// **The linearization point** of a `FutexWait` that blocks, or that returns because the word
/// differed, is the word's load inside this hold. Every other futex operation that reads or
/// changes the waiter population — `FutexWake`'s claim, `FutexCmpRequeue`'s commit (which reads
/// its own word in the same 2 → 5 → 6 order), a dying task's retirement — holds the task lock to
/// do it, and every mapping change holds the VM lock. So none of them falls between the
/// comparison and the registration: each is entirely before this hold (and a waker's earlier
/// store to the word is visible to the load, because that store precedes the waker's release of
/// the task lock, which precedes this hold's acquisition of it) or entirely after it (and then
/// finds this waiter). A user store that no futex operation follows is ordered by the load alone.
#[allow(clippy::too_many_arguments)]
pub fn compare_and_register_futex_waiter_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    table: &mut FutexWaiterTable,
    spaces: &AddressSpaceManager,
    memory: &MemorySubsystem,
    tid: u64,
    asid: Asid,
    addr: usize,
    expected: u32,
    kind: FutexKind,
) -> Result<(FutexKey, FutexWaitToken), FutexCompareRegisterRefusal> {
    use FutexCompareRegisterRefusal as Refused;
    let (key, word) = match resolve_futex_word_locked(spaces, memory, asid, addr, kind) {
        Ok(found) => found,
        Err(_) if !entering_incarnation(tcbs, tid, asid) => return Err(Refused::IncarnationMoved),
        Err(err) => return Err(Refused::Word(err)),
    };
    if !crate::kernel::syscall::sched::futex_should_park(expected, word) {
        return Err(Refused::ValueChanged);
    }
    register_futex_waiter_locked(tcbs, table, tid, asid, key, addr as u64)
        .map(|token| (key, token))
        .map_err(|refusal| match refusal {
            FutexRegisterRefusal::IncarnationMoved => Refused::IncarnationMoved,
            FutexRegisterRefusal::Full => Refused::Full,
        })
}

/// rank 2, read-only — whether `tid` is the `Running` `{tid, asid}` incarnation the registration
/// accepts. Orders a refusal; decides nothing.
fn entering_incarnation(tcbs: &[Option<ThreadControlBlock>], tid: u64, asid: Asid) -> bool {
    tcbs.iter().flatten().any(|t| {
        t.tid.0 == tid
            && t.asid.unwrap_or(Asid(0)) == asid
            && matches!(t.status, TaskStatus::Running)
    })
}

/// rank 2 — UNDO a registration this caller made: back to `Running` from exactly
/// `Blocked(Futex(token))`, and the record retired, together. Returns `false` (and changes nothing)
/// if the TCB is no longer on that token — a waker won, and the TCB is the waker's.
pub fn undo_futex_registration_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    table: &mut FutexWaiterTable,
    tid: u64,
    token: FutexWaitToken,
) -> bool {
    let Some(tcb) = tcbs
        .iter_mut()
        .flatten()
        .find(|t| t.tid.0 == tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token)))
    else {
        return false;
    };
    tcb.status = TaskStatus::Running;
    let slot = usize::from(token.slot);
    if table.generations[slot] == token.generation {
        table.retire(slot);
    }
    true
}

/// rank 2 — whether `tid` still waits on exactly `token`.
pub fn futex_still_parked_locked(
    tcbs: &[Option<ThreadControlBlock>],
    tid: u64,
    token: FutexWaitToken,
) -> bool {
    tcbs.iter()
        .flatten()
        .any(|t| t.tid.0 == tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token)))
}

/// FUTEX-WAKE-CAP1 — the oldest live waiter of a key, as found by [`next_futex_waiter_locked`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FutexWakeCandidate {
    pub tid: u64,
    pub token: FutexWaitToken,
}

/// rank 2 — the OLDEST live waiter on `key`, without claiming it. Stale records met on the way
/// (in this bucket, of any key) are retired. Mutates no TCB.
///
/// A wake asks this first so that it can secure the waiter's placement (a reserved run-queue
/// slot, at rank 1) before anything irreversible happens to the waiter.
pub fn next_futex_waiter_locked(
    tcbs: &[Option<ThreadControlBlock>],
    table: &mut FutexWaiterTable,
    key: FutexKey,
) -> Option<FutexWakeCandidate> {
    let b = key.bucket();
    let mut prev = NIL;
    let mut cur = table.head[b];
    while cur != NIL {
        let slot = usize::from(cur);
        let rec = table.records[slot]?;
        let next = rec.next;
        if table.live(tcbs, slot) {
            if rec.key == key {
                return Some(FutexWakeCandidate {
                    tid: rec.tid,
                    token: table.token(slot),
                });
            }
            prev = cur;
        } else {
            // Stale: the TCB moved on through some other owner. Reclaim it.
            table.retire_after(slot, prev);
        }
        cur = next;
    }
    None
}

/// rank 2 — THE wake claim, of exactly one waiter: `candidate` is made `Runnable` and its record
/// retired in this one critical section, iff its record is still the live one on `key` that
/// [`next_futex_waiter_locked`] returned. Returns `false` (and changes nothing) otherwise — another
/// owner took it first, and that owner is responsible for it.
///
/// The caller holds a reserved run-queue slot for the waiter when it calls this, so a `true`
/// here is always followed by a placement that cannot be refused for capacity.
pub fn claim_futex_waiter_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    table: &mut FutexWaiterTable,
    key: FutexKey,
    candidate: FutexWakeCandidate,
) -> bool {
    let slot = usize::from(candidate.token.slot);
    if slot >= MAX_FUTEX_WAITERS || table.token(slot) != candidate.token {
        return false;
    }
    let Some(rec) = table.records[slot] else {
        return false;
    };
    if rec.key != key || rec.tid != candidate.tid {
        return false;
    }
    let token = candidate.token;
    let Some(tcb) = tcbs
        .get_mut(usize::from(rec.tcb))
        .and_then(Option::as_mut)
        .filter(|t| {
            t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))
        })
    else {
        return false;
    };
    tcb.status = TaskStatus::Runnable;
    table.retire(slot);
    true
}

/// FUTEX-REQUEUE1 — what a wake loop achieved: how many waiters it placed, and whether it stopped
/// because a waiter it found could not be placed (its home queue full, its CPU unable to take
/// work, or no plan) rather than because none was left. A stopped wake leaves that waiter and every
/// later one parked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FutexWakeOutcome {
    pub woke: u32,
    pub stopped_unplaceable: bool,
}

/// FUTEX-REQUEUE1 — what a successful compare-and-requeue did, as NR 17 reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FutexRequeueOutcome {
    /// Waiters woken (placed exactly once) by the wake portion.
    pub woken: u32,
    /// Waiters moved, still blocked, from the source key to the target key.
    pub requeued: u32,
    /// The wake portion stopped at a waiter it could not place; see
    /// `sched::FUTEX_REQUEUE_STATUS_CAPACITY_LIMITED`.
    pub capacity_limited: bool,
}

/// FUTEX-REQUEUE1 — why a compare-and-requeue transferred nothing. Nothing was mutated for any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FutexRequeueRefusal {
    /// The source word did not hold `expected` at the linearization point.
    ValueChanged,
    /// The source and target addresses resolve to the same key: refused, not reordered.
    SameKey,
    /// A key could not be resolved (range, mapping, or a shared key with no backing object).
    Key(KernelError),
}

// ── FUTEX-REQUEUE-ATOMIC1: compare-and-requeue as ONE operation ─────────────────────────────
//
// NR 17 runs in three phases, and only the middle one decides anything:
//
// 1. PREFLIGHT (task lock and one CPU's scheduler lock, alternately, never nested): for the
//    oldest source waiters, up to `wake`, the placement a wake would use and a run-queue slot
//    RESERVED there — FUTEX-WAKE-CAP1's reservation, made before anything is decided. Its
//    observations may go stale; a reservation grants no ownership of any waiter.
// 2. COMMIT ([`compare_and_requeue_locked`], task → vm → memory held together): the keys, the
//    word, the comparison, and the selection of BOTH portions from one view of the source queue.
//    The wake portion is the longest FIFO prefix of live source waiters whose placement has a
//    reserved slot, up to `wake`; it is moved to a [`FutexKey::Pending`] key private to this
//    operation (still `Blocked(Futex(token))`, token unchanged). The requeue portion is the next
//    up to `requeue` live source waiters, moved to the target. This is the linearization point.
// 3. PLACEMENT: each pending waiter is claimed and its reserved slot filled (the WAKE-CAP1
//    claim → reserved commit), then every unused reservation is released. Nothing but this
//    operation and task exit can reach a pending waiter, so the placement cannot change the
//    outcome the commit fixed.

/// How many times NR 17 re-runs its preflight when the commit finds it stale.
pub const FUTEX_REQUEUE_ATTEMPTS: u32 = 4;

static NEXT_REQUEUE_OP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

/// A fresh operation id for [`FutexKey::Pending`]: unique for the life of the kernel.
pub fn next_requeue_op() -> u64 {
    NEXT_REQUEUE_OP.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
}

const PRIORITIES: usize = 3;

fn priority_from_index(i: usize) -> TaskPriority {
    match i {
        0 => TaskPriority::High,
        1 => TaskPriority::Normal,
        _ => TaskPriority::Low,
    }
}

/// FUTEX-REQUEUE-ATOMIC1 — run-queue slots counted by `(CPU, priority)`: the slots a preflight
/// RESERVED, or those a commit ASSIGNED to its wake portion. Fixed size (`MAX_CPUS × 3 × u16`,
/// 384 bytes with 64 CPUs); a run-queue reservation is a count per CPU and priority, so this is
/// all an operation needs to remember about the slots it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FutexPlacementCounts {
    n: [[u16; PRIORITIES]; MAX_CPUS],
}

impl Default for FutexPlacementCounts {
    fn default() -> Self {
        Self::new()
    }
}

impl FutexPlacementCounts {
    pub const fn new() -> Self {
        Self {
            n: [[0; PRIORITIES]; MAX_CPUS],
        }
    }
    pub fn add(&mut self, cpu: CpuId, priority: TaskPriority) {
        if let Some(c) = self.n.get_mut(usize::from(cpu.0)) {
            c[priority as usize] = c[priority as usize].saturating_add(1);
        }
    }
    /// Take one slot of `(cpu, priority)`; `false` if none is left.
    pub fn take(&mut self, cpu: CpuId, priority: TaskPriority) -> bool {
        match self.n.get_mut(usize::from(cpu.0)) {
            Some(c) if c[priority as usize] > 0 => {
                c[priority as usize] -= 1;
                true
            }
            _ => false,
        }
    }
    pub fn total(&self) -> u32 {
        self.n.iter().flatten().map(|&c| u32::from(c)).sum()
    }
    /// Every non-zero count, as `(cpu, priority, count)`.
    pub fn nonzero(&self) -> impl Iterator<Item = (CpuId, TaskPriority, u16)> + '_ {
        self.n.iter().enumerate().flat_map(|(cpu, per)| {
            per.iter()
                .enumerate()
                .filter(|(_, c)| **c > 0)
                .map(move |(p, &c)| (CpuId(cpu as u8), priority_from_index(p), c))
        })
    }
}

/// FUTEX-REQUEUE-ATOMIC1 — why a preflight secured fewer placements than it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FutexPlacementRefusal {
    /// A slot on `cpu` at `priority` was refused (full queue, offline or wake-only CPU).
    Slot { cpu: CpuId, priority: TaskPriority },
    /// The waiter of this exact wait cycle has no enqueue plan (no class, or not a live task).
    Plan { token: FutexWaitToken },
}

/// FUTEX-REQUEUE-ATOMIC1 — what a preflight secured for the commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FutexWakePreflight {
    /// The run-queue slots it reserved. The operation owns them until it fills or releases each.
    pub held: FutexPlacementCounts,
    /// Where it stopped, if it was refused a placement.
    pub refused: Option<FutexPlacementRefusal>,
    /// The operation's last attempt: the commit may not ask for another preflight.
    pub last_attempt: bool,
}

impl FutexWakePreflight {
    /// Nothing reserved (a `wake = 0` operation, or a word that already differs).
    pub const fn none(last_attempt: bool) -> Self {
        Self {
            held: FutexPlacementCounts::new(),
            refused: None,
            last_attempt,
        }
    }
}

/// FUTEX-REQUEUE-ATOMIC1 — what the commit decided. `Committed` carries the placement counts
/// (~400 bytes); the value is returned once per attempt and lives on the caller's stack, which a
/// kernel path with no allocation prefers to boxing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum FutexRequeueCommit {
    /// Committed: the operation's whole outcome is fixed.
    Committed(FutexRequeueCommitted),
    /// The preflight's reservations do not cover the waiters the wake portion must take now
    /// (another owner changed the source queue in between), and this is not the last attempt.
    /// NOTHING was mutated: release the reservations and preflight again.
    Stale,
}

/// FUTEX-REQUEUE-ATOMIC1 — a committed compare-and-requeue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FutexRequeueCommitted {
    pub source: FutexKey,
    pub target: FutexKey,
    /// The operation id of its pending keys.
    pub op: u64,
    /// Waiters selected for the wake portion — moved to `Pending { op, cpu, priority }`, one
    /// assigned slot each. This is the `woken` NR 17 reports: they are woken at the commit.
    pub woken: u32,
    /// Their placements, by `(cpu, priority)`; each is covered by a reserved slot.
    pub assigned: FutexPlacementCounts,
    /// Waiters moved, still blocked, to the target, in their source order.
    pub moved: u32,
    /// The wake portion stopped short of `wake` at a live waiter whose placement was not secured.
    pub capacity_limited: bool,
}

/// rank 2 — the oldest live waiter on `key` AFTER `after` (the preflight's cursor), or the oldest
/// of all for `None`. `Err(())` if `after` is no longer a live waiter on `key`: another owner
/// changed the queue, so the preflight stops (the commit then sees what it reserved is short).
/// Stale records met on the way are retired, as [`next_futex_waiter_locked`] does.
pub fn next_futex_waiter_after_locked(
    tcbs: &[Option<ThreadControlBlock>],
    table: &mut FutexWaiterTable,
    key: FutexKey,
    after: Option<FutexWakeCandidate>,
) -> Result<Option<FutexWakeCandidate>, ()> {
    let Some(after) = after else {
        return Ok(next_futex_waiter_locked(tcbs, table, key));
    };
    let slot = usize::from(after.token.slot);
    let still = slot < MAX_FUTEX_WAITERS
        && table.token(slot) == after.token
        && table.records[slot].is_some_and(|r| r.key == key && r.tid == after.tid)
        && table.live(tcbs, slot);
    if !still {
        return Err(());
    }
    let mut prev = after.token.slot;
    let mut cur = table.records[slot].map_or(NIL, |r| r.next);
    while cur != NIL {
        let s = usize::from(cur);
        let Some(rec) = table.records[s] else {
            break;
        };
        let next = rec.next;
        if table.live(tcbs, s) {
            if rec.key == key {
                return Ok(Some(FutexWakeCandidate {
                    tid: rec.tid,
                    token: table.token(s),
                }));
            }
            prev = cur;
        } else {
            table.retire_after(s, prev);
        }
        cur = next;
    }
    Ok(None)
}

/// rank 2 — where a wake of `slot`'s waiter would place it (see `homed_wake_placement_locked`).
fn record_placement(
    tcbs: &[Option<ThreadControlBlock>],
    classes: &[Option<TaskClass>],
    table: &FutexWaiterTable,
    slot: usize,
) -> Option<(CpuId, TaskPriority)> {
    let rec = table.records[slot]?;
    crate::kernel::task_enqueue::homed_wake_placement_locked(tcbs, classes, rec.tid)
}

/// Move `slot` (bucket predecessor `prev`) to `key`, appended at that key's tail: unlink, rekey,
/// append. Its slot and incarnation, and so its waiter's token, are unchanged.
fn rekey(table: &mut FutexWaiterTable, slot: usize, prev: u16, key: FutexKey, va: Option<u64>) {
    table.unlink_after(slot, prev);
    if let Some(r) = table.records[slot].as_mut() {
        r.key = key;
        if let Some(va) = va {
            r.va = va;
        }
    }
    table.link_tail(slot);
}

/// rank 2 → 5 → 6 — THE compare-and-requeue commit: the caller holds the task lock, then the vm
/// lock, then the memory lock, for the whole call. This is the operation's linearization point.
///
/// 1. Resolve the source key and read its word, and resolve the target key, from the caller's
///    address space `asid`, under the vm and memory locks.
/// 2. Refuse a target that is the same key (`SameKey`), then compare the word with `expected`
///    (`ValueChanged`). Either refusal writes nothing.
/// 3. Select, read-only, the WAKE PORTION: the live source waiters in FIFO order, each taking one
///    of the preflight's reserved slots for its placement (`homed_wake_placement_locked`: its
///    fixed home CPU and its class's priority — what a wake's plan resolves to), up to `wake`. It
///    ends at the first live waiter whose placement has no reserved slot left:
///    * if the preflight was refused exactly that placement (that CPU and priority, or that
///      waiter's plan), or this is the last attempt, the wake portion is CAPACITY-LIMITED there;
///    * otherwise the preflight is stale: return [`FutexRequeueCommit::Stale`] with nothing
///      written.
///
///    No waiter is ever skipped: the waiter the wake portion stops at is the first one the
///    requeue portion takes.
/// 4. Commit both portions in one walk of the source bucket: the selected waiters are moved, in
///    order, to `Pending { op, cpu, priority }` (still `Blocked(Futex(token))`); the next up to
///    `max_move` live source waiters are moved, in order, to the tail of the target key's queue.
///    Stale records met are retired. No TCB is written. Nothing can register, wake, requeue or
///    exit in between: every one of those takes the task lock, which is held.
///
/// Why one point: registrations, wake claims, other requeues, undo and exit all take the task
/// lock, and the comparison, both selections and both moves happen under one hold of it. After
/// it, the wake set is reachable only through its private key, so no concurrent operation can
/// change which waiters this operation woke or moved; placing them later changes nothing that is
/// observable as a futex outcome.
///
/// Bounded: two walks of one bucket (≤ `MAX_FUTEX_WAITERS` records, O(1) each), no allocation.
/// Records moved into the same bucket are appended after the walk's original tail, which the walk
/// stops at, so none is visited twice.
#[allow(clippy::too_many_arguments)]
pub fn compare_and_requeue_locked(
    tcbs: &[Option<ThreadControlBlock>],
    classes: &[Option<TaskClass>],
    table: &mut FutexWaiterTable,
    spaces: &AddressSpaceManager,
    memory: &MemorySubsystem,
    asid: Asid,
    source: (usize, FutexKind),
    target: (usize, FutexKind),
    expected: u32,
    wake: u32,
    max_move: u32,
    preflight: &FutexWakePreflight,
) -> Result<FutexRequeueCommit, FutexRequeueRefusal> {
    let (s_key, word) = resolve_futex_word_locked(spaces, memory, asid, source.0, source.1)
        .map_err(FutexRequeueRefusal::Key)?;
    let (t_key, _) = resolve_futex_key_locked(spaces, memory, asid, target.0, target.1)
        .map_err(FutexRequeueRefusal::Key)?;
    if s_key == t_key {
        return Err(FutexRequeueRefusal::SameKey);
    }
    if word != expected {
        return Err(FutexRequeueRefusal::ValueChanged);
    }
    let b = s_key.bucket();

    // (3) The wake portion, read-only: how many, and whether it stops short.
    let mut avail = preflight.held;
    let (mut woken, mut capacity_limited) = (0u32, false);
    let mut cur = table.head[b];
    while cur != NIL && woken < wake {
        let slot = usize::from(cur);
        let Some(rec) = table.records[slot] else {
            break;
        };
        if rec.key == s_key && table.live(tcbs, slot) {
            let at = record_placement(tcbs, classes, table, slot);
            match at {
                Some((cpu, priority)) if avail.take(cpu, priority) => woken += 1,
                _ => {
                    let explained = match preflight.refused {
                        Some(FutexPlacementRefusal::Slot { cpu, priority }) => {
                            at == Some((cpu, priority))
                        }
                        Some(FutexPlacementRefusal::Plan { token }) => token == table.token(slot),
                        None => false,
                    };
                    if !explained && !preflight.last_attempt {
                        return Ok(FutexRequeueCommit::Stale);
                    }
                    capacity_limited = true;
                    break;
                }
            }
        }
        cur = rec.next;
    }

    // (4) Both portions, in one walk. `assigned` is built from what is actually moved, so every
    // pending key's waiters are exactly covered by the slots counted for that key.
    let op = next_requeue_op();
    let mut assigned = FutexPlacementCounts::new();
    let last = table.tail[b];
    let mut prev = NIL;
    let mut cur = table.head[b];
    let (mut selected, mut moved) = (0u32, 0u32);
    while cur != NIL && (selected < woken || moved < max_move) {
        let slot = usize::from(cur);
        let Some(rec) = table.records[slot] else {
            break;
        };
        let next = rec.next;
        let at_last = cur == last;
        if !table.live(tcbs, slot) {
            table.retire_after(slot, prev);
        } else if rec.key != s_key {
            prev = cur;
        } else if selected < woken
            && let Some((cpu, priority)) = record_placement(tcbs, classes, table, slot)
        {
            // Step 3 proved this waiter's placement has a slot: the walk meets the same waiters in
            // the same order, because nothing can change under the held locks. (Were a placement
            // ever missing here, the waiter would simply begin the requeue portion below, and
            // `woken` would count only the waiters actually selected.)
            let pending = FutexKey::Pending {
                op,
                cpu: cpu.0,
                priority: priority as u8,
            };
            rekey(table, slot, prev, pending, None);
            assigned.add(cpu, priority);
            selected += 1;
        } else if moved < max_move {
            if selected < woken {
                woken = selected;
            }
            rekey(table, slot, prev, t_key, Some(target.0 as u64));
            moved += 1;
        } else {
            break;
        }
        if at_last {
            break;
        }
        cur = next;
    }
    let woken = selected;
    Ok(FutexRequeueCommit::Committed(FutexRequeueCommitted {
        source: s_key,
        target: t_key,
        op,
        woken,
        assigned,
        moved,
        capacity_limited,
    }))
}

/// FUTEX-REQUEUE-ATOMIC1 — the live RACE witness (feature `futex-requeue-race-witness`, armed by
/// `yarm.futex_requeue_witness=1`). NR 17 holds the window between its commit and its placement —
/// the interval that used to separate its transfer from its wake portion — open until a FutexWake
/// that STARTED inside it has finished, so the competing wake is causally inside the window, not
/// merely near it in time. Each step is logged in causal order: `..._WINDOW_OPEN` before the window
/// is published; `..._IN_WINDOW_WAKE` by the waker before it releases the window; `..._CLOSE` after.
/// Bounded: the window gives up after `SPIN_BOUND` polls and reports `competing_wakes=0`.
#[cfg(feature = "futex-requeue-race-witness")]
pub mod race_witness {
    use core::sync::atomic::{AtomicU64, Ordering::SeqCst};
    static OPEN: AtomicU64 = AtomicU64::new(0);
    static NEXT: AtomicU64 = AtomicU64::new(1);
    static WAKES: AtomicU64 = AtomicU64::new(0);
    const SPIN_BOUND: u64 = 400_000_000;

    /// NR 17, after its commit and before its placement: open the window and hold it.
    pub fn hold_window(cpu: u8, woken: u32, requeued: u32) {
        let id = NEXT.fetch_add(1, SeqCst);
        let before = WAKES.load(SeqCst);
        crate::yarm_log!(
            "FUTEX_REQUEUE_RACE_WINDOW_OPEN cpu={} window={} woken={} requeued={}",
            cpu,
            id,
            woken,
            requeued
        );
        OPEN.store(id, SeqCst);
        let mut spins = 0u64;
        while WAKES.load(SeqCst) == before && spins < SPIN_BOUND {
            core::hint::spin_loop();
            spins += 1;
        }
        OPEN.store(0, SeqCst);
        crate::yarm_log!(
            "FUTEX_REQUEUE_RACE_WINDOW_CLOSE cpu={} window={} competing_wakes={} spins={}",
            cpu,
            id,
            WAKES.load(SeqCst) - before,
            spins
        );
    }

    /// NR 10, before it selects anything: the window open now, if any.
    pub fn wake_begin() -> u64 {
        OPEN.load(SeqCst)
    }

    /// NR 10, after its wake: report it, and release the window, iff it ran wholly inside one.
    pub fn wake_end(window: u64, cpu: u8, tid: u64, woke: u32) {
        if window != 0 && OPEN.load(SeqCst) == window {
            crate::yarm_log!(
                "FUTEX_REQUEUE_RACE_IN_WINDOW_WAKE cpu={} tid={} window={} woke={}",
                cpu,
                tid,
                window,
                woke
            );
            WAKES.fetch_add(1, SeqCst);
        }
    }
}

/// FUTEX-WAIT-ATOMIC1 — the live wait witness (default off: feature `futex-wait-witness`, armed by
/// `yarm.futex_requeue_witness=1`, and only for a word whose high half is [`wait_witness::MAGIC`]).
///
/// A witness `FutexWait` holds two windows open, each until a `FutexWake` that STARTED inside it
/// has finished: **B** after the wait has decided to park and before its compare-and-register, and
/// **A** after the registration and before the compare-and-clear of its current slot. Causal order
/// on one console: `..._WINDOW_OPEN` before the window is published, the waker's
/// `..._IN_WINDOW_WAKE` before it releases it, `..._WINDOW_CLOSE` after (B's close also reports the
/// word it reads then, so a grader can tell whether the window saw a store). `..._RESULT` is the
/// wait's own outcome. A wait opens its windows only while a sibling thread of the waiter (same
/// address space, another TID) is current on another CPU; otherwise it is logged `..._SKIPPED` and
/// runs unwindowed. RISCV-PARK-RET1: a window is bounded and fails closed — it re-checks every
/// `PEER_RECHECK` polls that such a sibling is still current, and gives up when none has been for
/// `PEER_MISSES` checks in a row (`end=no_peer`: an explicit refusal — the siblings' own switches
/// briefly empty that CPU's slot, hence the debounce) or after `SPIN_BOUND` polls (`end=bound`: a
/// failure). A window never waits on a task that is merely queued.
#[cfg(feature = "futex-wait-witness")]
pub mod wait_witness {
    use core::sync::atomic::{AtomicU64, Ordering::SeqCst};
    /// The high half of a witness word.
    pub const MAGIC: u32 = 0xF7A1_0000;
    static OPEN: AtomicU64 = AtomicU64::new(0);
    static NEXT_WINDOW: AtomicU64 = AtomicU64::new(1);
    static NEXT_WAIT: AtomicU64 = AtomicU64::new(1);
    static WAKES: AtomicU64 = AtomicU64::new(0);
    /// The witness has one waiter thread, so its last wait is the one in flight.
    static LAST_WAIT: AtomicU64 = AtomicU64::new(0);
    const SPIN_BOUND: u64 = 50_000_000;
    const PEER_RECHECK: u64 = 1 << 16;
    const PEER_MISSES: u32 = 8;

    fn armed(expected: u32) -> bool {
        crate::kernel::boot::futex_requeue_witness_enabled() && expected & 0xFFFF_0000 == MAGIC
    }

    /// A new witness wait id, when this wait is the witness's.
    pub fn begin(expected: u32) -> Option<u64> {
        armed(expected).then(|| {
            let w = NEXT_WAIT.fetch_add(1, SeqCst);
            LAST_WAIT.store(w, SeqCst);
            w
        })
    }

    /// A witness wait that opens no window: no sibling thread of the waiter is current on another
    /// CPU, so no competing wake could arrive (a window held then would only spin to its bound).
    pub fn skip(wait: u64, cpu: u8, tid: u64) {
        crate::yarm_log!(
            "FUTEX_WAIT_WITNESS_SKIPPED wait={} cpu={} tid={} reason=no_peer_running",
            wait,
            cpu,
            tid
        );
    }

    /// The witness wait the park transaction just ran, for the route's outcome line.
    pub fn current_wait(expected: u32) -> Option<u64> {
        armed(expected).then(|| LAST_WAIT.load(SeqCst))
    }

    /// How a window ended: a wake ran inside it, the bound ran out, or no sibling was current.
    #[derive(Clone, Copy)]
    pub enum End {
        Woke,
        Bound,
        NoPeer,
    }

    /// Hold window `kind` (`B` or `A`) of wait `wait` open until a competing wake has run in it,
    /// while `peer()` (a sibling of the waiter is current on another CPU) holds, for at most
    /// `SPIN_BOUND` polls. Returns the window id, how many wakes ran wholly inside it, and why it
    /// ended.
    pub fn hold(
        kind: char,
        wait: u64,
        cpu: u8,
        tid: u64,
        peer: &dyn Fn() -> bool,
    ) -> (u64, u64, End) {
        let id = NEXT_WINDOW.fetch_add(1, SeqCst);
        let before = WAKES.load(SeqCst);
        crate::yarm_log!(
            "FUTEX_WAIT_WITNESS_WINDOW_OPEN kind={} wait={} window={} cpu={} tid={}",
            kind,
            wait,
            id,
            cpu,
            tid
        );
        OPEN.store(id, SeqCst);
        let mut spins = 0u64;
        let mut end = End::Woke;
        let mut misses = 0u32;
        while WAKES.load(SeqCst) == before {
            if spins >= SPIN_BOUND {
                end = End::Bound;
                break;
            }
            if spins % PEER_RECHECK == PEER_RECHECK - 1 {
                if peer() {
                    misses = 0;
                } else {
                    misses += 1;
                    if misses >= PEER_MISSES {
                        end = End::NoPeer;
                        break;
                    }
                }
            }
            core::hint::spin_loop();
            spins += 1;
        }
        OPEN.store(0, SeqCst);
        (id, WAKES.load(SeqCst) - before, end)
    }

    /// Close window `id`; `word` is what the witness reads now (B only; `None` for A).
    pub fn close(
        kind: char,
        wait: u64,
        (id, competing, end): (u64, u64, End),
        cpu: u8,
        expected: u32,
        word: Option<u32>,
    ) {
        let end = match end {
            End::Woke => "woke",
            End::Bound => "bound",
            End::NoPeer => "no_peer",
        };
        match word {
            Some(word) => crate::yarm_log!(
                "FUTEX_WAIT_WITNESS_WINDOW_CLOSE kind={} wait={} window={} cpu={} competing_wakes={} end={} expected=0x{:x} word=0x{:x}",
                kind,
                wait,
                id,
                cpu,
                competing,
                end,
                expected,
                word
            ),
            None => crate::yarm_log!(
                "FUTEX_WAIT_WITNESS_WINDOW_CLOSE kind={} wait={} window={} cpu={} competing_wakes={} end={}",
                kind,
                wait,
                id,
                cpu,
                competing,
                end
            ),
        }
    }

    /// The wait's outcome.
    pub fn result(wait: u64, tid: u64, outcome: &crate::kernel::syscall::sched::FutexParkOutcome) {
        use crate::kernel::syscall::sched::FutexParkOutcome as O;
        let slug = match outcome {
            O::Parked { .. } => "parked",
            O::WokenDuringPublication { .. } => "woken_during_publication",
            O::ValueChanged => "value_changed",
            O::WordUnreadable(_) => "word_unreadable",
            O::VictimChanged => "victim_changed",
            O::IncarnationMoved => "incarnation_moved",
            O::CapacityExhausted => "capacity_exhausted",
        };
        crate::yarm_log!(
            "FUTEX_WAIT_WITNESS_RESULT wait={} tid={} outcome={}",
            wait,
            tid,
            slug
        );
    }

    /// NR 10, before it selects anything: the window open now, if any.
    pub fn wake_begin() -> u64 {
        OPEN.load(SeqCst)
    }

    /// NR 10, after its wake: report it, and release the window, iff it ran wholly inside one.
    pub fn wake_end(window: u64, cpu: u8, tid: u64, woke: u32) {
        if window != 0 && OPEN.load(SeqCst) == window {
            crate::yarm_log!(
                "FUTEX_WAIT_WITNESS_IN_WINDOW_WAKE cpu={} tid={} window={} woke={}",
                cpu,
                tid,
                window,
                woke
            );
            WAKES.fetch_add(1, SeqCst);
        }
    }
}

/// rank 2 — claim the oldest waiter still on the pending key `key` (`Runnable`, record retired),
/// exactly as a wake claims. `None` once no live waiter is left on it (the rest exited, or undid
/// their own registration in their publication window).
pub fn claim_pending_futex_waiter_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    table: &mut FutexWaiterTable,
    key: FutexKey,
) -> Option<u64> {
    let candidate = next_futex_waiter_locked(tcbs, table, key)?;
    claim_futex_waiter_locked(tcbs, table, key, candidate).then_some(candidate.tid)
}

/// rank 2 — retire every record naming `tid` (its TCB is being torn down). Eager reclamation; a
/// record left behind would be stale and inert anyway. Returns how many were retired.
pub fn retire_futex_waiters_for_task_locked(table: &mut FutexWaiterTable, tid: u64) -> usize {
    let mut n = 0;
    for slot in 0..MAX_FUTEX_WAITERS {
        if table.records[slot].is_some_and(|r| r.tid == tid) {
            table.retire(slot);
            n += 1;
        }
    }
    n
}

/// rank 2 — how many LIVE waiters `key` has. Test use.
#[cfg(test)]
pub fn live_futex_waiters_locked(
    tcbs: &[Option<ThreadControlBlock>],
    table: &FutexWaiterTable,
    key: FutexKey,
) -> usize {
    (0..MAX_FUTEX_WAITERS)
        .filter(|&i| table.records[i].is_some_and(|r| r.key == key) && table.live(tcbs, i))
        .count()
}

/// rank 2 — how many LIVE waiters named `va` (any key). Test use.
#[cfg(test)]
pub fn live_futex_waiters_at_va_locked(
    tcbs: &[Option<ThreadControlBlock>],
    table: &FutexWaiterTable,
    va: u64,
) -> usize {
    (0..MAX_FUTEX_WAITERS)
        .filter(|&i| table.records[i].is_some_and(|r| r.va == va) && table.live(tcbs, i))
        .count()
}

/// FUTEX-FOUND1: a test-only phase handshake between the park transaction's rank-2 registration
/// and its rank-1 compare-and-clear — the window a concurrent waker lands in.
#[cfg(test)]
pub(crate) mod futex_phase_hook {
    extern crate std;
    use core::cell::RefCell;
    type Hook = alloc::boxed::Box<dyn FnMut()>;
    std::thread_local! {
        static AFTER_REGISTER: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }
    pub(crate) fn arm_after_register(hook: Hook) {
        AFTER_REGISTER.with(|h| *h.borrow_mut() = Some(hook));
    }
    pub(crate) fn run_after_register() {
        let hook = AFTER_REGISTER.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }

    // FUTEX-WAIT-ATOMIC1 — the wait's other boundaries, for tests. `BEFORE_REGISTER` runs once
    // the wait has decided to park and before its compare-and-register; `AFTER_CLEAR` after the
    // compare-and-clear of the current slot and before the read-back. One-shot.
    std::thread_local! {
        static BEFORE_REGISTER: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static AFTER_CLEAR: RefCell<Option<Hook>> = const { RefCell::new(None) };
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
    pub(crate) fn arm_after_clear(hook: Hook) {
        AFTER_CLEAR.with(|h| *h.borrow_mut() = Some(hook));
    }
    pub(crate) fn run_after_clear() {
        let hook = AFTER_CLEAR.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }

    /// FUTEX-WAKE-CAP1 — the wake's own windows, for tests: `WAKE_AFTER_RESERVE` runs after a
    /// waiter's placement is reserved and before it is claimed; `WAKE_AFTER_CLAIM` after the
    /// claim and before the reserved commit. One-shot, like the registration hook.
    pub(crate) const WAKE_AFTER_RESERVE: u8 = 1;
    pub(crate) const WAKE_AFTER_CLAIM: u8 = 2;
    type WakeHook = alloc::boxed::Box<dyn FnMut(u8, u64) -> bool>;
    std::thread_local! {
        static WAKE_STEP: RefCell<Option<WakeHook>> = const { RefCell::new(None) };
    }
    /// The hook sees (step, tid) and returns whether to stay armed.
    pub(crate) fn arm_wake_step(hook: WakeHook) {
        WAKE_STEP.with(|h| *h.borrow_mut() = Some(hook));
    }
    pub(crate) fn run_wake_step(step: u8, tid: u64) {
        let hook = WAKE_STEP.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook
            && hook(step, tid)
        {
            WAKE_STEP.with(|h| *h.borrow_mut() = Some(hook));
        }
    }

    /// FUTEX-REQUEUE-ATOMIC1 — the compare-and-requeue's own window, for tests: after the
    /// transaction that compares and moves has released its locks, before the woken waiters are
    /// placed. One-shot.
    std::thread_local! {
        static REQUEUE_WINDOW: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }
    pub(crate) fn arm_requeue_window(hook: Hook) {
        REQUEUE_WINDOW.with(|h| *h.borrow_mut() = Some(hook));
    }
    pub(crate) fn run_requeue_window() {
        let hook = REQUEUE_WINDOW.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }
}

/// Test-only fixtures: the private key a task would resolve, and a waiter published through the
/// table exactly as the registration owner publishes one (record and token together).
#[cfg(test)]
impl super::KernelState {
    /// The private key `tid` resolves for `va`: its address space's incarnation, or the reserved
    /// `Asid(0)` / generation `0` for a task with no address space.
    pub(crate) fn private_futex_key_for_test(&self, tid: u64, va: usize) -> FutexKey {
        let asid = self.task_asid(tid).unwrap_or(Asid(0));
        let aspace_generation = self.with_user_spaces(|m| m.generation(asid)).unwrap_or(0);
        FutexKey::Private {
            asid,
            aspace_generation,
            va: va as u64,
        }
    }

    /// Publish `tid` as a waiter on its private key for `va`, from whatever status it holds,
    /// through [`register_futex_waiter_locked`] — so a production wake finds it.
    pub(crate) fn inject_futex_waiter_for_test(&mut self, tid: u64, va: usize) -> FutexWaitToken {
        let key = self.private_futex_key_for_test(tid, va);
        let asid = self.task_asid(tid).unwrap_or(Asid(0));
        // The registration admits only `Running`; the fixture's task may hold any status.
        self.set_task_status_for_test(tid, TaskStatus::Running);
        self.with_task_futex_mut(|tcbs, table| {
            register_futex_waiter_locked(tcbs.as_mut_slice(), table, tid, asid, key, va as u64)
                .expect("inject futex waiter")
        })
    }
}

#[cfg(test)]
impl super::KernelState {
    /// Whether `tid` is a LIVE futex waiter whose record named `va`.
    pub(crate) fn futex_parked_at_va_for_test(&self, tid: u64, va: u64) -> bool {
        self.with_tcbs(|tcbs| {
            let table = super::kernel_ref(&self.futex_waiters);
            tcbs.iter().flatten().any(|t| {
                t.tid.0 == tid
                    && matches!(t.status, TaskStatus::Blocked(WaitReason::Futex(token))
                        if table.va_of(token) == Some(va))
            })
        })
    }
}
