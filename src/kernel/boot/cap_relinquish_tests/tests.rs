// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VFS-LIFETIME1 — NR 22 `CapRelinquish`, driven through its production owners on both routes.
//!
//! The shape is the file grant's: a root MemoryObject capability in one process (initramfs),
//! delegated to a relay (VFS) and from there to the recipient (PM). Each test states which holder
//! releases, and checks the three things F08 is about: the other holders still resolve, the
//! object's capability count, and — at the last release — the registry entry.

use super::cap_relinquish::{
    BroadRelinquishOwners, RelinquishLinkPlan, RelinquishOwners, RelinquishRefusal,
    SplitRelinquishOwners, commit_relinquish_locked, plan_relinquish_links, run_relinquish,
};
use super::{Bootstrap, DelegatedCapabilityLink, KernelError, KernelState};
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::ipc::ThreadId;
use crate::runtime::SharedKernel;

const SOURCE: u64 = 0; // the bootstrap task stands in for initramfs: it owns the root
const RELAY: u64 = 7301; // VFS
const RECIPIENT: u64 = 7302; // PM
const OTHER: u64 = 7303;

struct Chain {
    object: CapObject,
    root: CapId,
    relay: CapId,
    leaf: CapId,
}

fn chain(state: &mut KernelState) -> Chain {
    chain_with(state, |state| {
        state.alloc_anonymous_memory_object().expect("object")
    })
}

#[repr(C, align(4096))]
struct Initrd([u8; 2 * crate::kernel::vm::PAGE_SIZE]);

/// The production grant shape: the root is an initramfs file slice (borrowed backing).
fn slice_chain(state: &mut KernelState, initrd: &Initrd) -> Chain {
    chain_with(state, |state| {
        state
            .create_initramfs_file_slice_mo(&initrd.0[..], 0, 300)
            .expect("slice")
    })
}

fn chain_with(
    state: &mut KernelState,
    create: impl FnOnce(&mut KernelState) -> (u64, CapId),
) -> Chain {
    state.register_task(RELAY).expect("relay");
    state.register_task(RECIPIENT).expect("recipient");
    let (id, root) = create(state);
    let relay = state
        .grant_capability_task_to_task_with_rights(
            SOURCE,
            root,
            RELAY,
            CapRights::READ | CapRights::MAP,
        )
        .expect("source -> relay");
    let leaf = state
        .grant_capability_task_to_task_with_rights(
            RELAY,
            relay,
            RECIPIENT,
            CapRights::READ | CapRights::MAP,
        )
        .expect("relay -> recipient");
    Chain {
        object: CapObject::MemoryObject { id },
        root,
        relay,
        leaf,
    }
}

fn refs(state: &KernelState, object: CapObject) -> Option<u32> {
    let CapObject::MemoryObject { id } = object else {
        return None;
    };
    state.with_memory_state(|memory| {
        memory
            .memory_objects
            .iter()
            .flatten()
            .find(|mo| mo.id == id)
            .map(|mo| mo.cap_refcount)
    })
}

fn links(state: &KernelState) -> alloc::vec::Vec<DelegatedCapabilityLink> {
    state.with_capability_state(|capability| {
        super::kernel_ref(&capability.delegated_capability_links)
            .iter()
            .flatten()
            .copied()
            .collect()
    })
}

fn relinquish(
    state: &mut KernelState,
    tid: u64,
    cap: CapId,
) -> Result<super::cap_relinquish::RelinquishOutcome, RelinquishRefusal> {
    let owners = BroadRelinquishOwners {
        kernel: core::cell::RefCell::new(state),
    };
    run_relinquish(&owners, tid, cap)
}

