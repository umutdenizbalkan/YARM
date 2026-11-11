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
//!   ([`register_futex_waiter_locked`]);
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
use crate::kernel::task::{TaskStatus, ThreadControlBlock, WaitReason};
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
    // SAFETY: `phys` is inside a validated user-readable mapping held stable by the vm lock, the
    // direct-map pointer is bounds-checked, and the word is 4-byte aligned.
    Ok(unsafe { core::ptr::read_volatile(ptr as *const u32) })
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

/// FUTEX-REQUEUE1 — what the atomic transfer step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FutexRequeueTransfer {
    pub source: FutexKey,
    pub target: FutexKey,
    /// The live source waiters left at the head of the source queue for the wake portion.
    pub kept: u32,
    /// The live source waiters moved to the target, in their source order.
    pub moved: u32,
}

/// rank 2 → 5 → 6 — THE compare-and-requeue transfer: the caller holds the task lock, then the vm
/// lock, then the memory lock, for the whole call. This is the operation's linearization point.
///
/// 1. Resolve the source key and read its word, and resolve the target key, from the caller's
///    address space `asid` — under the vm and memory locks, so no mapping can change in between.
/// 2. Refuse a target that is the same key (`SameKey`), then compare the word with `expected`
///    (`ValueChanged`). Either refusal writes nothing.
/// 3. In one walk of the source bucket: leave the first `keep` live source waiters in place (the
///    wake portion's), then move up to `max_move` further live source waiters, in source order, to
///    the tail of the target key's queue. A moved record keeps its slot and incarnation, so its
///    waiter's token is unchanged; only its key (and its diagnostic VA, now the caller's target
///    address) changes. No TCB is written: a moved waiter stays `Blocked(Futex(token))`.
///
/// Why this is atomic: every waiter registration takes the task lock, which is held here, so a
/// waiter is either registered before this step (and is a candidate) or after it (and is not).
/// The comparison is made under the same hold. A registered waiter still inside its own wait's
/// publication window is moved like any other: its re-read, undo and read-back are by token,
/// which the move does not touch (see `doc/SYSCALL_ABI.md`).
///
/// Bounded: one walk of one bucket (≤ `MAX_FUTEX_WAITERS` records, O(1) each). When source and
/// target share a bucket, moved records are appended after the walk's original tail, which the
/// walk stops at, so none is visited twice.
#[allow(clippy::too_many_arguments)]
pub fn compare_and_requeue_locked(
    tcbs: &[Option<ThreadControlBlock>],
    table: &mut FutexWaiterTable,
    spaces: &AddressSpaceManager,
    memory: &MemorySubsystem,
    asid: Asid,
    source: (usize, FutexKind),
    target: (usize, FutexKind),
    expected: u32,
    keep: u32,
    max_move: u32,
) -> Result<FutexRequeueTransfer, FutexRequeueRefusal> {
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
    let last = table.tail[b];
    let mut prev = NIL;
    let mut cur = table.head[b];
    let (mut kept, mut moved) = (0u32, 0u32);
    while cur != NIL && moved < max_move {
        let slot = usize::from(cur);
        let Some(rec) = table.records[slot] else {
            break;
        };
        let next = rec.next;
        let at_last = cur == last;
        if !table.live(tcbs, slot) {
            // Stale: reclaimed on the way, exactly as a wake does.
            table.retire_after(slot, prev);
        } else if rec.key != s_key {
            prev = cur;
        } else if kept < keep {
            kept += 1;
            prev = cur;
        } else {
            table.unlink_after(slot, prev);
            if let Some(r) = table.records[slot].as_mut() {
                r.key = t_key;
                r.va = target.0 as u64;
            }
            table.link_tail(slot);
            moved += 1;
        }
        if at_last {
            break;
        }
        cur = next;
    }
    Ok(FutexRequeueTransfer {
        source: s_key,
        target: t_key,
        kept,
        moved,
    })
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
