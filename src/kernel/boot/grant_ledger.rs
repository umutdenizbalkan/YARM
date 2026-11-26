// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VFS-LIFETIME1 — the file-grant ledger: what the file-grant path (NR 28 → FILE_GRANT_RO →
//! NR 29) currently holds, counted from the kernel's own tables.
//!
//! * `live_slice_objects` — `InitramfsFileSlice` MemoryObjects still in the registry (one per
//!   FILE_GRANT_RO that was never fully released);
//! * `slice_cap_refs` — the sum of their capability reference counts;
//! * per process CNode: how many occupied slots name such an object (`slice`), next to the
//!   CNode's total occupancy (`occupied`, which also counts spawn outputs, endpoints and every
//!   other permanent capability — the grant-related share is `slice`);
//! * `links` — live delegation links (every IPC capability transfer records one).
//!
//! The counting is unconditional and hosted-tested. Printing it is the default-off
//! `vfs-lifetime-witness` feature: after every NR 22 (including a refused one — `CapRelinquish(0)`
//! is init's checkpoint probe), after every NR 28 and at every NR 29 use. The two reads are taken
//! one after the other, rank 6 (memory) then rank 4 (capability), never nested, so the ledger is a
//! checkpoint, not an atomic snapshot: the witness takes it at points where the grant path is
//! quiescent.

use super::*;

/// One process CNode's share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct LedgerCnode {
    pub(crate) pid: u64,
    pub(crate) occupied: usize,
    pub(crate) slice: usize,
}

pub(crate) const LEDGER_MAX_OBJECTS: usize = 64;
pub(crate) const LEDGER_MAX_CNODES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GrantLedger {
    pub(crate) live_slice_objects: usize,
    pub(crate) slice_cap_refs: u64,
    pub(crate) slice_slots: usize,
    pub(crate) links: usize,
    pub(crate) cnodes: [LedgerCnode; LEDGER_MAX_CNODES],
    pub(crate) cnode_count: usize,
    /// Objects or CNodes beyond the fixed arrays (the counts above stay exact for objects; a
    /// CNode past the array is still counted in `slice_slots`).
    pub(crate) truncated: bool,
}

/// Rank 6: the live slice objects' ids and their capability reference total.
pub(crate) fn slice_objects_locked(
    memory: &MemorySubsystem,
) -> ([u64; LEDGER_MAX_OBJECTS], usize, usize, u64) {
    let mut ids = [0u64; LEDGER_MAX_OBJECTS];
    let mut kept = 0usize;
    let mut live = 0usize;
    let mut refs = 0u64;
    for object in memory.memory_objects.iter().flatten() {
        if matches!(object.kind, MemoryObjectKind::InitramfsFileSlice { .. }) {
            live += 1;
            refs += u64::from(object.cap_refcount);
            if kept < ids.len() {
                ids[kept] = object.id;
                kept += 1;
            }
        }
    }
    (ids, kept, live, refs)
}

/// Rank 4: every process CNode's occupancy and slice slots, and the live link count.
pub(crate) fn ledger_from_capability_locked(
    capability: &CapabilitySubsystem,
    objects: ([u64; LEDGER_MAX_OBJECTS], usize, usize, u64),
) -> GrantLedger {
    let (ids, kept, live, refs) = objects;
    let ids = &ids[..kept];
    let mut ledger = GrantLedger {
        live_slice_objects: live,
        slice_cap_refs: refs,
        slice_slots: 0,
        links: kernel_ref(&capability.delegated_capability_links)
            .iter()
            .flatten()
            .count(),
        cnodes: [LedgerCnode::default(); LEDGER_MAX_CNODES],
        cnode_count: 0,
        truncated: kept < live,
    };
    for record in capability.process_cnodes.iter().flatten() {
        let Some(space) = capability
            .cnode_spaces
            .iter()
            .flatten()
            .find(|space| space.id == record.cnode)
        else {
            continue;
        };
        let cspace = kernel_ref(&space.cspace);
        let slice: usize = ids
            .iter()
            .map(|id| cspace.memory_object_id_refcount(*id))
            .sum();
        ledger.slice_slots += slice;
        if ledger.cnode_count < LEDGER_MAX_CNODES {
            ledger.cnodes[ledger.cnode_count] = LedgerCnode {
                pid: record.pid,
                occupied: cspace.occupied_slots(),
                slice,
            };
            ledger.cnode_count += 1;
        } else {
            ledger.truncated = true;
        }
    }
    ledger
}

/// The ledger through the split accessors (rank 6, released, then rank 4).
#[cfg_attr(
    not(all(feature = "vfs-lifetime-witness", not(feature = "hosted-dev"))),
    allow(dead_code)
)]
pub(crate) fn ledger_split(shared: &crate::runtime::SharedKernel) -> GrantLedger {
    let objects = shared.with_memory_split_mut(|memory| slice_objects_locked(memory));
    shared.with_capability_state_split_mut(|capability| {
        ledger_from_capability_locked(capability, objects)
    })
}

/// `vfs-lifetime-witness`: print the ledger after `event` by `tid` (`mo` = the object concerned,
/// 0 when none).
#[cfg(all(feature = "vfs-lifetime-witness", not(feature = "hosted-dev")))]
pub(crate) fn print_ledger_split(
    shared: &crate::runtime::SharedKernel,
    event: &str,
    tid: u64,
    mo: u64,
) {
    let ledger = ledger_split(shared);
    print_ledger(&ledger, event, tid, mo);
}

#[cfg_attr(
    not(all(feature = "vfs-lifetime-witness", not(feature = "hosted-dev"))),
    allow(dead_code)
)]
pub(crate) fn print_ledger(ledger: &GrantLedger, event: &str, tid: u64, mo: u64) {
    crate::yarm_log!(
        "VL1_LEDGER event={} tid={} mo_id={} live_slice_mo={} slice_cap_refs={} slice_slots={} links={} cnodes={} truncated={}",
        event,
        tid,
        mo,
        ledger.live_slice_objects,
        ledger.slice_cap_refs,
        ledger.slice_slots,
        ledger.links,
        ledger.cnode_count,
        u8::from(ledger.truncated)
    );
    for c in &ledger.cnodes[..ledger.cnode_count] {
        crate::yarm_log!(
            "VL1_LEDGER_CNODE event={} pid={} occupied={} slice={}",
            event,
            c.pid,
            c.occupied,
            c.slice
        );
    }
}

/// `vfs-lifetime-witness`: at NR 29's use of `mo`, how many capabilities and mappings name it.
#[cfg(all(feature = "vfs-lifetime-witness", not(feature = "hosted-dev")))]
pub(crate) fn print_grant_use_split(
    shared: &crate::runtime::SharedKernel,
    tid: u64,
    image_id: u64,
    cap: u64,
    mo: u64,
) {
    let refs = shared.with_memory_split_mut(|memory| {
        memory
            .memory_objects
            .iter()
            .flatten()
            .find(|object| object.id == mo)
            .map(|object| (object.cap_refcount, object.map_refcount))
    });
    let (cap_refs, map_refs) = refs.unwrap_or((0, 0));
    crate::yarm_log!(
        "VL1_GRANT_USE tid={} image_id={} cap={} mo_id={} cap_refcount={} map_refcount={}",
        tid,
        image_id,
        cap,
        mo,
        cap_refs,
        map_refs
    );
    print_ledger_split(shared, "nr29_use", tid, mo);
}