#[test]
fn a_relay_release_keeps_the_recipient_and_hands_its_link_to_the_source() {
    let mut state = Bootstrap::init().expect("init");
    let c = chain(&mut state);
    assert_eq!(refs(&state, c.object), Some(3));

    let out = relinquish(&mut state, RELAY, c.relay).expect("relay releases");
    assert_eq!(out.reparented, 1);
    assert_eq!(out.parent, Some((SOURCE, c.root)));
    assert!(!out.reclaimed);
    assert_eq!(refs(&state, c.object), Some(2));
    assert!(
        state.resolve_capability_for_task(RELAY, c.relay).is_err(),
        "the relay's slot is gone"
    );
    let leaf = state
        .resolve_capability_for_task(RECIPIENT, c.leaf)
        .expect("the recipient keeps its grant");
    assert_eq!(leaf.object, c.object);
    assert_eq!(
        leaf.rights(),
        CapRights::READ | CapRights::MAP,
        "rights unchanged"
    );
    // The recipient's link now names the source: the source keeps its revocation authority.
    assert_eq!(
        links(&state),
        alloc::vec![DelegatedCapabilityLink {
            source_tid: SOURCE,
            source_cap: c.root,
            dest_tid: RECIPIENT,
            dest_cap: c.leaf,
        }]
    );
    let root_cnode = state.task_cnode(SOURCE).expect("cnode");
    state
        .revoke_capability_in_cnode(root_cnode, c.root)
        .expect("source revokes");
    assert!(
        state
            .resolve_capability_for_task(RECIPIENT, c.leaf)
            .is_err(),
        "and its revoke still reaches the recipient"
    );
    assert_eq!(
        refs(&state, c.object),
        None,
        "the last reference went with the revoke"
    );
}

#[test]
fn a_root_release_makes_its_children_roots_and_the_last_release_reclaims() {
    let mut state = Bootstrap::init().expect("init");
    let c = chain(&mut state);
    let out = relinquish(&mut state, SOURCE, c.root).expect("source releases");
    assert_eq!(
        (out.reparented, out.parent, out.reclaimed),
        (1, None, false)
    );
    assert!(state.resolve_capability_for_task(RELAY, c.relay).is_ok());
    assert!(state.resolve_capability_for_task(RECIPIENT, c.leaf).is_ok());
    assert_eq!(
        links(&state).len(),
        1,
        "relay -> recipient remains; source -> relay is gone"
    );

    let out = relinquish(&mut state, RELAY, c.relay).expect("relay releases");
    assert_eq!(
        (out.reparented, out.parent, out.reclaimed),
        (1, None, false)
    );
    assert!(links(&state).is_empty(), "the recipient is a root now");
    assert_eq!(refs(&state, c.object), Some(1));
    assert!(
        state.resolve_capability_for_task(RECIPIENT, c.leaf).is_ok(),
        "still usable"
    );

    let out = relinquish(&mut state, RECIPIENT, c.leaf).expect("recipient releases");
    assert_eq!((out.reparented, out.reclaimed), (0, true));
    assert_eq!(refs(&state, c.object), None, "the registry entry is gone");
}

#[test]
fn a_retried_or_stale_release_never_reaches_a_newer_capability() {
    let mut state = Bootstrap::init().expect("init");
    let c = chain(&mut state);
    relinquish(&mut state, RECIPIENT, c.leaf).expect("first release");
    assert_eq!(
        relinquish(&mut state, RECIPIENT, c.leaf),
        Err(RelinquishRefusal::Unresolvable),
        "a retry is a no-op"
    );
    // The freed slot is reused by a newer capability; the old id names an older generation.
    let cnode = state.task_cnode(RECIPIENT).expect("cnode");
    let (id, _) = state.alloc_anonymous_memory_object().expect("object");
    let newer = state
        .mint_capability_in_cnode(
            cnode,
            Capability::new(CapObject::MemoryObject { id }, CapRights::READ),
        )
        .expect("newer");
    assert_eq!(newer.index(), c.leaf.index(), "the same slot");
    assert_ne!(newer.generation(), c.leaf.generation());
    assert_eq!(
        relinquish(&mut state, RECIPIENT, c.leaf),
        Err(RelinquishRefusal::Unresolvable)
    );
    assert!(
        state.resolve_capability_for_task(RECIPIENT, newer).is_ok(),
        "the newer capability survives"
    );
}

#[test]
fn each_refusal_mutates_nothing() {
    let mut state = Bootstrap::init().expect("init");
    let c = chain(&mut state);
    let before = (links(&state), refs(&state, c.object));

    // Not a MemoryObject.
    let kernel_cap = state
        .mint_capability_for_current_context(Capability::new(CapObject::Kernel, CapRights::READ))
        .expect("kernel cap");
    assert_eq!(
        relinquish(&mut state, SOURCE, kernel_cap),
        Err(RelinquishRefusal::NotMemoryObject)
    );

    // A live envelope still derives a receiver's capability from the relay's slot.
    let endpoint = CapObject::Endpoint {
        index: 1,
        generation: 1,
    };
    let handle = state
        .stash_transfer_envelope(
            ThreadId(RELAY),
            c.relay,
            endpoint,
            Some(ThreadId(OTHER)),
            None,
        )
        .expect("stash");
    assert_eq!(
        relinquish(&mut state, RELAY, c.relay),
        Err(RelinquishRefusal::EnvelopePending)
    );
    assert_eq!((links(&state), refs(&state, c.object)), before);
    assert!(
        state
            .take_transfer_envelope(handle, endpoint, ThreadId(OTHER))
            .is_some()
    );

    // A registered transfer mapping is NR 4's to release.
    state.with_ipc_state_mut(|ipc| {
        ipc.active_transfer_mappings[0] = Some(super::ActiveTransferMapping {
            owner_tid: ThreadId(RECIPIENT),
            transfer_cap: c.leaf,
            base: crate::kernel::vm::VirtAddr(0x4000_0000),
            len: 4096,
        });
    });
    assert_eq!(
        relinquish(&mut state, RECIPIENT, c.leaf),
        Err(RelinquishRefusal::Mapped)
    );
    state.with_ipc_state_mut(|ipc| ipc.active_transfer_mappings[0] = None);
    assert_eq!((links(&state), refs(&state, c.object)), before);
    assert!(state.resolve_capability_for_task(RECIPIENT, c.leaf).is_ok());

    // An unknown slot.
    assert_eq!(
        relinquish(&mut state, RELAY, CapId(0x7_0000_0001)),
        Err(RelinquishRefusal::Unresolvable)
    );
    assert_eq!((links(&state), refs(&state, c.object)), before);
}

#[test]
fn a_link_table_that_moved_under_the_plan_refuses_whole() {
    let mut state = Bootstrap::init().expect("init");
    let c = chain(&mut state);
    let snapshot = state.with_capability_state(|capability| {
        super::kernel_ref(&capability.delegated_capability_links).to_vec()
    });
    let plan = plan_relinquish_links(&snapshot, RELAY, c.relay, |t| t).expect("plan");
    assert!(plan.parent.is_some() && plan.children.len() == 1);
    // The recipient releases in between, removing the planned child link.
    relinquish(&mut state, RECIPIENT, c.leaf).expect("recipient releases");
    let cnode = state.task_cnode(RELAY).expect("cnode");
    let expected = state
        .resolve_capability_for_task(RELAY, c.relay)
        .expect("relay cap");
    let result = state.with_capability_state_mut(|capability| {
        commit_relinquish_locked(capability, cnode, c.relay, expected, &plan)
    });
    assert_eq!(result, Err(RelinquishRefusal::Moved));
    assert!(
        state.resolve_capability_for_task(RELAY, c.relay).is_ok(),
        "nothing was cleared"
    );
    // A fresh attempt plans again and succeeds.
    assert!(relinquish(&mut state, RELAY, c.relay).is_ok());
}

#[test]
fn the_planner_names_one_parent_and_every_child() {
    let l = |s: u64, sc: u64, d: u64, dc: u64| {
        Some(DelegatedCapabilityLink {
            source_tid: s,
            source_cap: CapId(sc),
            dest_tid: d,
            dest_cap: CapId(dc),
        })
    };
    let table = [
        l(1, 10, 2, 20),
        l(2, 20, 3, 30),
        None,
        l(2, 20, 4, 40),
        l(5, 50, 6, 60),
    ];
    let plan = plan_relinquish_links(&table, 2, CapId(20), |t| t).expect("plan");
    assert_eq!(plan.parent.map(|(i, _)| i), Some(0));
    assert_eq!(
        plan.children
            .iter()
            .map(|(i, _)| *i)
            .collect::<alloc::vec::Vec<_>>(),
        alloc::vec![1, 3]
    );
    // A slot materialized twice is not a shape this kernel produces: refuse rather than guess.
    let doubled = [l(1, 10, 2, 20), l(7, 70, 2, 20)];
    assert_eq!(
        plan_relinquish_links(&doubled, 2, CapId(20), |t| t),
        Err(RelinquishRefusal::Moved)
    );
    // TIDs resolve to processes: a link recorded by thread 9 of process 2 names process 2's slot.
    let threaded = [l(9, 20, 3, 30)];
    let plan = plan_relinquish_links(&threaded, 2, CapId(20), |t| if t == 9 { 2 } else { t })
        .expect("plan");
    assert_eq!(plan.children.len(), 1);
    let _: RelinquishLinkPlan = plan;
}

#[test]
fn the_final_holder_exiting_reclaims_the_object() {
    let mut state = Bootstrap::init().expect("init");
    let c = chain(&mut state);
    relinquish(&mut state, SOURCE, c.root).expect("source");
    relinquish(&mut state, RELAY, c.relay).expect("relay");
    assert_eq!(refs(&state, c.object), Some(1));
    state
        .mark_task_dead(RECIPIENT)
        .expect("the recipient exits holding its grant");
    assert_eq!(
        refs(&state, c.object),
        None,
        "teardown released the last reference"
    );
    assert_eq!(
        state.resolve_capability_for_task(RECIPIENT, c.leaf),
        Err(KernelError::InvalidCapability)
    );
}

/// The production order for a holder that exits holding its grant: the exit (U9-EXIT1) does no
/// general capability revoke, so the exited holder's slot keeps the object; PM's reap (NR 31's
/// `reap_faulted_task_noalloc_cleanup`) tears the process CNode down and reclaims it.
#[test]
fn an_exited_holder_keeps_its_grant_until_the_reap_reclaims_it() {
    let mut state = Bootstrap::init().expect("init");
    let initrd = alloc::boxed::Box::new(Initrd([0u8; 2 * crate::kernel::vm::PAGE_SIZE]));
    let c = slice_chain(&mut state, &initrd);
    relinquish(&mut state, SOURCE, c.root).expect("source");
    relinquish(&mut state, RELAY, c.relay).expect("relay");
    state
        .exit_task(RECIPIENT, 0)
        .expect("the holder exits without releasing");
    assert_eq!(
        refs(&state, c.object),
        Some(1),
        "the exited, unreaped holder still holds it"
    );
    state
        .reap_faulted_task_noalloc_cleanup(RECIPIENT)
        .expect("PM's reap");
    assert_eq!(
        refs(&state, c.object),
        None,
        "the reap's teardown released the last reference"
    );
}

/// The witness ledger counts what the grant path holds from the kernel's own tables: one live slice
/// object with three capability references along source -> relay -> recipient, then none.
#[test]
fn the_ledger_counts_the_grant_and_returns_to_zero() {
    use crate::kernel::boot::grant_ledger::{ledger_from_capability_locked, slice_objects_locked};
    let mut state = Bootstrap::init().expect("init");
    let ledger = |state: &mut KernelState| {
        let objects = state.with_memory_state(slice_objects_locked);
        state.with_capability_state(|capability| ledger_from_capability_locked(capability, objects))
    };
    let before = ledger(&mut state);
    assert_eq!(
        (
            before.live_slice_objects,
            before.slice_cap_refs,
            before.slice_slots
        ),
        (0, 0, 0)
    );
    let initrd = alloc::boxed::Box::new(Initrd([0u8; 2 * crate::kernel::vm::PAGE_SIZE]));
    let c = slice_chain(&mut state, &initrd);
    let held = ledger(&mut state);
    assert_eq!((held.live_slice_objects, held.slice_cap_refs), (1, 3));
    assert!(!held.truncated);
    // A non-slice object is not a grant resource.
    let _ = state.alloc_anonymous_memory_object().expect("anonymous");
    assert_eq!(ledger(&mut state).live_slice_objects, 1);
    relinquish(&mut state, SOURCE, c.root).expect("source");
    relinquish(&mut state, RELAY, c.relay).expect("relay");
    relinquish(&mut state, RECIPIENT, c.leaf).expect("recipient");
    let after = ledger(&mut state);
    assert_eq!(
        (
            after.live_slice_objects,
            after.slice_cap_refs,
            after.slice_slots
        ),
        (0, 0, 0)
    );
    assert_eq!(
        after.links, before.links,
        "every link the chain made is gone"
    );
}

#[test]
fn the_split_adapter_runs_the_same_transaction() {
    let shared = SharedKernel::new(Bootstrap::init().expect("init"));
    let c = shared.with(chain);
    let owners = SplitRelinquishOwners { shared: &shared };
    let out = run_relinquish(&owners, RELAY, c.relay).expect("relay releases off the broad lock");
    assert_eq!(
        (out.reparented, out.parent, out.reclaimed),
        (1, Some((SOURCE, c.root)), false)
    );
    assert!(shared.with(|s| s.resolve_capability_for_task(RECIPIENT, c.leaf).is_ok()));
    assert_eq!(shared.with(|s| refs(s, c.object)), Some(2));
    assert_eq!(
        run_relinquish(&owners, RELAY, c.relay),
        Err(RelinquishRefusal::Unresolvable)
    );
    let out = run_relinquish(&owners, SOURCE, c.root).expect("source");
    assert_eq!((out.reparented, out.reclaimed), (1, false));
    let out = run_relinquish(&owners, RECIPIENT, c.leaf).expect("recipient");
    assert_eq!((out.reparented, out.reclaimed), (0, true));
    assert_eq!(shared.with(|s| refs(s, c.object)), None);
    // Both adapters answer the same refusal for the same input.
    let kernel_cap = shared.with(|s| {
        s.mint_capability_for_current_context(Capability::new(CapObject::Kernel, CapRights::READ))
            .expect("kernel cap")
    });
    assert_eq!(
        run_relinquish(&owners, SOURCE, kernel_cap),
        Err(RelinquishRefusal::NotMemoryObject)
    );
    let _ = owners.pid_of(SOURCE);
}

#[test]
fn nr22_through_the_dispatcher_answers_in_the_frame() {
    use crate::kernel::syscall::{
        CAP_RELINQUISH_STATUS_DONE, SYSCALL_CAP_RELINQUISH_NR, SyscallError,
    };
    use crate::kernel::trapframe::TrapFrame;
    let mut state = Bootstrap::init().expect("init");
    let c = chain(&mut state);
    let mut frame = TrapFrame::new(
        SYSCALL_CAP_RELINQUISH_NR,
        [c.root.0 as usize, 0, 0, 0, 0, 0],
    );
    state
        .handle_trap(crate::kernel::trap::Trap::Syscall, Some(&mut frame))
        .expect("dispatched");
    assert_eq!(frame.error_code(), None);
    assert_eq!(
        (frame.ret0, frame.ret1, frame.ret2),
        (1, 0, CAP_RELINQUISH_STATUS_DONE)
    );
    let mut frame = TrapFrame::new(
        SYSCALL_CAP_RELINQUISH_NR,
        [c.root.0 as usize, 0, 0, 0, 0, 0],
    );
    state
        .handle_trap(crate::kernel::trap::Trap::Syscall, Some(&mut frame))
        .expect("dispatched");
    assert_eq!(
        frame.error_code(),
        Some(SyscallError::InvalidCapability.code())
    );
}
