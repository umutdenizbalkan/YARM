// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;
use crate::kernel::ipc::ThreadId;
use crate::kernel::vm::{CachePolicy, PAGE_SIZE};
use std::{format, string::String, vec::Vec};

#[test]
fn boot_memory_map_reservation_splits_usable_region() {
    let regions = [MemoryRegion {
        start: 0x1000_0000,
        len: 0x20_000,
        usable: true,
    }];
    let reserved = [(0x1000_8000, 0x1000_C000)];
    let (sanitized, len) = Bootstrap::apply_reserved_ranges(&regions, &reserved);
    let usable = &sanitized[..len];
    assert_eq!(usable.len(), 2);
    assert_eq!(usable[0].start, 0x1000_0000);
    assert_eq!(usable[0].len, 0x8000);
    assert_eq!(usable[1].start, 0x1000_C000);
    assert_eq!(usable[1].len, 0x14000);
}

#[test]
fn init_static_with_boot_memory_map_uses_sanitized_ranges() {
    let regions = [MemoryRegion {
        start: 0x1000_0000,
        len: 0x20_000,
        usable: true,
    }];
    let reserved = [(0x1000_0000, 0x1000_1000)];
    let state = Bootstrap::init_static_with_boot_memory_map(
        Bootstrap::default_capacity_profile(),
        &regions,
        &reserved,
    );
    assert!(state.is_ok());
}

#[test]
fn selected_arch_trap_entry_routes_timer() {
    let mut state = Bootstrap::init().expect("init");
    #[cfg(target_arch = "x86_64")]
    let ctx = crate::arch::trap_entry::ArchTrapContext {
        vector: 0x20,
        error_code: 0,
        fault_addr: 0,
    };
    #[cfg(target_arch = "aarch64")]
    let ctx = crate::arch::trap_entry::ArchTrapContext {
        esr_el1: 0,
        far_el1: 0,
        irq_line: None,
        is_timer_irq: true,
    };
    #[cfg(any(
        target_arch = "riscv64",
        not(any(
            target_arch = "riscv64",
            target_arch = "x86_64",
            target_arch = "aarch64"
        ))
    ))]
    let ctx = crate::arch::trap_entry::ArchTrapContext {
        scause: 1usize << (usize::BITS as usize - 1) | 5,
        stval: 0,
    };

    state
        .handle_selected_arch_trap_entry(CpuId(0), ctx, None)
        .expect("trap");
}

#[test]
#[cfg(target_arch = "x86_64")]
fn selected_arch_trap_entry_routes_external_irq_notification() {
    let mut state = Bootstrap::init().expect("init");
    let (_notif_idx, notif_cap, notif_recv_cap) = state.create_notification(4).expect("notif");
    state.bind_irq_notification(1, notif_cap).expect("bind");
    let ctx = crate::arch::trap_entry::ArchTrapContext {
        vector: 0x21, // external IRQ line 1
        error_code: 0,
        fault_addr: 0,
    };

    state
        .handle_selected_arch_trap_entry(CpuId(0), ctx, None)
        .expect("trap");

    let msg = state.ipc_recv(notif_recv_cap).expect("recv").expect("msg");
    assert_eq!(msg.opcode, 1);
    assert_eq!(msg.as_slice()[0], 1);
}

#[test]
#[cfg(target_arch = "x86_64")]
fn selected_arch_trap_entry_external_irq_without_route_is_noop() {
    let mut state = Bootstrap::init().expect("init");
    let (_notif_idx, _notif_cap, notif_recv_cap) = state.create_notification(4).expect("notif");
    let ctx = crate::arch::trap_entry::ArchTrapContext {
        vector: 0x21, // external IRQ line 1
        error_code: 0,
        fault_addr: 0,
    };

    state
        .handle_selected_arch_trap_entry(CpuId(0), ctx, None)
        .expect("trap");

    let msg = state.try_ipc_recv(notif_recv_cap).expect("probe");
    assert!(msg.is_none());
}

#[test]
#[cfg(target_arch = "x86_64")]
fn selected_arch_trap_entry_routes_highest_external_irq_notification() {
    let mut state = Bootstrap::init().expect("init");
    let (_notif_idx, notif_cap, notif_recv_cap) = state.create_notification(4).expect("notif");
    let highest_irq = (crate::arch::platform_constants::MAX_IRQ_LINES - 1) as u16;
    let vector = 0x20 + highest_irq as u8;
    state
        .bind_irq_notification(highest_irq, notif_cap)
        .expect("bind");
    let ctx = crate::arch::trap_entry::ArchTrapContext {
        vector,
        error_code: 0,
        fault_addr: 0,
    };

    state
        .handle_selected_arch_trap_entry(CpuId(0), ctx, None)
        .expect("trap");

    let msg = state.ipc_recv(notif_recv_cap).expect("recv").expect("msg");
    assert_eq!(msg.opcode, highest_irq);
    assert_eq!(msg.as_slice()[0], highest_irq as u8);
}

#[test]
#[cfg(target_arch = "x86_64")]
fn selected_arch_trap_entry_external_limit_vector_is_not_routed_as_irq() {
    let mut state = Bootstrap::init().expect("init");
    let (_notif_idx, notif_cap, notif_recv_cap) = state.create_notification(4).expect("notif");
    let first_unmapped_irq = crate::arch::platform_constants::MAX_IRQ_LINES as u16;
    state
        .bind_irq_notification(first_unmapped_irq, notif_cap)
        .expect("bind");
    let ctx = crate::arch::trap_entry::ArchTrapContext {
        vector: 0x20 + first_unmapped_irq as u8,
        error_code: 0,
        fault_addr: 0,
    };

    state
        .handle_selected_arch_trap_entry(CpuId(0), ctx, None)
        .expect("trap");

    let msg = state.try_ipc_recv(notif_recv_cap).expect("probe");
    assert!(msg.is_none());
}

#[test]
fn bootstrap_sets_minimal_kernel_state() {
    let state = Bootstrap::init().expect("bootstrap should fit static limits");
    assert_eq!(state.kernel_aspace.mappings(), 1);
    assert_eq!(state.online_cpu_count(), 1);
    assert_eq!(state.current_tid().expect("boot task"), 0);
    assert_eq!(state.task_status(0), Some(TaskStatus::Running));
}

#[test]
fn transfer_envelope_handles_are_single_use_and_replay_safe() {
    let mut state = Bootstrap::init().expect("init");
    let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let (_eid, send_cap, _recv_cap) = state.create_endpoint(1).expect("endpoint");
    let endpoint = state
        .current_task_capability(send_cap)
        .expect("send cap")
        .object;

    let first = state
        .stash_transfer_envelope(ThreadId(0), mem_cap, endpoint, None, None)
        .expect("stash first");
    assert!(
        state
            .take_transfer_envelope(first, endpoint, ThreadId(0))
            .is_some()
    );
    assert!(
        state
            .take_transfer_envelope(first, endpoint, ThreadId(0))
            .is_none()
    );

    let second = state
        .stash_transfer_envelope(ThreadId(0), mem_cap, endpoint, None, None)
        .expect("stash second");
    assert_ne!(first, second);
    assert!(
        state
            .take_transfer_envelope(first, endpoint, ThreadId(0))
            .is_none()
    );
    let wrong_endpoint = CapObject::Endpoint {
        index: usize::MAX,
        generation: 1,
    };
    assert!(
        state
            .take_transfer_envelope(second, wrong_endpoint, ThreadId(0))
            .is_none()
    );
    assert!(
        state
            .take_transfer_envelope(second, endpoint, ThreadId(0))
            .is_some()
    );

    let bound = state
        .stash_transfer_envelope(ThreadId(0), mem_cap, endpoint, Some(ThreadId(9)), None)
        .expect("stash bound");
    assert!(
        state
            .take_transfer_envelope(bound, endpoint, ThreadId(8))
            .is_none()
    );
    assert!(
        state
            .take_transfer_envelope(bound, endpoint, ThreadId(9))
            .is_some()
    );
}

#[test]
fn transfer_envelope_shared_region_rejects_zero_len() {
    let mut state = Bootstrap::init().expect("init");
    let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let (_eid, send_cap, _recv_cap) = state.create_endpoint(1).expect("endpoint");
    let endpoint = state
        .current_task_capability(send_cap)
        .expect("send cap")
        .object;

    let handle = state.stash_transfer_envelope(
        ThreadId(0),
        mem_cap,
        endpoint,
        None,
        Some(TransferSharedRegion {
            offset: 0x1000,
            len: 0,
        }),
    );
    assert!(handle.is_none());
    let telemetry = state.ipc_path_telemetry();
    assert_eq!(telemetry.transfer_record_failures, 1);
}

#[test]
fn transfer_envelope_shared_region_rejects_memory_len_overflow() {
    let mut state = Bootstrap::init().expect("init");
    let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let (_eid, send_cap, _recv_cap) = state.create_endpoint(1).expect("endpoint");
    let endpoint = state
        .current_task_capability(send_cap)
        .expect("send cap")
        .object;

    let handle = state.stash_transfer_envelope(
        ThreadId(0),
        mem_cap,
        endpoint,
        None,
        Some(TransferSharedRegion {
            offset: 0x2000,
            len: (PAGE_SIZE as u64) + 1,
        }),
    );
    assert!(handle.is_none());
}

#[test]
fn transfer_state_transition_guard_rejects_invalid_hops() {
    let record = TransferEnvelope {
        source_tid: ThreadId(0),
        source_cap: CapId(1),
        source_object: CapObject::Kernel,
        endpoint: CapObject::Kernel,
        receiver_tid: None,
        state: TransferState::Created,
        shared_region: None,
        generation: 1,
    };
    assert!(record.transition(TransferState::MappedBoth).is_none());
    assert!(record.transition(TransferState::MappedReceiver).is_some());
}

#[test]
fn shared_transfer_pins_memory_object_until_materialized() {
    let mut state = Bootstrap::init().expect("init");
    let (mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let (_eid, send_cap, _recv_cap) = state.create_endpoint(1).expect("endpoint");
    let endpoint = state
        .current_task_capability(send_cap)
        .expect("send cap")
        .object;

    let handle = state
        .stash_transfer_envelope(
            ThreadId(0),
            mem_cap,
            endpoint,
            None,
            Some(TransferSharedRegion {
                offset: 0x2000,
                len: PAGE_SIZE as u64,
            }),
        )
        .expect("stash");
    let slot = state.memory_object_slot_by_id(mem_id).expect("slot");
    let pinned = state.memory.memory_objects[slot].expect("object");
    assert_eq!(pinned.pin_refcount, 1);

    let cnode = state.current_task_cnode().expect("cnode");
    state
        .revoke_capability_in_cnode(cnode, mem_cap)
        .expect("revoke");
    assert!(
        state.memory_object_slot_by_id(mem_id).is_some(),
        "pinned object must remain alive after cap revoke"
    );

    let _ = state
        .take_transfer_envelope(handle, endpoint, ThreadId(0))
        .expect("materialize");
    state.reclaim_memory_object_if_unreferenced(CapObject::MemoryObject { id: mem_id });
    assert!(
        state.memory_object_slot_by_id(mem_id).is_none(),
        "object should reclaim after unpin + no cap/map refs"
    );
}

#[test]
fn process_cleanup_purges_transfer_envelopes_and_unpins_memory() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    let (mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let (_eid, send_cap, _recv_cap) = state.create_endpoint(1).expect("endpoint");
    let endpoint = state
        .current_task_capability(send_cap)
        .expect("send cap")
        .object;

    let handle = state
        .stash_transfer_envelope(
            ThreadId(0),
            mem_cap,
            endpoint,
            Some(ThreadId(1)),
            Some(TransferSharedRegion {
                offset: 0x4000,
                len: PAGE_SIZE as u64,
            }),
        )
        .expect("stash");
    let slot = state.memory_object_slot_by_id(mem_id).expect("slot");
    assert_eq!(
        state.memory.memory_objects[slot]
            .expect("object")
            .pin_refcount,
        1
    );

    state.exit_task(1, 1).expect("exit");
    state.purge_transfer_envelopes_for_pid(1);
    assert!(
        state
            .take_transfer_envelope(handle, endpoint, ThreadId(1))
            .is_none(),
        "cleanup should purge envelope bound to dead process"
    );
    let slot = state
        .memory_object_slot_by_id(mem_id)
        .expect("slot remains");
    assert_eq!(
        state.memory.memory_objects[slot]
            .expect("object")
            .pin_refcount,
        0
    );
}

#[test]
fn process_cleanup_repeated_transfer_envelope_purge_keeps_telemetry_balanced() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    let (_eid, send_cap, _recv_cap) = state.create_endpoint(1).expect("endpoint");
    let endpoint = state
        .current_task_capability(send_cap)
        .expect("send cap")
        .object;

    for i in 0..4 {
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        state
            .stash_transfer_envelope(
                ThreadId(0),
                mem_cap,
                endpoint,
                Some(ThreadId(1)),
                Some(TransferSharedRegion {
                    offset: 0x4000 + (i * PAGE_SIZE) as u64,
                    len: PAGE_SIZE as u64,
                }),
            )
            .expect("stash");
    }

    state.exit_task(1, 1).expect("exit");
    state.purge_transfer_envelopes_for_pid(1);

    let telemetry = state.ipc_path_telemetry();
    assert_eq!(telemetry.transfer_records_created, 4);
    assert_eq!(telemetry.transfer_records_revoked, 4);
}

#[test]
fn process_cleanup_purges_active_transfer_mappings_and_unmaps_pages() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue");
    state.dispatch_next_task().expect("dispatch");
    let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
    state.bind_task_asid(1, asid1).expect("bind1");
    let (mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let mem_cap_task1 = state
        .grant_capability_task_to_task(0, mem_cap, 1)
        .expect("grant mem");

    if state.current_tid() != Some(1) {
        state.yield_current().expect("switch to task1");
    }
    assert_eq!(state.current_tid(), Some(1));
    state
        .map_user_page_in_current_asid_with_caps(
            mem_cap_task1,
            VirtAddr(0x9000),
            PageFlags {
                read: true,
                write: true,
                execute: false,
                user: true,
                cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
            },
        )
        .expect("map");
    state
        .register_active_transfer_mapping(ThreadId(1), mem_cap_task1, VirtAddr(0x9000), PAGE_SIZE)
        .expect("register mapping");
    state.note_shared_mem_mapped(PAGE_SIZE);
    state.exit_task(1, 1).expect("exit");
    assert_eq!(state.current_tid(), Some(0));

    state.purge_active_transfer_mappings_for_pid(1);
    assert!(
        !state.remove_active_transfer_mapping(ThreadId(1), mem_cap_task1),
        "active mapping should be purged during process cleanup"
    );
    let slot = state
        .memory_object_slot_by_id(mem_id)
        .expect("slot remains");
    assert_eq!(
        state.memory.memory_objects[slot]
            .expect("object")
            .map_refcount,
        0
    );
    let telemetry = state.ipc_path_telemetry();
    assert_eq!(telemetry.shared_mem_bytes_mapped, PAGE_SIZE as u64);
    assert_eq!(telemetry.shared_mem_bytes_released, PAGE_SIZE as u64);
}

#[test]
fn revoking_transfer_cap_forces_unmap_of_active_transfer_mapping() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue");
    state.dispatch_next_task().expect("dispatch");
    let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
    state.bind_task_asid(1, asid1).expect("bind1");
    let (mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let mem_cap_task1 = state
        .grant_capability_task_to_task(0, mem_cap, 1)
        .expect("grant mem");

    if state.current_tid() != Some(1) {
        state.yield_current().expect("switch to task1");
    }
    state
        .map_user_page_in_current_asid_with_caps(
            mem_cap_task1,
            VirtAddr(0xA000),
            PageFlags {
                read: true,
                write: true,
                execute: false,
                user: true,
                cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
            },
        )
        .expect("map");
    state
        .register_active_transfer_mapping(ThreadId(1), mem_cap_task1, VirtAddr(0xA000), PAGE_SIZE)
        .expect("register mapping");
    state.note_shared_mem_mapped(PAGE_SIZE);

    state.revoke_capability_direct_in_process_cnode(1, mem_cap_task1);
    assert!(
        !state.remove_active_transfer_mapping(ThreadId(1), mem_cap_task1),
        "revocation should remove active mapping"
    );
    let slot = state.memory_object_slot_by_id(mem_id).expect("slot");
    assert_eq!(
        state.memory.memory_objects[slot]
            .expect("object")
            .map_refcount,
        0
    );
    let telemetry = state.ipc_path_telemetry();
    assert_eq!(telemetry.shared_mem_bytes_mapped, PAGE_SIZE as u64);
    assert_eq!(telemetry.shared_mem_bytes_released, PAGE_SIZE as u64);
}

#[test]
fn repeated_transfer_cap_revoke_force_unmaps_keep_map_release_telemetry_in_sync() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue");
    state.dispatch_next_task().expect("dispatch");
    let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
    state.bind_task_asid(1, asid1).expect("bind1");
    if state.current_tid() != Some(0) {
        state.yield_current().expect("switch to task0");
    }

    for i in 0..3 {
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let mem_cap_task1 = state
            .grant_capability_task_to_task(0, mem_cap, 1)
            .expect("grant mem");
        let base = 0xC000 + (i * PAGE_SIZE);
        state.yield_current().expect("switch to task1");
        state
            .map_user_page_in_current_asid_with_caps(
                mem_cap_task1,
                VirtAddr(base as u64),
                PageFlags {
                    read: true,
                    write: true,
                    execute: false,
                    user: true,
                    cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
                },
            )
            .expect("map");
        state
            .register_active_transfer_mapping(
                ThreadId(1),
                mem_cap_task1,
                VirtAddr(base as u64),
                PAGE_SIZE,
            )
            .expect("register mapping");
        state.note_shared_mem_mapped(PAGE_SIZE);
        state.revoke_capability_direct_in_process_cnode(1, mem_cap_task1);
        state.yield_current().expect("switch back to task0");
    }

    let telemetry = state.ipc_path_telemetry();
    assert_eq!(telemetry.shared_mem_bytes_mapped, (3 * PAGE_SIZE) as u64);
    assert_eq!(telemetry.shared_mem_bytes_released, (3 * PAGE_SIZE) as u64);
}

#[test]
fn phase5_mixed_teardown_paths_keep_transfer_and_mapping_telemetry_balanced() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue");
    state.dispatch_next_task().expect("dispatch");
    let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
    state.bind_task_asid(1, asid1).expect("bind1");
    let (_eid, send_cap, _recv_cap) = state.create_endpoint(4).expect("endpoint");
    let endpoint = state
        .current_task_capability(send_cap)
        .expect("send cap")
        .object;

    if state.current_tid() != Some(0) {
        state.yield_current().expect("switch to task0");
    }

    for i in 0..3 {
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        state
            .stash_transfer_envelope(
                ThreadId(0),
                mem_cap,
                endpoint,
                Some(ThreadId(1)),
                Some(TransferSharedRegion {
                    offset: 0x5000 + (i * PAGE_SIZE) as u64,
                    len: PAGE_SIZE as u64,
                }),
            )
            .expect("stash");

        let mem_cap_task1 = state
            .grant_capability_task_to_task(0, mem_cap, 1)
            .expect("grant mem");
        let base = 0xF000 + (i * PAGE_SIZE);
        state.yield_current().expect("switch to task1");
        state
            .map_user_page_in_current_asid_with_caps(
                mem_cap_task1,
                VirtAddr(base as u64),
                PageFlags {
                    read: true,
                    write: true,
                    execute: false,
                    user: true,
                    cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
                },
            )
            .expect("map");
        state
            .register_active_transfer_mapping(
                ThreadId(1),
                mem_cap_task1,
                VirtAddr(base as u64),
                PAGE_SIZE,
            )
            .expect("register mapping");
        state.note_shared_mem_mapped(PAGE_SIZE);
        state.revoke_capability_direct_in_process_cnode(1, mem_cap_task1);
        state.yield_current().expect("switch to task0");
    }

    state.exit_task(1, 1).expect("exit");
    state.purge_transfer_envelopes_for_pid(1);
    state.purge_active_transfer_mappings_for_pid(1);

    let telemetry = state.ipc_path_telemetry();
    assert_eq!(telemetry.transfer_records_created, 3);
    assert_eq!(telemetry.transfer_records_revoked, 6);
    assert_eq!(
        telemetry.shared_mem_bytes_mapped,
        telemetry.shared_mem_bytes_released
    );
}

#[test]
fn spawn_user_task_from_image_registers_asid_and_class() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _map_cap) = state.create_user_address_space().expect("asid");
    let spawned = state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 55,
            entry: 0x8000,
            asid: Some(asid),
            class: TaskClass::SystemServer,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("spawn");
    assert_eq!(spawned.tid, 55);
    assert_eq!(spawned.entry, 0x8000);
    assert_eq!(spawned.asid, Some(asid));
    assert_eq!(state.task_class(55), Some(TaskClass::SystemServer));
    let tcb = state.tcb_mut(55).expect("tcb");
    assert_eq!(tcb.asid, Some(asid));
    let stack_top = tcb.user_stack_top.expect("stack top");
    assert_ne!(stack_top.0, 0, "user stack top must be non-zero");
    assert_eq!(tcb.user_context.instruction_ptr, VirtAddr(0x8000));
    assert_eq!(tcb.user_context.stack_ptr, stack_top);
    let stack_base = VirtAddr(stack_top.0 - 64 * crate::kernel::vm::PAGE_SIZE as u64);
    let guard = VirtAddr(stack_base.0 - crate::kernel::vm::PAGE_SIZE as u64);
    let aspace = state.user_spaces.get(asid).expect("aspace");
    assert!(
        aspace.resolve(stack_base).is_some(),
        "stack page should be mapped"
    );
    let guard_mapping = aspace.resolve(guard).expect("guard page should be mapped");
    assert_eq!(
        guard_mapping.flags,
        crate::kernel::vm::PageFlags::GUARD,
        "guard page below stack must be mapped as no-access guard"
    );
}

#[test]
fn spawn_user_task_from_image_copies_startup_args_into_user_context() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    let mut startup_args = UserImageSpec::DEFAULT_STARTUP_ARGS;
    startup_args[0] = 77;
    startup_args[1] = 0x1234;
    startup_args[2] = 0x5678;
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 77,
            entry: 0x8000,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args,
            ..Default::default()
        })
        .expect("spawn");
    let ctx = state.thread_user_context(77).expect("ctx");
    assert_eq!(ctx.arg0, 77);
    assert_eq!(ctx.arg1, 0x1234);
    assert_eq!(ctx.arg2, 0x5678);
}

#[test]
fn delegated_endpoint_caps_are_init_local_and_resolvable() {
    let mut state = Bootstrap::init().expect("init");
    state
        .register_task_with_class(1, TaskClass::SystemServer)
        .expect("register init");
    let (_eid, send_root, recv_root) = state.create_endpoint(4).expect("endpoint");
    let send_init = state
        .grant_capability_task_to_task_with_rights(0, send_root, 1, CapRights::SEND)
        .expect("grant send");
    let recv_init = state
        .grant_capability_task_to_task_with_rights(0, recv_root, 1, CapRights::RECEIVE)
        .expect("grant recv");
    assert_ne!(send_init, send_root);
    assert_ne!(recv_init, recv_root);
    let init_cnode = state.task_cnode(1).expect("init cnode");
    let send_cap = state
        .capability_for_cnode(init_cnode, send_init)
        .expect("init send cap");
    let recv_cap = state
        .capability_for_cnode(init_cnode, recv_init)
        .expect("init recv cap");
    assert!(send_cap.has_right(CapRights::SEND));
    assert!(recv_cap.has_right(CapRights::RECEIVE));
}

#[test]
fn capability_snapshot_for_task_returns_empty_for_fresh_cnode() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(90).expect("task");
    let caps = state
        .snapshot_live_capabilities_for_task(90)
        .expect("snapshot");
    assert!(caps.is_empty());
}

#[test]
fn capability_snapshot_for_task_includes_live_endpoint_caps() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(91).expect("task");
    let (_eid, send_root, recv_root) = state.create_endpoint(2).expect("endpoint");
    let send_child = state
        .grant_capability_task_to_task_with_rights(0, send_root, 91, CapRights::SEND)
        .expect("grant send");
    let recv_child = state
        .grant_capability_task_to_task_with_rights(0, recv_root, 91, CapRights::RECEIVE)
        .expect("grant recv");

    let caps = state
        .snapshot_live_capabilities_for_task(91)
        .expect("snapshot");
    assert!(caps.iter().any(|(id, cap)| {
        *id == send_child && matches!(cap.object, CapObject::Endpoint { .. }) && cap.has_right(CapRights::SEND)
    }));
    assert!(caps.iter().any(|(id, cap)| {
        *id == recv_child && matches!(cap.object, CapObject::Endpoint { .. }) && cap.has_right(CapRights::RECEIVE)
    }));
}

#[test]
fn capability_snapshot_for_task_skips_stale_endpoint_caps() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(92).expect("task");
    let (eid, _send_root, recv_root) = state.create_endpoint(2).expect("endpoint");
    let recv_child = state
        .grant_capability_task_to_task_with_rights(0, recv_root, 92, CapRights::RECEIVE)
        .expect("grant recv");
    state.destroy_endpoint(eid).expect("destroy");

    let caps = state
        .snapshot_live_capabilities_for_task(92)
        .expect("snapshot");
    assert!(!caps.iter().any(|(id, _)| *id == recv_child));
}

#[test]
fn supervisor_fault_slot_cap_can_register_supervisor_endpoint() {
    let mut state = Bootstrap::init().expect("init");
    state
        .register_task_with_class(1, TaskClass::SystemServer)
        .expect("register init");
    let (_eid, _send_root, recv_root) = state.create_endpoint(4).expect("endpoint");
    let recv_init = state
        .grant_capability_task_to_task_with_rights(0, recv_root, 1, CapRights::RECEIVE)
        .expect("grant recv");
    state
        .set_supervisor_endpoint_for_task(1, recv_init)
        .expect("set supervisor endpoint");
    state
        .report_task_exit_to_supervisor(1, 7, 9)
        .expect("report");
    let msg = state.ipc_recv(recv_root).expect("recv").expect("msg");
    assert_eq!(msg.opcode, yarm_ipc_abi::supervisor_abi::SUPERVISOR_OP_TASK_EXITED);
}

#[test]
fn spawn_user_task_from_image_requires_valid_asid() {
    let mut state = Bootstrap::init().expect("init");
    let err = state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 77,
            entry: 0x9000,
            asid: None,
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect_err("missing asid should fail");
    assert_eq!(err, KernelError::UserMemoryFault);
}

#[test]
fn can_bring_up_secondary_cpu_and_schedule_on_it() {
    let mut state = Bootstrap::init().expect("init");
    assert!(state.bring_up_cpu(CpuId(1)).is_ok());
    assert_eq!(state.online_cpu_count(), 2);

    state.register_task(42).expect("task42");
    state.enqueue_on_cpu(CpuId(1), 42).expect("enqueue cpu1");

    state.set_current_cpu(CpuId(1)).expect("switch cpu1");
    assert_eq!(state.dispatch_next_current_cpu(), Some(42));
    assert_eq!(state.current_tid(), Some(42));
    assert_eq!(state.task_status(42), Some(TaskStatus::Runnable));
}

#[test]
fn cross_cpu_work_queue_round_trip() {
    let mut state = Bootstrap::init().expect("init");
    state.bring_up_cpu(CpuId(1)).expect("cpu1");
    state
        .submit_cross_cpu_work(CpuId(1), WorkItem::Reschedule)
        .expect("submit");

    state.set_current_cpu(CpuId(1)).expect("switch cpu1");

    assert_eq!(
        state.drain_cross_cpu_work().expect("drain"),
        Some(WorkItem::Reschedule)
    );
    assert_eq!(state.drain_cross_cpu_work().expect("drain"), None);
}

#[test]
fn destroy_user_address_space_queues_shootdowns_and_retires_asid() {
    let mut state = Bootstrap::init().expect("init");
    state.bring_up_cpu(CpuId(1)).expect("cpu1");

    let (asid, aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .destroy_user_address_space(aspace_cap)
        .expect("destroy aspace");

    assert!(state.user_spaces.get(asid).is_none());
    assert_eq!(
        state
            .user_spaces
            .retired_entry(asid)
            .map(|entry| entry.pending_cpu_bitmap),
        Some(0b11)
    );

    let mut seen = [false; 2];
    for cpu in [CpuId(0), CpuId(1)] {
        state.set_current_cpu(cpu).expect("switch cpu");
        if let Some(WorkItem::TlbShootdown {
            asid: item_asid,
            va_range,
            ..
        }) = state.drain_cross_cpu_work().expect("drain")
        {
            assert_eq!(item_asid, asid);
            assert_eq!(va_range, None);
            seen[cpu.0 as usize] = true;
        }
    }
    assert_eq!(seen, [true, true]);

    state
        .submit_cross_cpu_work(
            CpuId(0),
            WorkItem::TlbShootdown {
                asid,
                va_range: None,
                requester: None,
                sequence: 0,
            },
        )
        .expect("requeue cpu0 shootdown");
    state
        .submit_cross_cpu_work(
            CpuId(1),
            WorkItem::TlbShootdown {
                asid,
                va_range: None,
                requester: None,
                sequence: 0,
            },
        )
        .expect("requeue cpu1 shootdown");

    state.set_current_cpu(CpuId(0)).expect("switch cpu0");
    state
        .process_cross_cpu_work_for_cpu(CpuId(0))
        .expect("process cpu0");
    assert_eq!(
        crate::arch::selected_isa::page_table::take_last_invalidated_asid_for_test(),
        Some(asid)
    );
    assert_eq!(
        state
            .user_spaces
            .retired_entry(asid)
            .map(|entry| entry.pending_cpu_bitmap),
        Some(0b10)
    );

    state.set_current_cpu(CpuId(1)).expect("switch cpu1");
    state
        .process_cross_cpu_work_for_cpu(CpuId(1))
        .expect("process cpu1");
    assert_eq!(
        crate::arch::selected_isa::page_table::take_last_invalidated_asid_for_test(),
        Some(asid)
    );
    assert_eq!(state.user_spaces.retired_entry(asid), None);
}

#[test]
fn destroy_aspace_with_blocked_ipc_waiter_and_preemption_preserves_ordering() {
    let mut state = Bootstrap::init().expect("init");
    state.bring_up_cpu(CpuId(1)).expect("cpu1");
    state.register_task(1).expect("task1");
    state.register_task(2).expect("task2");
    state.enqueue_current_cpu(1).expect("enqueue1");
    state.enqueue_current_cpu(2).expect("enqueue2");

    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(1, asid).expect("bind asid to task1");

    let (_eid, send_cap, recv_cap) = state
        .create_endpoint_with_mode(1, EndpointMode::Synchronous)
        .expect("endpoint");
    let send_cap_task1 = state
        .grant_capability_task_to_task(0, send_cap, 1)
        .expect("dup send cap");
    let recv_cap_task2 = state
        .grant_capability_task_to_task(0, recv_cap, 2)
        .expect("dup recv cap");

    while state.current_tid() != Some(1) {
        state.yield_current().expect("switch to task1");
    }
    assert_eq!(state.current_tid(), Some(1));
    assert_eq!(
        state.ipc_send(send_cap_task1, Message::new(1, b"hold").expect("msg")),
        Err(KernelError::WouldBlock)
    );
    assert_eq!(
        state.task_status(1),
        Some(TaskStatus::Blocked(WaitReason::EndpointSend(
            send_cap_task1
        )))
    );
    if state.current_tid() != Some(2) {
        state.yield_current().expect("switch to task2");
    }
    assert_eq!(state.current_tid(), Some(2));

    state
        .destroy_user_address_space_by_asid(asid)
        .expect("destroy aspace");
    assert!(state.user_spaces.get(asid).is_none());
    assert_eq!(
        state
            .user_spaces
            .retired_entry(asid)
            .map(|entry| entry.pending_cpu_bitmap),
        Some(0b11)
    );

    assert!(state.on_preempt_current_cpu().is_some());
    if state.current_tid() != Some(2) {
        state.yield_current().expect("reschedule task2");
    }
    assert_eq!(state.current_tid(), Some(2));

    let delivered = state
        .ipc_recv(recv_cap_task2)
        .expect("recv")
        .expect("message");
    assert_eq!(delivered.as_slice(), b"hold");
    assert_eq!(state.task_status(1), Some(TaskStatus::Runnable));
}

#[test]
fn process_cross_cpu_work_applies_matching_cpu_items_only() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(2).expect("task2");
    state.bring_up_cpu(CpuId(1)).expect("cpu1");

    state
        .submit_cross_cpu_work(CpuId(1), WorkItem::WakeTask { tid: ThreadId(2) })
        .expect("submit wake");
    state
        .submit_cross_cpu_work(
            CpuId(0),
            WorkItem::TlbShootdown {
                asid: Asid(1),
                va_range: None,
                requester: None,
                sequence: 0,
            },
        )
        .expect("submit tlb");

    let done = state
        .process_cross_cpu_work_for_cpu(CpuId(0))
        .expect("process cpu0");
    assert_eq!(done, 1);
    assert_eq!(state.tlb_shootdown_count(), 1);

    // WakeTask for cpu1 should still be queued.
    state.set_current_cpu(CpuId(1)).expect("switch cpu1");
    assert_eq!(
        state.drain_cross_cpu_work().expect("drain cpu1"),
        Some(WorkItem::WakeTask { tid: ThreadId(2) })
    );
}

#[test]
fn retired_asid_without_ack_stays_retired_and_does_not_timeout_escalate() {
    let mut state = Bootstrap::init().expect("init");
    state.bring_up_cpu(CpuId(1)).expect("cpu1");
    let (_eid, _send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
    state
        .set_supervisor_endpoint(recv_cap)
        .expect("supervisor endpoint");

    let (asid, aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .destroy_user_address_space(aspace_cap)
        .expect("destroy aspace");
    assert!(state.user_spaces.retired_entry(asid).is_some());

    // Drop queued shootdown work without processing ACKs; retired ASIDs now
    // require explicit acknowledgements and must not auto-timeout.
    state.set_current_cpu(CpuId(0)).expect("cpu0");
    let _ = state.drain_cross_cpu_work().expect("drain cpu0");
    state.set_current_cpu(CpuId(1)).expect("cpu1");
    let _ = state.drain_cross_cpu_work().expect("drain cpu1");

    state.set_current_cpu(CpuId(0)).expect("cpu0");
    for _ in 0..16 {
        let _ = state
            .process_cross_cpu_work_for_cpu(CpuId(0))
            .expect("tick timeout");
    }

    assert!(state.user_spaces.retired_entry(asid).is_some());
    assert_eq!(state.tlb_shootdown_timeout_count(), 0);
    assert_eq!(state.try_ipc_recv(recv_cap).expect("recv"), None);
}

#[test]
fn capability_checked_ipc_round_trip() {
    let mut state = Bootstrap::init().expect("init");
    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
    let msg = Message::new(7, b"ping").expect("message");

    state.ipc_send(send_cap, msg).expect("send should pass");
    let received = state
        .ipc_recv(recv_cap)
        .expect("recv should pass")
        .expect("message expected");

    assert_eq!(received.sender_tid.0, 7);
    assert_eq!(received.as_slice(), b"ping");
}

#[test]
fn timer_trap_preempts_and_rotates() {
    let mut state = Bootstrap::init().expect("init");
    state.set_timer_for_test(Timer::new(1));
    state.register_task(1).expect("register task 1");
    state.enqueue_current_cpu(1).expect("queue task 1");

    let running_before = state.current_tid().expect("running");
    state
        .handle_trap(Trap::TimerInterrupt, None)
        .expect("timer trap should be handled");
    let running_after = state.current_tid().expect("running");

    assert_ne!(running_before, running_after);
    assert_eq!(state.task_status(running_after), Some(TaskStatus::Running));
}

#[test]
fn ipc_recv_deadline_timeout_wakes_blocked_waiter_on_timer_tick() {
    let mut state = Bootstrap::init().expect("init");
    state.set_timer_for_test(Timer::new(1));
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue");
    state.dispatch_next_task().expect("dispatch to task1");
    let blocked_tid = state.current_tid().expect("running tid");

    let (_eid, _send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
    let first = state
        .ipc_recv_with_deadline(recv_cap, 1)
        .expect("deadline recv should not fail");
    assert_eq!(first, None);
    assert_eq!(
        state.task_status(blocked_tid),
        Some(TaskStatus::Blocked(WaitReason::EndpointReceive(recv_cap)))
    );

    state
        .handle_trap(Trap::TimerInterrupt, None)
        .expect("timer trap");

    assert!(matches!(
        state.task_status(blocked_tid),
        Some(TaskStatus::Runnable | TaskStatus::Running)
    ));
    assert!(
        state
            .consume_ipc_timeout_fired_for_tid(blocked_tid)
            .expect("consume timeout marker"),
        "timeout marker should be set when deadline wake fires"
    );
}

#[test]
fn ipc_send_deadline_timeout_wakes_blocked_sender_on_timer_tick() {
    let mut state = Bootstrap::init().expect("init");
    state.set_timer_for_test(Timer::new(1));
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue");
    state.dispatch_next_task().expect("dispatch to task1");
    let blocked_tid = state.current_tid().expect("running tid");

    let (_eid, send_cap, _recv_cap) = state
        .create_endpoint_with_mode(1, EndpointMode::Synchronous)
        .expect("endpoint");
    let msg = Message::new(1, b"x").expect("msg");
    let send_result = state.ipc_send_with_deadline(send_cap, msg, 1);
    assert_eq!(send_result, Err(KernelError::WouldBlock));
    assert_eq!(
        state.task_status(blocked_tid),
        Some(TaskStatus::Blocked(WaitReason::EndpointSend(send_cap)))
    );

    state
        .handle_trap(Trap::TimerInterrupt, None)
        .expect("timer trap");

    assert!(matches!(
        state.task_status(blocked_tid),
        Some(TaskStatus::Runnable | TaskStatus::Running)
    ));
    assert!(
        state
            .consume_ipc_timeout_fired_for_tid(blocked_tid)
            .expect("consume timeout marker"),
        "timeout marker should be set when send wait times out"
    );
}

#[test]
fn reply_cap_record_is_single_use_and_routes_reply_to_bound_endpoint() {
    let mut state = Bootstrap::init().expect("init");
    let (_eid, _send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
    let reply_cap = state
        .create_reply_cap_for_caller(ThreadId(0), recv_cap, None)
        .expect("create reply cap");

    let reply = Message::new(9, b"ok").expect("reply");
    state.ipc_reply(reply_cap, reply).expect("reply send");
    let received = state
        .ipc_recv(recv_cap)
        .expect("recv")
        .expect("message expected");
    assert_eq!(received.sender_tid.0, 9);
    assert_eq!(received.as_slice(), b"ok");

    let replay = Message::new(9, b"no").expect("replay");
    assert_eq!(
        state.ipc_reply(reply_cap, replay),
        Err(KernelError::StaleCapability)
    );
}

#[test]
fn recv_v2_blocked_waiter_direct_delivery_consumes_exactly_once() {
    std::thread::Builder::new()
        .name("recv_v2_blocked_waiter_direct_delivery_consumes_exactly_once".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(run_recv_v2_blocked_waiter_direct_delivery_consumes_exactly_once)
        .expect("spawn test thread")
        .join()
        .expect("join test thread");
}

fn run_recv_v2_blocked_waiter_direct_delivery_consumes_exactly_once() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.register_task(2).expect("task2");
    let (asid1, aspace_map_cap1) = state.create_user_address_space().expect("asid1");
    state.bind_task_asid(1, asid1).expect("bind task1 asid");
    state
        .map_user_page(
            aspace_map_cap1,
            VirtAddr(0x2000),
            Mapping {
                phys: PhysAddr(0x6000),
                flags: PageFlags::USER_RW,
            },
        )
        .expect("map task1 recv buffers");
    let (_eid, send_cap, recv_cap_global) = state.create_endpoint(4).expect("endpoint");
    let recv_cap = state
        .grant_capability_task_to_task(0, recv_cap_global, 1)
        .expect("dup recv cap");
    let send_cap_task2 = state
        .grant_capability_task_to_task(0, send_cap, 2)
        .expect("dup send cap");
    state.enqueue_current_cpu(1).expect("enqueue");
    state.enqueue_current_cpu(2).expect("enqueue");
    state.dispatch_next_task().expect("dispatch");

    while state.current_tid() != Some(1) {
        state.yield_current().expect("switch to task1");
    }
    let payload_ptr = 0x2000usize;
    let meta_ptr = 0x2080usize;
    state
        .copy_to_user(asid1, VirtAddr(payload_ptr as u64), b"pre")
        .expect("pre copy_to_user");
    let pre = state
        .read_user_memory_for_asid(asid1, payload_ptr, 3)
        .expect("pre copy_from_user");
    assert_eq!(&pre[..3], b"pre");
    let mut recv_frame = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcRecv as usize,
        [
            recv_cap.0 as usize,
            payload_ptr,
            16,
            meta_ptr,
            40,
            0,
        ],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut recv_frame))
        .expect("recv blocks");
    assert_ne!(state.current_tid(), Some(1));

    if state.current_tid() != Some(2) {
        state.yield_current().expect("switch sender");
    }
    let msg = Message::with_header(7, 0x1234, 0, None, b"hello").expect("msg");
    state.ipc_send(send_cap_task2, msg).expect("send");
    state.yield_current().expect("switch receiver");
    assert_eq!(state.current_tid(), Some(1));
    let payload = state
        .read_user_memory_for_asid(asid1, payload_ptr, 5)
        .expect("read payload");
    let meta = state
        .read_user_memory_for_asid(asid1, meta_ptr, 40)
        .expect("read meta");
    assert_eq!(payload[..5], *b"hello");
    assert_eq!(
        u16::from_le_bytes(meta[10..12].try_into().expect("msg flags")),
        0
    );
    assert_eq!(u16::from_le_bytes(meta[8..10].try_into().expect("opcode")), 0x1234);
    assert_eq!(u64::from_le_bytes(meta[16..24].try_into().expect("cap")), u64::MAX);
    assert_eq!(u64::from_le_bytes(meta[32..40].try_into().expect("sender")), 7);

    state.yield_current().expect("switch sender");
    state.yield_current().expect("switch receiver");
    assert_eq!(state.ipc_recv(recv_cap).expect("recv queued"), None);
}

#[test]
fn user_memory_copy_to_then_copy_from_same_asid_roundtrips() {
    std::thread::Builder::new()
        .name("user_memory_copy_to_then_copy_from_same_asid_roundtrips".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(run_user_memory_copy_to_then_copy_from_same_asid_roundtrips)
        .expect("spawn test thread")
        .join()
        .expect("join test thread");
}

fn run_user_memory_copy_to_then_copy_from_same_asid_roundtrips() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(0, asid).expect("bind");
    state
        .map_user_page(
            aspace_map_cap,
            VirtAddr(0x2000),
            Mapping {
                phys: PhysAddr(0x8000),
                flags: PageFlags::USER_RW,
            },
        )
        .expect("map rw");
    state
        .copy_to_user(asid, VirtAddr(0x2000), b"abcd")
        .expect("copy_to_user");
    let out = state
        .read_user_memory_for_asid(asid, 0x2000, 4)
        .expect("copy_from_user");
    assert_eq!(&out[..4], b"abcd");
}

#[test]
fn ipc_reply_wakes_blocked_recv_v2_waiter_without_duplicate_enqueue() {
    std::thread::Builder::new()
        .name("ipc_reply_wakes_blocked_recv_v2_waiter_without_duplicate_enqueue".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(run_ipc_reply_wakes_blocked_recv_v2_waiter_without_duplicate_enqueue)
        .expect("spawn test thread")
        .join()
        .expect("join test thread");
}

fn run_ipc_reply_wakes_blocked_recv_v2_waiter_without_duplicate_enqueue() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.register_task(2).expect("task2");
    let (asid1, aspace_map_cap1) = state.create_user_address_space().expect("asid1");
    state.bind_task_asid(1, asid1).expect("bind task1 asid");
    state
        .map_user_page(
            aspace_map_cap1,
            VirtAddr(0x3000),
            Mapping {
                phys: PhysAddr(0x7000),
                flags: PageFlags::USER_RW,
            },
        )
        .expect("map task1 reply recv buffers");
    let (_eid, _send_cap, recv_cap_global) = state.create_endpoint(4).expect("endpoint");
    let recv_cap = state
        .grant_capability_task_to_task(0, recv_cap_global, 1)
        .expect("dup recv cap");
    state.enqueue_current_cpu(2).expect("enqueue");
    state.enqueue_current_cpu(1).expect("enqueue");
    state.dispatch_next_task().expect("dispatch");
    while state.current_tid() != Some(0) {
        state.yield_current().expect("switch to root");
    }
    let reply_cap = state
        .create_reply_cap_for_caller(ThreadId(1), recv_cap_global, Some(ThreadId(2)))
        .expect("reply cap");
    let reply_cap_task2 = state
        .grant_capability_task_to_task(0, reply_cap, 2)
        .expect("dup reply cap");

    while state.current_tid() != Some(1) {
        state.yield_current().expect("switch to task1");
    }
    let payload_ptr = 0x3000usize;
    let meta_ptr = 0x3080usize;
    let mut recv_frame = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcRecv as usize,
        [
            recv_cap.0 as usize,
            payload_ptr,
            16,
            meta_ptr,
            40,
            0,
        ],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut recv_frame))
        .expect("recv blocks");
    while state.current_tid() != Some(2) {
        state.yield_current().expect("switch replier");
    }
    let reply = Message::with_header(0, 0x44, 0, None, b"rp").expect("reply");
    state.ipc_reply(reply_cap_task2, reply).expect("reply");
    while state.current_tid() != Some(1) {
        state.yield_current().expect("switch receiver");
    }
    let payload = state
        .read_user_memory_for_asid(asid1, payload_ptr, 2)
        .expect("read payload");
    let mut meta = [0u8; 40];
    state
        .copy_from_current_user_into_slice(meta_ptr, meta.len(), &mut meta)
        .expect("read meta");
    assert_eq!(payload[..2], *b"rp");
    assert_eq!(u16::from_le_bytes(meta[8..10].try_into().expect("opcode")), 0x44);

    state.yield_current().expect("switch sender");
    state.yield_current().expect("switch receiver");
    assert_eq!(state.ipc_recv(recv_cap).expect("recv queued"), None);
}

#[test]
fn reply_caps_are_revoked_when_caller_exits() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue");
    state.dispatch_next_task().expect("dispatch");
    let (_eid, _send_cap, recv_cap_global) = state.create_endpoint(4).expect("endpoint");
    let recv_cap = state
        .grant_capability_task_to_task(0, recv_cap_global, 1)
        .expect("dup recv cap");

    let reply_cap = state
        .create_reply_cap_for_caller(ThreadId(1), recv_cap, None)
        .expect("create reply cap");

    state.exit_task(1, 7).expect("exit caller");

    let reply = Message::new(9, b"late").expect("reply");
    assert_eq!(
        state.ipc_reply(reply_cap, reply),
        Err(KernelError::StaleCapability)
    );
}

#[test]
fn reply_caps_are_revoked_when_caller_marked_dead() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue");
    state.dispatch_next_task().expect("dispatch");
    let (_eid, _send_cap, recv_cap_global) = state.create_endpoint(4).expect("endpoint");
    let recv_cap = state
        .grant_capability_task_to_task(0, recv_cap_global, 1)
        .expect("dup recv cap");
    let reply_cap = state
        .create_reply_cap_for_caller(ThreadId(1), recv_cap, None)
        .expect("create reply cap");

    state.mark_task_dead(1).expect("mark dead");

    let reply = Message::new(9, b"late").expect("reply");
    assert_eq!(
        state.ipc_reply(reply_cap, reply),
        Err(KernelError::StaleCapability)
    );
}

#[test]
fn reply_cap_rejects_use_from_unbound_responder_task() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.register_task(2).expect("task2");
    state.enqueue_current_cpu(1).expect("enqueue1");
    state.enqueue_current_cpu(2).expect("enqueue2");
    state.dispatch_next_task().expect("dispatch");
    let (_eid, _send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
    let reply_cap = state
        .create_reply_cap_for_caller(ThreadId(0), recv_cap, Some(ThreadId(1)))
        .expect("create reply cap");
    let reply_cap_task2 = state
        .grant_capability_task_to_task(0, reply_cap, 2)
        .expect("dup reply cap");

    while state.current_tid() != Some(2) {
        state.yield_current().expect("switch");
    }
    let msg = Message::new(2, b"bad").expect("reply");
    assert_eq!(
        state.ipc_reply(reply_cap_task2, msg),
        Err(KernelError::MissingRight)
    );
}

#[test]
fn reply_caps_are_revoked_when_caller_restarts() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    let (_eid, _send_cap, recv_cap_global) = state.create_endpoint(4).expect("endpoint");
    let recv_cap = state
        .grant_capability_task_to_task(0, recv_cap_global, 1)
        .expect("dup recv cap");
    let reply_cap = state
        .create_reply_cap_for_caller(ThreadId(1), recv_cap, None)
        .expect("create reply cap");

    let token = state.exit_task(1, 11).expect("exit");
    state.restart_task(1, token).expect("restart");

    let reply = Message::new(9, b"late").expect("reply");
    assert_eq!(
        state.ipc_reply(reply_cap, reply),
        Err(KernelError::StaleCapability)
    );
}

#[test]
fn old_reply_cap_replay_is_rejected_after_restart_and_remint() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    let (_eid, _send_cap, recv_cap_global) = state.create_endpoint(4).expect("endpoint");
    let recv_cap = state
        .grant_capability_task_to_task(0, recv_cap_global, 1)
        .expect("dup recv cap");
    let old_reply_cap = state
        .create_reply_cap_for_caller(ThreadId(1), recv_cap, None)
        .expect("create old reply cap");

    let token = state.exit_task(1, 12).expect("exit");
    state.restart_task(1, token).expect("restart");
    let recv_cap_after_restart = state
        .grant_capability_task_to_task(0, recv_cap_global, 1)
        .expect("dup recv cap after restart");
    let new_reply_cap = state
        .create_reply_cap_for_caller(ThreadId(1), recv_cap_after_restart, None)
        .expect("create new reply cap");

    let replay = Message::new(9, b"stale").expect("stale reply");
    assert_eq!(
        state.ipc_reply(old_reply_cap, replay),
        Err(KernelError::StaleCapability)
    );

    let fresh = Message::new(9, b"fresh").expect("fresh reply");
    state
        .ipc_reply(new_reply_cap, fresh)
        .expect("fresh reply send");
}

#[test]
fn duplicated_stale_reply_cap_is_rejected_after_caller_restart() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.register_task(2).expect("task2");
    let (_eid, _send_cap, recv_cap_global) = state.create_endpoint(4).expect("endpoint");
    let recv_cap = state
        .grant_capability_task_to_task(0, recv_cap_global, 1)
        .expect("dup recv cap");
    let reply_cap = state
        .create_reply_cap_for_caller(ThreadId(1), recv_cap, Some(ThreadId(2)))
        .expect("create reply cap");
    let reply_cap_task2 = state
        .grant_capability_task_to_task(0, reply_cap, 2)
        .expect("dup reply cap to task2");

    let token = state.exit_task(1, 13).expect("exit");
    state.restart_task(1, token).expect("restart");

    state.enqueue_current_cpu(2).expect("enqueue2");
    state.dispatch_next_task().expect("dispatch2");
    let replay = Message::new(2, b"stale").expect("stale reply");
    assert!(
        matches!(
            state.ipc_reply(reply_cap_task2, replay),
            Err(KernelError::StaleCapability | KernelError::WrongObject)
        ),
        "duplicated stale reply-cap should be rejected after caller restart"
    );
}

#[test]
fn normalized_page_fault_event_faults_current_task() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue task1");

    state
        .handle_trap_event(
            TrapEvent::PageFault(FaultInfo {
                addr: VirtAddr(0x1200),
                access: super::super::trap::FaultAccess::Read,
            }),
            None,
        )
        .expect("page fault event handled");

    assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
    assert_eq!(state.current_tid(), Some(1));
    assert_eq!(
        state.last_fault(),
        Some(FaultInfo {
            addr: VirtAddr(0x1200),
            access: super::super::trap::FaultAccess::Read,
        })
    );
}

#[test]
fn recv_on_empty_endpoint_blocks_then_send_wakes() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("register task 1");
    state.enqueue_current_cpu(1).expect("queue task 1");
    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
    let send_cap_task1 = state
        .grant_capability_task_to_task(0, send_cap, 1)
        .expect("dup send cap to task1");

    assert_eq!(state.current_tid(), Some(0));
    let first_try = state.ipc_recv(recv_cap).expect("recv call should not fail");
    assert!(first_try.is_none());
    assert_eq!(
        state.task_status(0),
        Some(TaskStatus::Blocked(WaitReason::EndpointReceive(recv_cap)))
    );
    assert_eq!(state.current_tid(), Some(1));

    let msg = Message::new(1, b"ok").expect("msg");
    state
        .ipc_send(send_cap_task1, msg)
        .expect("send should wake waiter");
    assert_eq!(state.task_status(0), Some(TaskStatus::Runnable));
}

#[test]
fn synchronous_send_blocks_until_receiver_arrives() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("register task 1");
    state.enqueue_current_cpu(1).expect("queue task 1");
    let (_eid, send_cap, recv_cap) = state
        .create_endpoint_with_mode(1, EndpointMode::Synchronous)
        .expect("sync endpoint");
    let recv_cap_task1 = state
        .grant_capability_task_to_task(0, recv_cap, 1)
        .expect("dup recv cap to task1");

    let msg = Message::new(0, b"xy").expect("msg");
    let send_result = state.ipc_send(send_cap, msg);
    assert_eq!(send_result, Err(KernelError::WouldBlock));
    assert_eq!(
        state.task_status(0),
        Some(TaskStatus::Blocked(WaitReason::EndpointSend(send_cap)))
    );
    assert_eq!(state.current_tid(), Some(1));

    let recv = state
        .ipc_recv(recv_cap_task1)
        .expect("recv call")
        .expect("direct handoff message");
    assert_eq!(recv.as_slice(), b"xy");
    assert_eq!(state.task_status(0), Some(TaskStatus::Runnable));
}

#[test]
fn synchronous_endpoint_supports_multiple_blocked_senders() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("register sender 1");
    state.register_task(2).expect("register sender 2");
    state.register_task(3).expect("register receiver");

    let (_eid, send_cap, recv_cap) = state
        .create_endpoint_with_mode(1, EndpointMode::Synchronous)
        .expect("sync endpoint");
    let send_cap_task1 = state
        .grant_capability_task_to_task(0, send_cap, 1)
        .expect("dup send cap to task1");
    let recv_cap_task3 = state
        .grant_capability_task_to_task(0, recv_cap, 3)
        .expect("dup recv cap to task3");

    state.enqueue_current_cpu(1).expect("queue task 1");
    state.enqueue_current_cpu(2).expect("queue task 2");
    state.enqueue_current_cpu(3).expect("queue task 3");

    let msg0 = Message::new(0, b"m0").expect("msg0");
    assert_eq!(state.ipc_send(send_cap, msg0), Err(KernelError::WouldBlock));
    assert_eq!(
        state.task_status(0),
        Some(TaskStatus::Blocked(WaitReason::EndpointSend(send_cap)))
    );
    assert_eq!(state.current_tid(), Some(1));

    let msg1 = Message::new(1, b"m1").expect("msg1");
    assert_eq!(
        state.ipc_send(send_cap_task1, msg1),
        Err(KernelError::WouldBlock)
    );
    assert_eq!(
        state.task_status(1),
        Some(TaskStatus::Blocked(WaitReason::EndpointSend(
            send_cap_task1
        )))
    );

    state.yield_current().expect("switch to receiver");
    assert_eq!(state.current_tid(), Some(3));

    let first = state
        .ipc_recv(recv_cap_task3)
        .expect("recv1")
        .expect("msg1");
    let second = state
        .ipc_recv(recv_cap_task3)
        .expect("recv2")
        .expect("msg2");
    assert_eq!(first.as_slice(), b"m0");
    assert_eq!(second.as_slice(), b"m1");
}

#[test]
fn blocked_sender_queue_depth_is_uniform_across_endpoints() {
    let mut state = Bootstrap::init().expect("init");
    for tid in 1..=5u64 {
        state.register_task(tid).expect("task");
    }
    let (_eid, send_cap, _recv_cap) = state
        .create_endpoint_with_mode(8, EndpointMode::Synchronous)
        .expect("endpoint");
    let send_caps: [CapId; 5] = [1u64, 2, 3, 4, 5].map(|tid| {
        state
            .grant_capability_task_to_task(0, send_cap, tid)
            .expect("dup send")
    });
    for tid in 1..=5u64 {
        state.enqueue_current_cpu(tid).expect("enqueue");
    }

    assert_eq!(
        state.ipc_send(send_cap, Message::new(0, b"d0").expect("msg")),
        Err(KernelError::WouldBlock)
    );
    for (idx, cap) in send_caps.iter().copied().enumerate() {
        assert_eq!(
            state.ipc_send(cap, Message::new((idx + 1) as u64, b"dx").expect("msg")),
            Err(KernelError::WouldBlock)
        );
    }
}

#[test]
fn stale_endpoint_capability_rejected_after_recreate() {
    let mut state = Bootstrap::init().expect("init");
    let (eid, send_cap, _recv_cap) = state
        .create_endpoint_with_mode(1, EndpointMode::Buffered)
        .expect("endpoint");

    state.destroy_endpoint(eid).expect("destroy");
    let _ = state
        .create_endpoint_with_mode(1, EndpointMode::Buffered)
        .expect("recreate");

    let msg = Message::new(1, b"stale").expect("msg");
    assert_eq!(
        state.ipc_send(send_cap, msg),
        Err(KernelError::StaleCapability)
    );
}

#[test]
fn can_derive_and_revoke_endpoint_capability() {
    let mut state = Bootstrap::init().expect("init");
    let (_eid, send_cap, _recv_cap) = state.create_endpoint(2).expect("endpoint");

    let child = state
        .current_task_capability(send_cap)
        .map(|cap| cap.object)
        .expect("source cap");
    let child = state
        .mint_capability_for_current_context(Capability::new(child, CapRights::SEND))
        .expect("derive");
    let msg = Message::new(9, b"ok").expect("msg");
    assert!(state.ipc_send(child, msg).is_ok());

    let cnode = state.current_task_cnode().expect("cnode");
    assert_eq!(state.revoke_capability_in_cnode(cnode, child), Ok(()));
    let msg2 = Message::new(9, b"no").expect("msg");
    assert_eq!(
        state.ipc_send(child, msg2),
        Err(KernelError::InvalidCapability)
    );
}

#[test]
fn same_cap_id_in_distinct_cnodes_does_not_alias() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.register_task(2).expect("task2");
    let cnode1 = state.task_cnode(1).expect("cnode1");
    let cnode2 = state.task_cnode(2).expect("cnode2");
    let slot_index = 7usize;
    let cap1 = state
        .cspace_for_cnode_mut(cnode1)
        .expect("cspace1")
        .mint_at(
            slot_index,
            Capability::new(CapObject::MemoryObject { id: 0xA1 }, CapRights::READ),
        )
        .expect("mint1");
    let cap2 = state
        .cspace_for_cnode_mut(cnode2)
        .expect("cspace2")
        .mint_at(
            slot_index,
            Capability::new(CapObject::MemoryObject { id: 0xB2 }, CapRights::READ),
        )
        .expect("mint2");
    assert_eq!(cap1, cap2);

    state.enqueue_current_cpu(1).expect("enqueue1");
    state.yield_current().expect("switch1");
    assert_eq!(state.current_tid(), Some(1));
    let task1_view = state.current_task_capability(cap1).expect("task1 cap");
    assert_eq!(task1_view.object, CapObject::MemoryObject { id: 0xA1 });

    state.enqueue_current_cpu(2).expect("enqueue2");
    state.yield_current().expect("switch2a");
    if state.current_tid() != Some(2) {
        state.yield_current().expect("switch2b");
    }
    assert_eq!(state.current_tid(), Some(2));
    let task2_view = state.current_task_capability(cap2).expect("task2 cap");
    assert_eq!(task2_view.object, CapObject::MemoryObject { id: 0xB2 });
}

#[test]
fn revoke_isolated_to_owning_cnode_space() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.register_task(2).expect("task2");
    let cnode1 = state.task_cnode(1).expect("cnode1");
    let cnode2 = state.task_cnode(2).expect("cnode2");
    let slot_index = 9usize;
    let cap = state
        .cspace_for_cnode_mut(cnode1)
        .expect("cspace1")
        .mint_at(
            slot_index,
            Capability::new(CapObject::MemoryObject { id: 0x111 }, CapRights::READ),
        )
        .expect("mint1");
    let cap_other = state
        .cspace_for_cnode_mut(cnode2)
        .expect("cspace2")
        .mint_at(
            slot_index,
            Capability::new(CapObject::MemoryObject { id: 0x222 }, CapRights::READ),
        )
        .expect("mint2");
    assert_eq!(cap, cap_other);
    assert_eq!(
        state
            .cspace_for_cnode_mut(cnode1)
            .expect("cspace1")
            .revoke(cap),
        Ok(())
    );
    assert!(
        state
            .cspace_for_cnode(cnode1)
            .expect("cspace1")
            .get(cap)
            .is_none()
    );
    let remaining = state
        .cspace_for_cnode(cnode2)
        .expect("cspace2")
        .get(cap_other)
        .expect("other cnode cap remains");
    assert_eq!(remaining.object, CapObject::MemoryObject { id: 0x222 });
}

#[test]
fn grant_with_rights_attenuates_delegated_capability() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    let cap = state
        .mint_capability_for_current_context(Capability::new(
            CapObject::Kernel,
            CapRights::READ | CapRights::WRITE | CapRights::MAP,
        ))
        .expect("mint");
    let delegated = state
        .grant_capability_task_to_task_with_rights(0, cap, 1, CapRights::READ | CapRights::MAP)
        .expect("grant");
    let delegated_cap = state
        .resolve_capability_for_task(1, delegated)
        .expect("delegated cap");
    assert!(delegated_cap.has_right(CapRights::READ));
    assert!(delegated_cap.has_right(CapRights::MAP));
    assert!(!delegated_cap.has_right(CapRights::WRITE));
}

#[test]
fn revoke_source_capability_cascades_to_delegated_descendants() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.register_task(2).expect("task2");
    let root = state
        .mint_capability_for_current_context(Capability::new(
            CapObject::Kernel,
            CapRights::READ | CapRights::WRITE,
        ))
        .expect("root");
    let delegated_task1 = state
        .grant_capability_task_to_task_with_rights(0, root, 1, CapRights::READ)
        .expect("delegate task1");
    let delegated_task2 = state
        .grant_capability_task_to_task_with_rights(1, delegated_task1, 2, CapRights::READ)
        .expect("delegate task2");
    assert!(
        state
            .resolve_capability_for_task(1, delegated_task1)
            .is_ok()
    );
    assert!(
        state
            .resolve_capability_for_task(2, delegated_task2)
            .is_ok()
    );

    let root_cnode = state.task_cnode(0).expect("root cnode");
    assert_eq!(state.revoke_capability_in_cnode(root_cnode, root), Ok(()));
    assert!(state.resolve_capability_for_task(0, root).is_err());
    assert!(
        state
            .resolve_capability_for_task(1, delegated_task1)
            .is_err()
    );
    assert!(
        state
            .resolve_capability_for_task(2, delegated_task2)
            .is_err()
    );
}

#[test]
fn source_revoke_cascades_to_multiple_direct_and_transitive_descendants() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.register_task(2).expect("task2");
    state.register_task(3).expect("task3");

    let root = state
        .mint_capability_for_current_context(Capability::new(
            CapObject::Kernel,
            CapRights::READ | CapRights::WRITE,
        ))
        .expect("root");
    let direct_t1 = state
        .grant_capability_task_to_task_with_rights(0, root, 1, CapRights::READ)
        .expect("direct t1");
    let direct_t2 = state
        .grant_capability_task_to_task_with_rights(0, root, 2, CapRights::READ)
        .expect("direct t2");
    let transitive_t3 = state
        .grant_capability_task_to_task_with_rights(1, direct_t1, 3, CapRights::READ)
        .expect("transitive t3");

    assert!(state.resolve_capability_for_task(1, direct_t1).is_ok());
    assert!(state.resolve_capability_for_task(2, direct_t2).is_ok());
    assert!(state.resolve_capability_for_task(3, transitive_t3).is_ok());

    let root_cnode = state.task_cnode(0).expect("root cnode");
    assert_eq!(state.revoke_capability_in_cnode(root_cnode, root), Ok(()));
    assert!(state.resolve_capability_for_task(1, direct_t1).is_err());
    assert!(state.resolve_capability_for_task(2, direct_t2).is_err());
    assert!(state.resolve_capability_for_task(3, transitive_t3).is_err());
}

#[test]
fn source_revoke_only_impacts_delegated_descendants_not_unrelated_caps() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");

    let root = state
        .mint_capability_for_current_context(Capability::new(CapObject::Kernel, CapRights::READ))
        .expect("root");
    let delegated = state
        .grant_capability_task_to_task_with_rights(0, root, 1, CapRights::READ)
        .expect("delegated");
    let unrelated = state
        .mint_capability_for_current_context(Capability::new(
            CapObject::MemoryObject { id: 0xABCD },
            CapRights::READ,
        ))
        .expect("unrelated");

    let root_cnode = state.task_cnode(0).expect("root cnode");
    assert_eq!(state.revoke_capability_in_cnode(root_cnode, root), Ok(()));
    assert!(state.resolve_capability_for_task(1, delegated).is_err());
    assert!(state.resolve_capability_for_task(0, unrelated).is_ok());
}

#[test]
fn invalid_source_revoke_does_not_revoke_delegated_descendants() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    let root = state
        .mint_capability_for_current_context(Capability::new(CapObject::Kernel, CapRights::READ))
        .expect("root");
    let delegated = state
        .grant_capability_task_to_task_with_rights(0, root, 1, CapRights::READ)
        .expect("delegate");
    let root_cnode = state.task_cnode(0).expect("root cnode");
    let bogus = CapId(root.0.wrapping_add(1));
    assert_eq!(
        state.revoke_capability_in_cnode(root_cnode, bogus),
        Err(KernelError::InvalidCapability)
    );
    assert!(state.resolve_capability_for_task(0, root).is_ok());
    assert!(state.resolve_capability_for_task(1, delegated).is_ok());
}

#[test]
fn ipc_message_header_and_cap_transfer_metadata_are_preserved() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue");
    state.dispatch_next_task().expect("dispatch");
    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
    let (_mem_id, mem_cap) = state.create_memory_object(PhysAddr(0xC000)).expect("mem");
    let recv_cap_task1 = state
        .grant_capability_task_to_task(0, recv_cap, 1)
        .expect("dup recv to task1");
    state.yield_current().expect("switch to task1");
    assert_eq!(state.current_tid(), Some(1));
    assert_eq!(state.ipc_recv(recv_cap_task1).expect("block recv"), None);
    assert_eq!(state.current_tid(), Some(0));

    state
        .ipc_send_with_cap_transfer(send_cap, ThreadId(0), 0x55, mem_cap, b"mt")
        .expect("send transfer");
    state.yield_current().expect("switch receiver");
    assert_eq!(state.current_tid(), Some(1));
    let msg = state
        .ipc_recv(recv_cap_task1)
        .expect("recv")
        .expect("message");

    assert_eq!(msg.opcode, 0x55);
    assert_eq!(
        msg.flags & Message::FLAG_CAP_TRANSFER,
        Message::FLAG_CAP_TRANSFER
    );
    assert_ne!(msg.transferred_cap().map(|cap| cap.0), Some(mem_cap.0));
    assert_eq!(msg.as_slice(), b"mt");
}

#[test]
fn syscall_trap_dispatches_ipc_send_recv() {
    let mut state = Bootstrap::init().expect("init");
    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");

    let send_payload = usize::from_le_bytes([b'h', b'i', 0, 0, 0, 0, 0, 0]);
    let mut send_frame = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcSend as usize,
        [
            send_cap.0 as usize,
            42,
            2,
            send_payload,
            0,
            crate::kernel::syscall::SYSCALL_NO_TRANSFER_CAP as usize,
        ],
    );

    state
        .handle_trap(Trap::Syscall, Some(&mut send_frame))
        .expect("syscall send");
    assert_eq!(send_frame.error_code(), None);

    let mut recv_frame = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcRecv as usize,
        [recv_cap.0 as usize, 0, 0, 0, 0, 0],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut recv_frame))
        .expect("syscall recv");
    assert_eq!(recv_frame.error_code(), None);
    assert_eq!(recv_frame.ret0() as u64, 0);
    assert_eq!(recv_frame.ret1(), 2);
    assert_eq!(recv_frame.arg(3) & 0xFF, b'h' as usize);
}

#[test]
fn control_plane_cnode_resize_syscall_trap_allows_system_server_target() {
    let mut state = Bootstrap::init().expect("init");
    state
        .register_task_with_class(810, TaskClass::SystemServer)
        .expect("register system server");
    state
        .register_task_with_class(811, TaskClass::App)
        .expect("register app");
    state
        .enqueue_current_cpu(810)
        .expect("enqueue system server");
    state.dispatch_next_task().expect("dispatch");
    if state.current_tid() != Some(810) {
        state.yield_current().expect("switch to system server");
    }

    let target_cnode = state.process_cnode_for_pid(811).expect("target cnode");
    let before = state
        .cnode_slot_capacity(target_cnode)
        .expect("target capacity");
    let requested = before.saturating_add(4);
    state
        .control_plane_set_process_cnode_slots_via_syscall(811, requested)
        .expect("control-plane resize syscall");
    assert_eq!(state.cnode_slot_capacity(target_cnode), Some(requested));
}

#[test]
fn control_plane_cnode_resize_syscall_trap_denies_non_system_server_targeting_other_process() {
    let mut state = Bootstrap::init().expect("init");
    state
        .register_task_with_class(820, TaskClass::App)
        .expect("register requester");
    state
        .register_task_with_class(821, TaskClass::App)
        .expect("register target");
    state.enqueue_current_cpu(820).expect("enqueue requester");
    state.dispatch_next_task().expect("dispatch");
    if state.current_tid() != Some(820) {
        state.yield_current().expect("switch to requester");
    }

    let target_cnode = state.process_cnode_for_pid(821).expect("target cnode");
    let before = state
        .cnode_slot_capacity(target_cnode)
        .expect("target capacity");
    let err = state
        .control_plane_set_process_cnode_slots_via_syscall(821, before.saturating_add(4))
        .expect_err("control-plane policy must deny");
    assert_eq!(
        err,
        TrapHandleError::Syscall(crate::kernel::syscall::SyscallError::MissingRight)
    );
    assert_eq!(state.cnode_slot_capacity(target_cnode), Some(before));
}

#[test]
fn control_plane_cnode_resize_syscall_wrapper_rejects_zero_target_pid() {
    let mut state = Bootstrap::init().expect("init");
    let err = state
        .control_plane_set_process_cnode_slots_via_syscall(0, 8)
        .expect_err("zero pid must be rejected");
    assert_eq!(
        err,
        TrapHandleError::Syscall(crate::kernel::syscall::SyscallError::InvalidArgs)
    );
}

#[test]
fn user_address_space_mapping_enforces_split_and_alignment() {
    let mut state = Bootstrap::init().expect("init");
    let (_asid, aspace_map_cap) = state.create_user_address_space().expect("asid");

    let ok = state.map_user_page(
        aspace_map_cap,
        VirtAddr(0x1000),
        Mapping {
            phys: PhysAddr(0x2000),
            flags: PageFlags {
                read: true,
                write: true,
                execute: true,
                user: true,
                cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
            },
        },
    );
    assert_eq!(ok, Ok(None));

    let bad_range = state.map_user_page(
        aspace_map_cap,
        VirtAddr(crate::kernel::vm::KERNEL_SPACE_BASE),
        Mapping {
            phys: PhysAddr(0x3000),
            flags: PageFlags::USER_RX,
        },
    );
    assert_eq!(bad_range, Err(KernelError::Vm(VmError::PrivilegeViolation)));

    let misaligned = state.map_user_page(
        aspace_map_cap,
        VirtAddr(0x1001),
        Mapping {
            phys: PhysAddr(0x4000),
            flags: PageFlags::USER_RX,
        },
    );
    assert_eq!(misaligned, Err(KernelError::Vm(VmError::Misaligned)));
}

#[test]
fn user_address_space_mapping_requires_aspace_map_capability() {
    let mut state = Bootstrap::init().expect("init");
    let (_asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
    let (_eid, send_cap, _recv_cap) = state.create_endpoint(2).expect("endpoint");

    let wrong_object = state.map_user_page(
        send_cap,
        VirtAddr(0x1000),
        Mapping {
            phys: PhysAddr(0x2000),
            flags: PageFlags::USER_RX,
        },
    );
    assert_eq!(wrong_object, Err(KernelError::WrongObject));

    let read_only_cap = state
        .current_task_capability(aspace_map_cap)
        .map(|cap| cap.object)
        .expect("aspace cap object");
    let read_only_cap = state
        .mint_capability_for_current_context(Capability::new(read_only_cap, CapRights::READ))
        .expect("derive read-only aspace cap");
    let missing_right = state.map_user_page(
        read_only_cap,
        VirtAddr(0x1000),
        Mapping {
            phys: PhysAddr(0x3000),
            flags: PageFlags::USER_RX,
        },
    );
    assert_eq!(missing_right, Err(KernelError::MissingRight));
}

#[test]
fn memory_object_capability_controls_mapping_and_unmap_protect() {
    let mut state = Bootstrap::init().expect("init");
    let (_asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
    let (_mem_id, mem_cap) = state
        .create_memory_object(PhysAddr(0x9000))
        .expect("memobj");

    let mapped = state.map_user_page_with_caps(
        aspace_map_cap,
        mem_cap,
        VirtAddr(0x2000),
        PageFlags {
            read: true,
            write: true,
            execute: false,
            user: true,
            cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
        },
    );
    assert_eq!(mapped, Ok(None));

    let old = state
        .protect_user_page(aspace_map_cap, VirtAddr(0x2000), PageFlags::USER_RX)
        .expect("protect")
        .expect("old mapping");
    assert_eq!(old.flags.write, true);

    let unmapped = state
        .unmap_user_page(aspace_map_cap, VirtAddr(0x2000))
        .expect("unmap")
        .expect("mapped entry");
    assert_eq!(unmapped.phys, PhysAddr(0x9000));
}

#[test]
fn smp_unmap_waits_for_live_tlb_shootdown_completion() {
    let mut state = Bootstrap::init().expect("init");
    state.bring_up_cpu(CpuId(1)).expect("cpu1");

    let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(0, asid).expect("bind cpu0 task");
    state.register_task(1).expect("task1");
    state.bind_task_asid(1, asid).expect("bind cpu1 task");
    state.enqueue_on_cpu(CpuId(1), 1).expect("enqueue cpu1");
    state.set_current_cpu(CpuId(1)).expect("switch cpu1");
    state.dispatch_next_task().expect("dispatch cpu1");
    assert_eq!(state.current_tid_on_cpu(CpuId(1)), Some(1));
    state.set_current_cpu(CpuId(0)).expect("switch cpu0");

    let (_mem_id, mem_cap) = state
        .create_memory_object(PhysAddr(0xB000))
        .expect("memobj");
    state
        .map_user_page_with_caps(
            aspace_map_cap,
            mem_cap,
            VirtAddr(0x4000),
            PageFlags {
                read: true,
                write: true,
                execute: false,
                user: true,
                cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
            },
        )
        .expect("map");

    let _ = state
        .unmap_user_page(aspace_map_cap, VirtAddr(0x4000))
        .expect("unmap")
        .expect("mapped");

    assert!(
        state.tlb_shootdown_count() >= 1,
        "remote shootdown handler should run at least once"
    );
    assert_eq!(
        state.with_ipc_state(|ipc| {
            ipc.live_tlb_shootdown
                .active
                .map(|wait| wait.pending_cpu_bitmap)
                .unwrap_or(0)
        }),
        0
    );
    state.set_current_cpu(CpuId(0)).expect("switch cpu0");
    assert_eq!(state.drain_cross_cpu_work().expect("drain cpu0"), None);
    state.set_current_cpu(CpuId(1)).expect("switch cpu1");
    assert_eq!(state.drain_cross_cpu_work().expect("drain cpu1"), None);
}

#[test]
fn memory_object_mapping_requires_memory_rights() {
    let mut state = Bootstrap::init().expect("init");
    let (_asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
    let (_mem_id, mem_cap) = state
        .create_memory_object(PhysAddr(0xA000))
        .expect("memobj");

    let readonly_mem = state
        .current_task_capability(mem_cap)
        .map(|cap| cap.object)
        .expect("mem cap object");
    let readonly_mem = state
        .mint_capability_for_current_context(Capability::new(readonly_mem, CapRights::READ))
        .expect("derive ro");

    let res = state.map_user_page_with_caps(
        aspace_map_cap,
        readonly_mem,
        VirtAddr(0x3000),
        PageFlags {
            read: true,
            write: true,
            execute: false,
            user: true,
            cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
        },
    );
    assert_eq!(res, Err(KernelError::MissingRight));
}

#[test]
fn revoked_unmapped_memory_object_reclaims_frame() {
    let mut state = Bootstrap::init().expect("init");
    let (id, mem_cap) = state.alloc_anonymous_memory_object().expect("anon");
    let phys = state
        .memory
        .memory_objects
        .iter()
        .flatten()
        .find(|entry| entry.id == id)
        .map(|entry| entry.phys)
        .expect("phys");

    let cnode = state.current_task_cnode().expect("cnode");
    state
        .revoke_capability_in_cnode(cnode, mem_cap)
        .expect("revoke mem cap");

    assert!(
        state
            .memory
            .memory_objects
            .iter()
            .flatten()
            .all(|entry| entry.id != id)
    );

    let (_next_id, next_cap) = state.alloc_anonymous_memory_object().expect("next anon");
    let next_phys = state
        .capability_service()
        .resolve_current_task_capability(next_cap)
        .expect("next cap")
        .object;
    let next_phys = match next_phys {
        CapObject::MemoryObject { id } => state
            .memory
            .memory_objects
            .iter()
            .flatten()
            .find(|entry| entry.id == id)
            .map(|entry| entry.phys)
            .expect("next phys"),
        _ => panic!("unexpected cap object"),
    };
    assert_eq!(next_phys, phys);
}

#[test]
fn syscall_send_can_copy_from_user_memory_when_task_has_asid() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(0, asid).expect("bind");
    state
        .map_user_page(
            aspace_map_cap,
            VirtAddr(0x0),
            Mapping {
                phys: PhysAddr(0x5000),
                flags: PageFlags {
                    read: true,
                    write: true,
                    execute: true,
                    user: true,
                    cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
                },
            },
        )
        .expect("map");
    state.write_user_memory(0, 0, b"hi").expect("write");

    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
    let mut send_frame = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcSend as usize,
        [
            send_cap.0 as usize,
            0,
            2,
            0,
            0,
            crate::kernel::syscall::SYSCALL_NO_TRANSFER_CAP as usize,
        ],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut send_frame))
        .expect("send syscall");

    let received = state.ipc_recv(recv_cap).expect("recv").expect("msg");
    assert_eq!(received.as_slice(), b"hi");
}

#[test]
fn syscall_send_large_payload_uses_shared_region_descriptor_with_cap_transfer() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue");
    state.dispatch_next_task().expect("dispatch");
    let (asid, _aspace_map_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(0, asid).expect("bind");
    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
    let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let recv_cap_task1 = state
        .grant_capability_task_to_task(0, recv_cap, 1)
        .expect("dup recv to task1");
    state.yield_current().expect("switch to task1");
    assert_eq!(state.current_tid(), Some(1));
    assert_eq!(state.ipc_recv(recv_cap_task1).expect("block recv"), None);
    assert_eq!(state.current_tid(), Some(0));

    let mut send_frame = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcSend as usize,
        [
            send_cap.0 as usize,
            0x2000,
            Message::MAX_PAYLOAD + 16,
            0,
            0,
            mem_cap.0 as usize,
        ],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut send_frame))
        .expect("send syscall");

    state.yield_current().expect("switch receiver");
    assert_eq!(state.current_tid(), Some(1));
    let msg = state.ipc_recv(recv_cap_task1).expect("recv").expect("msg");
    assert!(msg.transferred_cap().is_some());
    let region = crate::kernel::ipc::SharedMemoryRegion::decode(msg.as_slice()).expect("region");
    assert_eq!(region.offset, 0x2000);
    assert_eq!(region.len as usize, Message::MAX_PAYLOAD + 16);
}

#[test]
fn syscall_recv_can_copy_to_user_memory_when_task_has_asid() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(0, asid).expect("bind");

    state
        .map_user_page(
            aspace_map_cap,
            VirtAddr(0x0),
            Mapping {
                phys: PhysAddr(0x6000),
                flags: PageFlags {
                    read: true,
                    write: true,
                    execute: false,
                    user: true,
                    cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
                },
            },
        )
        .expect("map rw");

    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
    state
        .ipc_send(send_cap, Message::new(9, b"ok").expect("msg"))
        .expect("send");

    let mut recv_frame = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcRecv as usize,
        [recv_cap.0 as usize, 16, 2, 0, 0, 0],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut recv_frame))
        .expect("recv syscall");

    assert_eq!(recv_frame.error_code(), None);
    let bytes = state.read_user_memory(0, 16, 2).expect("read back");
    assert_eq!(&bytes[..2], b"ok");
}

#[test]
fn syscall_recv_reports_page_fault_on_unwritable_user_buffer() {
    use super::super::syscall::SyscallError;

    let mut state = Bootstrap::init().expect("init");
    let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(0, asid).expect("bind");

    state
        .map_user_page(
            aspace_map_cap,
            VirtAddr(0x0),
            Mapping {
                phys: PhysAddr(0x7000),
                flags: PageFlags::USER_RX,
            },
        )
        .expect("map rx only");

    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
    state
        .ipc_send(send_cap, Message::new(4, b"pf").expect("msg"))
        .expect("send");

    let mut recv_frame = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcRecv as usize,
        [recv_cap.0 as usize, 8, 2, 0, 0, 0],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut recv_frame))
        .expect("recv syscall should return fault code, not trap error");

    assert_eq!(
        recv_frame.error_code(),
        Some(SyscallError::PageFault.code())
    );
    assert_eq!(
        state.last_fault(),
        Some(super::super::trap::FaultInfo {
            addr: VirtAddr(8),
            access: super::super::trap::FaultAccess::Write,
        })
    );
}

#[test]
fn page_fault_syscall_faults_current_task_and_schedules_next() {
    use super::super::syscall::SyscallError;

    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue task1");

    let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(0, asid).expect("bind");
    state
        .map_user_page(
            aspace_map_cap,
            VirtAddr(0x0),
            Mapping {
                phys: PhysAddr(0x7000),
                flags: PageFlags::USER_RX,
            },
        )
        .expect("map rx");

    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
    state
        .ipc_send(send_cap, Message::new(4, b"pf").expect("msg"))
        .expect("send");

    let mut recv_frame = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcRecv as usize,
        [recv_cap.0 as usize, 8, 2, 0, 0, 0],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut recv_frame))
        .expect("syscall handled");

    assert_eq!(
        recv_frame.error_code(),
        Some(SyscallError::PageFault.code())
    );
    assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
    assert_eq!(state.current_tid(), Some(1));
}

#[test]
fn set_fault_handler_requires_receive_capability() {
    let mut state = Bootstrap::init().expect("init");
    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");

    assert_eq!(
        state.set_fault_handler(send_cap),
        Err(KernelError::MissingRight)
    );
    assert!(state.set_fault_handler(recv_cap).is_ok());
}

#[test]
fn page_fault_emits_report_to_fault_handler_endpoint() {
    use super::super::syscall::SyscallError;
    use super::fault_state::SupervisorFaultReportWire;

    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue task1");

    let (_handler_eid, _handler_send, handler_recv) =
        state.create_endpoint(4).expect("handler endpoint");
    state.set_fault_handler(handler_recv).expect("set handler");
    let handler_recv_task1 = state
        .grant_capability_task_to_task(0, handler_recv, 1)
        .expect("dup handler recv to task1");

    let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(0, asid).expect("bind");
    state
        .map_user_page(
            aspace_map_cap,
            VirtAddr(0x0),
            Mapping {
                phys: PhysAddr(0x7000),
                flags: PageFlags::USER_RX,
            },
        )
        .expect("map rx");

    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
    state
        .ipc_send(send_cap, Message::new(4, b"pf").expect("msg"))
        .expect("send");

    let mut recv_frame = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcRecv as usize,
        [recv_cap.0 as usize, 8, 2, 0, 0, 0],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut recv_frame))
        .expect("syscall handled");

    assert_eq!(
        recv_frame.error_code(),
        Some(SyscallError::PageFault.code())
    );
    assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
    assert_eq!(state.current_tid(), Some(1));

    let report = state
        .ipc_recv(handler_recv_task1)
        .expect("handler recv")
        .expect("fault report");
    assert_eq!(report.sender_tid.0, 0);
    let decoded = SupervisorFaultReportWire::decode(report.as_slice()).expect("decode fault wire");
    assert_eq!(decoded.faulting_tid, 0);
    assert_eq!(decoded.access, super::super::trap::FaultAccess::Write);
}

#[test]
fn fault_policy_defaults_to_kill_task() {
    let state = Bootstrap::init().expect("init");
    assert_eq!(state.fault_policy(), FaultPolicy::KillTask);
}

#[test]
fn page_fault_with_notify_and_continue_keeps_current_task_running() {
    use super::super::syscall::SyscallError;
    use super::fault_state::SupervisorFaultReportWire;

    let mut state = Bootstrap::init().expect("init");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue task1");
    state.set_fault_policy(FaultPolicy::NotifyAndContinue);

    let (_handler_eid, _handler_send, handler_recv) =
        state.create_endpoint(4).expect("handler endpoint");
    state.set_fault_handler(handler_recv).expect("set handler");

    let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(0, asid).expect("bind");
    state
        .map_user_page(
            aspace_map_cap,
            VirtAddr(0x0),
            Mapping {
                phys: PhysAddr(0x7000),
                flags: PageFlags::USER_RX,
            },
        )
        .expect("map rx");

    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
    state
        .ipc_send(send_cap, Message::new(4, b"pf").expect("msg"))
        .expect("send");

    let mut recv_frame = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcRecv as usize,
        [recv_cap.0 as usize, 8, 2, 0, 0, 0],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut recv_frame))
        .expect("syscall handled");

    assert_eq!(
        recv_frame.error_code(),
        Some(SyscallError::PageFault.code())
    );
    assert_eq!(state.task_status(0), Some(TaskStatus::Running));
    assert_eq!(state.current_tid(), Some(0));

    let report = state
        .ipc_recv(handler_recv)
        .expect("handler recv")
        .expect("fault report");
    assert_eq!(report.sender_tid.0, 0);
    let decoded = SupervisorFaultReportWire::decode(report.as_slice()).expect("decode fault wire");
    assert_eq!(decoded.faulting_tid, 0);
}

#[test]
fn task_fault_policy_override_beats_global_policy() {
    use super::super::syscall::SyscallError;

    let mut state = Bootstrap::init().expect("init");
    state.set_fault_policy(FaultPolicy::NotifyAndContinue);
    state
        .set_task_fault_policy(0, Some(FaultPolicy::KillTask))
        .expect("set override");
    state.register_task(1).expect("task1");
    state.enqueue_current_cpu(1).expect("enqueue task1");

    let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(0, asid).expect("bind");
    state
        .map_user_page(
            aspace_map_cap,
            VirtAddr(0x0),
            Mapping {
                phys: PhysAddr(0xB000),
                flags: PageFlags::USER_RX,
            },
        )
        .expect("map rx");

    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
    state
        .ipc_send(send_cap, Message::new(4, b"pf").expect("msg"))
        .expect("send");

    let mut recv_frame = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcRecv as usize,
        [recv_cap.0 as usize, 8, 2, 0, 0, 0],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut recv_frame))
        .expect("syscall handled");

    assert_eq!(
        recv_frame.error_code(),
        Some(SyscallError::PageFault.code())
    );
    assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
    assert_eq!(state.current_tid(), Some(1));
}

#[test]
fn notification_irq_route_delivers_message_to_bound_endpoint() {
    let mut state = Bootstrap::init().expect("init");
    let (_notif_idx, notif_cap, notif_recv_cap) = state.create_notification(4).expect("notif");
    state.bind_irq_notification(11, notif_cap).expect("bind");

    state
        .handle_trap_event(TrapEvent::ExternalInterrupt(11), None)
        .expect("handle irq");

    let msg = state.ipc_recv(notif_recv_cap).expect("recv").expect("msg");
    assert_eq!(msg.opcode, 11);
    assert_eq!(msg.as_slice()[0], 11);
}

#[test]
fn create_notification_rejects_non_signal_cap_for_irq_binding() {
    let mut state = Bootstrap::init().expect("init");
    let (_eid, _send_cap, recv_cap) = state.create_endpoint(2).expect("ep");
    let err = state
        .bind_irq_notification(1, recv_cap)
        .expect_err("must fail");
    assert_eq!(err, KernelError::MissingRight);
}

#[test]
fn delegate_device_server_caps_configures_driver_record() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(34).expect("task");
    let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let iova_cap = state.create_iova_space_cap().expect("iova");

    let plan = DeviceServerDelegation {
        server_tid: ThreadId(34),
        irq_line: 10,
        mem_cap,
        dma_offset: 0,
        dma_len: crate::kernel::vm::PAGE_SIZE,
        iova_cap,
        iova_base: crate::kernel::vm::PAGE_SIZE * 8,
        iova_len: crate::kernel::vm::PAGE_SIZE,
    };

    let (irq_cap, dma_cap, iova_cap) = state.delegate_device_server_caps(plan).expect("delegate");
    let driver_cnode = state.task_cnode(34).expect("driver cnode");
    assert!(state.capability_for_cnode(driver_cnode, irq_cap).is_some());
    assert!(state.capability_for_cnode(driver_cnode, dma_cap).is_some());
    assert!(state.capability_for_cnode(driver_cnode, iova_cap).is_some());
    assert!(
        state
            .validate_driver_dma_iova(
                34,
                crate::kernel::vm::PAGE_SIZE * 8,
                crate::kernel::vm::PAGE_SIZE,
            )
            .is_ok()
    );
}

#[test]
fn ipc_fastpath_telemetry_distinguishes_switch_and_queue_paths() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(60).expect("sender");
    state.register_task(61).expect("receiver");

    let (_eid, send_cap, recv_cap) = state
        .create_endpoint_with_mode(2, EndpointMode::Synchronous)
        .expect("endpoint");
    let recv_cap_task61 = state
        .grant_capability_task_to_task(0, recv_cap, 61)
        .expect("dup recv to task61");
    let send_cap_task60 = state
        .grant_capability_task_to_task(0, send_cap, 60)
        .expect("dup send to task60");

    state.enqueue_current_cpu(61).expect("enqueue receiver");
    state.yield_current().expect("run receiver");
    assert_eq!(state.current_tid(), Some(61));
    let mut recv_tf = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcRecv as usize,
        [recv_cap_task61.0 as usize, 8, 0x9000, 0, 0, 0],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut recv_tf))
        .expect("recv trap");

    state.enqueue_current_cpu(60).expect("enqueue sender");
    state.yield_current().expect("run sender");
    if state.current_tid() != Some(60) {
        state.yield_current().expect("run sender retry");
    }
    assert_eq!(state.current_tid(), Some(60));
    let msg = Message::new(60, b"fp").expect("msg");
    let fast = state
        .ipc_send_fastpath(send_cap_task60, msg)
        .expect("fastpath");
    assert!(fast.switched_to_waiter);

    let (_beid, bsend_cap, _brecv_cap) = state.create_endpoint(2).expect("buffered");
    let queued = Message::new(60, b"q").expect("queued");
    state.ipc_send(bsend_cap, queued).expect("queue send");

    let t = state.ipc_path_telemetry();
    assert_eq!(t.fastpath_attempts, 1);
    assert_eq!(t.fastpath_switches, 1);
    assert_eq!(t.queued_sends, 1);
    assert_eq!(t.blocked_sends, 0);
    assert_eq!(t.rendezvous_handoffs, 1);
    assert_eq!(t.scheduler_fastpath_handoffs, 1);
    assert!(t.scheduler_context_switches >= 1);
    assert!(t.scheduler_yield_calls >= 2);
}

#[test]
fn capacity_telemetry_reports_bootstrap_usage() {
    let state = Bootstrap::init().expect("init");
    let t = state.capacity_telemetry();

    assert_eq!(t.tasks.used, 1);
    assert_eq!(t.tasks.capacity, super::MAX_TASKS);
    assert_eq!(t.endpoints.used, 0);
    assert_eq!(t.notifications.used, 0);
    assert_eq!(t.capability_slots.used, 0);
    assert!(!t.tasks.near_full);
}

#[test]
fn capacity_telemetry_marks_endpoint_pressure_near_full() {
    let mut state = Bootstrap::init().expect("init");
    let threshold = (super::MAX_ENDPOINTS * 9).div_ceil(10);
    for _ in 0..threshold {
        let _ = state.create_endpoint(1).expect("endpoint");
    }

    let t = state.capacity_telemetry();
    assert_eq!(t.endpoints.used, threshold);
    assert_eq!(t.endpoints.capacity, super::MAX_ENDPOINTS);
    assert!(t.endpoints.near_full);
}

#[test]
fn runtime_capacity_profile_constrained_limits_endpoint_creation() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    let limits = state.runtime_capacity_config();
    assert_eq!(state.capacity_profile(), KernelCapacityProfile::Constrained);

    for _ in 0..limits.max_endpoints {
        state.create_endpoint(1).expect("endpoint");
    }
    assert_eq!(state.create_endpoint(1), Err(KernelError::EndpointFull));
}

#[test]
fn runtime_capacity_profile_constrained_limits_task_creation() {
    let mut task_state = crate::std::boxed::Box::new(
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init"),
    );
    let limits = task_state.runtime_capacity_config();

    for tid in 2..=limits.max_tasks as u64 {
        task_state.register_task(tid).expect("task");
    }
    assert_eq!(
        task_state.register_task((limits.max_tasks + 1) as u64),
        Err(KernelError::TaskTableFull)
    );
}

#[test]
fn runtime_capacity_profile_constrained_limits_driver_registration() {
    let mut driver_state = crate::std::boxed::Box::new(
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init"),
    );
    let limits = driver_state.runtime_capacity_config();
    let registerable_drivers =
        core::cmp::min(limits.max_drivers, limits.max_tasks.saturating_sub(1));
    for offset in 0..registerable_drivers {
        let tid = (offset + 2) as u64;
        driver_state.register_task(tid).expect("task");
        driver_state.register_driver(tid).expect("driver");
    }
    if registerable_drivers == limits.max_drivers && limits.max_drivers < limits.max_tasks {
        let overflow_tid = (limits.max_drivers + 2) as u64;
        driver_state.register_task(overflow_tid).expect("task");
        assert_eq!(
            driver_state.register_driver(overflow_tid),
            Err(KernelError::TaskTableFull)
        );
    }
}

#[test]
fn runtime_capacity_profile_constrained_limits_memory_objects() {
    let mut memory_state = crate::std::boxed::Box::new(
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init"),
    );
    let limits = memory_state.runtime_capacity_config();

    for _ in 0..limits.max_memory_objects {
        memory_state
            .create_memory_object(crate::kernel::vm::PhysAddr(0x1000_0000))
            .expect("memory object");
    }
    assert_eq!(
        memory_state.create_memory_object(crate::kernel::vm::PhysAddr(0x1000_0000)),
        Err(KernelError::MemoryObjectFull)
    );
}

#[test]
fn capacity_telemetry_reports_runtime_profile_capacities() {
    let state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    let limits = state.runtime_capacity_config();
    let t = state.capacity_telemetry();

    assert_eq!(t.endpoints.capacity, limits.max_endpoints);
    assert_eq!(t.notifications.capacity, limits.max_notifications);
    assert_eq!(t.tasks.capacity, limits.max_tasks);
    assert_eq!(t.drivers.capacity, limits.max_drivers);
    assert_eq!(t.memory_objects.capacity, limits.max_memory_objects);
    assert_eq!(t.capability_slots.capacity, limits.max_total_cnode_slots);
}

#[test]
fn constrained_profile_uses_smaller_default_cnode_slot_capacity_for_apps() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    let limits = state.runtime_capacity_config();
    state
        .register_task_with_class(220, TaskClass::App)
        .expect("app task");

    let cnode = state.process_cnode_for_pid(220).expect("process cnode");
    assert_eq!(
        state.cnode_slot_capacity(cnode),
        Some(limits.default_cnode_slot_capacity)
    );
}

#[test]
fn driver_tasks_get_max_cnode_slot_capacity() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    let limits = state.runtime_capacity_config();
    state
        .register_task_with_class(221, TaskClass::Driver)
        .expect("driver task");

    let cnode = state.process_cnode_for_pid(221).expect("process cnode");
    assert_eq!(
        state.cnode_slot_capacity(cnode),
        Some(limits.driver_cnode_slot_capacity)
    );
}

#[test]
fn system_server_can_request_larger_cnode_slots_on_registration() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    let limits = state.runtime_capacity_config();
    let requested = limits.default_cnode_slot_capacity.saturating_add(32);
    state
        .register_task_with_class_and_cnode_slots_in_process(
            225,
            TaskClass::SystemServer,
            225,
            Some(requested),
        )
        .expect("system server task");
    let cnode = state.process_cnode_for_pid(225).expect("process cnode");
    assert_eq!(state.cnode_slot_capacity(cnode), Some(requested));
}

#[test]
fn app_cannot_request_non_default_cnode_slots_on_registration() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    let limits = state.runtime_capacity_config();
    let requested = limits.default_cnode_slot_capacity.saturating_add(1);
    assert_eq!(
        state.register_task_with_class_and_cnode_slots_in_process(
            226,
            TaskClass::App,
            226,
            Some(requested),
        ),
        Err(KernelError::MissingRight)
    );
}

#[test]
fn capability_space_telemetry_tracks_revoke_scratch_cache_reuse() {
    let mut state = Bootstrap::init().expect("init");
    state
        .register_task_with_class(227, TaskClass::Driver)
        .expect("driver task");
    let cnode = state.process_cnode_for_pid(227).expect("process cnode");
    let cap = Capability::new(CapObject::Kernel, CapRights::READ);

    let first = state
        .mint_capability_in_cnode(cnode, cap)
        .expect("mint first");
    state
        .revoke_capability_in_cnode(cnode, first)
        .expect("revoke first");
    let second = state
        .mint_capability_in_cnode(cnode, cap)
        .expect("mint second");
    state
        .revoke_capability_in_cnode(cnode, second)
        .expect("revoke second");

    let telemetry = state.capability_space_telemetry();
    assert!(telemetry.cnode_spaces >= 1);
    assert!(telemetry.revoke_scratch_cache_misses >= 1);
    assert!(telemetry.revoke_scratch_cache_hits >= 1);
}

fn total_reserved_cnode_slots(state: &KernelState) -> usize {
    state.with_capability_state(|capability| {
        capability
            .cnode_spaces
            .iter()
            .flatten()
            .map(|space| space.slot_capacity)
            .sum()
    })
}

#[test]
fn cnode_resize_grow_updates_total_slot_accounting() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    state
        .register_task_with_class(232, TaskClass::Driver)
        .expect("driver task");
    let cnode = state.process_cnode_for_pid(232).expect("cnode");
    let before_total = total_reserved_cnode_slots(&state);
    let before = state.cnode_slot_capacity(cnode).expect("capacity");
    let grow_by = 7usize;

    state
        .resize_cnode_slots(cnode, before.saturating_add(grow_by))
        .expect("grow");

    let after_total = total_reserved_cnode_slots(&state);
    assert_eq!(after_total, before_total.saturating_add(grow_by));
}

#[test]
fn cnode_resize_shrink_updates_total_slot_accounting() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    state
        .register_task_with_class(233, TaskClass::Driver)
        .expect("driver task");
    let cnode = state.process_cnode_for_pid(233).expect("cnode");
    let before_total = total_reserved_cnode_slots(&state);
    let before = state.cnode_slot_capacity(cnode).expect("capacity");
    assert!(before > 8);
    let shrink_by = 8usize;

    state
        .resize_cnode_slots(cnode, before.saturating_sub(shrink_by))
        .expect("shrink");

    let after_total = total_reserved_cnode_slots(&state);
    assert_eq!(after_total, before_total.saturating_sub(shrink_by));
}

#[test]
fn failed_cnode_grow_keeps_total_slot_accounting_unchanged() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    state
        .register_task_with_class(234, TaskClass::Driver)
        .expect("driver task");
    let limits = state.runtime_capacity_config();
    let cnode = state.process_cnode_for_pid(234).expect("cnode");
    let before_total = total_reserved_cnode_slots(&state);
    let before = state.cnode_slot_capacity(cnode).expect("capacity");
    let over_budget_target = before
        .saturating_add(limits.max_total_cnode_slots)
        .saturating_add(1);

    assert_eq!(
        state.resize_cnode_slots(cnode, over_budget_target),
        Err(KernelError::CapabilityFull)
    );
    assert_eq!(total_reserved_cnode_slots(&state), before_total);
}

#[test]
fn process_cnode_cleanup_releases_total_slot_accounting() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    state
        .register_task_with_class(235, TaskClass::Driver)
        .expect("driver task");
    let cnode = state.process_cnode_for_pid(235).expect("cnode");
    let cnode_slots = state.cnode_slot_capacity(cnode).expect("capacity");
    let before_total = total_reserved_cnode_slots(&state);

    state.mark_task_dead(235).expect("dead");

    let after_total = total_reserved_cnode_slots(&state);
    assert_eq!(after_total, before_total.saturating_sub(cnode_slots));
}

#[test]
fn repeated_cnode_resize_cycles_do_not_drift_total_slot_accounting() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    state
        .register_task_with_class(236, TaskClass::Driver)
        .expect("driver task");
    let cnode = state.process_cnode_for_pid(236).expect("cnode");
    let base = state.cnode_slot_capacity(cnode).expect("capacity");
    let baseline_total = total_reserved_cnode_slots(&state);

    for _ in 0..32 {
        state
            .resize_cnode_slots(cnode, base.saturating_add(9))
            .expect("grow cycle");
        state.resize_cnode_slots(cnode, base).expect("shrink cycle");
        assert_eq!(total_reserved_cnode_slots(&state), baseline_total);
    }
}

#[test]
fn system_server_control_plane_can_resize_other_process_cnode() {
    let mut state = Bootstrap::init().expect("init");
    state
        .register_task_with_class(228, TaskClass::SystemServer)
        .expect("system server task");
    state
        .register_task_with_class(229, TaskClass::App)
        .expect("app task");

    let app_cnode = state.process_cnode_for_pid(229).expect("app cnode");
    let before = state.cnode_slot_capacity(app_cnode).expect("capacity");
    let requested = before.saturating_add(8);

    state
        .control_plane_set_process_cnode_slots(228, 229, requested)
        .expect("control-plane resize");
    assert_eq!(state.cnode_slot_capacity(app_cnode), Some(requested));
}

#[test]
fn non_system_server_control_plane_cannot_resize_other_process_cnode() {
    let mut state = Bootstrap::init().expect("init");
    state
        .register_task_with_class(230, TaskClass::App)
        .expect("app 1");
    state
        .register_task_with_class(231, TaskClass::App)
        .expect("app 2");
    assert_eq!(
        state.control_plane_set_process_cnode_slots(230, 231, 16),
        Err(KernelError::MissingRight)
    );
}

#[test]
fn cnode_slot_budget_rejects_overcommit() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    let limits = state.runtime_capacity_config();
    let mut saw_overcommit_rejection = false;

    for idx in 0..=limits.max_tasks {
        let cnode = CNodeId(10_000 + idx as u64);
        let result = state.ensure_cnode_space_with_slots(cnode, limits.driver_cnode_slot_capacity);
        if result == Err(KernelError::CapabilityFull) {
            saw_overcommit_rejection = true;
            break;
        }
        assert!(
            result.is_ok(),
            "unexpected cnode creation error: {result:?}"
        );
    }

    assert!(saw_overcommit_rejection);
}

#[test]
fn driver_process_can_resize_its_cnode_slots() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    state
        .register_task_with_class(222, TaskClass::Driver)
        .expect("driver task");
    let cnode = state.process_cnode_for_pid(222).expect("cnode");
    let before = state.cnode_slot_capacity(cnode).expect("capacity");
    assert!(before > 1);
    state
        .resize_process_cnode_slots(222, before - 1)
        .expect("resize down");
    assert_eq!(state.cnode_slot_capacity(cnode), Some(before - 1));
}

#[test]
fn app_process_cnode_resize_is_denied_by_policy() {
    let mut state = Bootstrap::init().expect("init");
    state
        .register_task_with_class(223, TaskClass::App)
        .expect("app task");
    assert_eq!(
        state.resize_process_cnode_slots(223, 2),
        Err(KernelError::MissingRight)
    );
}

#[test]
fn cnode_resize_rejects_shrink_below_occupied_slots() {
    let mut state =
        Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained).expect("init");
    state
        .register_task_with_class(224, TaskClass::Driver)
        .expect("driver task");
    let cnode = state.process_cnode_for_pid(224).expect("cnode");
    state
        .mint_capability_in_cnode(cnode, Capability::new(CapObject::Kernel, CapRights::READ))
        .expect("mint one");
    state
        .mint_capability_in_cnode(cnode, Capability::new(CapObject::Kernel, CapRights::READ))
        .expect("mint two");
    assert_eq!(
        state.resize_cnode_slots(cnode, 1),
        Err(KernelError::CapabilityFull)
    );
}

#[test]
fn synchronous_endpoint_blocked_send_updates_telemetry() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(62).expect("sender");

    let (_eid, send_cap, _recv_cap) = state
        .create_endpoint_with_mode(1, EndpointMode::Synchronous)
        .expect("endpoint");

    let msg = Message::new(62, b"blk").expect("msg");
    assert_eq!(state.ipc_send(send_cap, msg), Err(KernelError::WouldBlock));

    let t = state.ipc_path_telemetry();
    assert_eq!(t.blocked_sends, 1);
    assert_eq!(t.queued_sends, 0);
}

#[test]
fn ipc_fastpath_blocked_path_is_measured_without_switch() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(63).expect("sender");

    let (_eid, send_cap, _recv_cap) = state
        .create_endpoint_with_mode(1, EndpointMode::Synchronous)
        .expect("endpoint");

    let msg = Message::new(63, b"fp-block").expect("msg");
    assert_eq!(
        state.ipc_send_fastpath(send_cap, msg),
        Err(KernelError::WouldBlock)
    );

    let t = state.ipc_path_telemetry();
    assert_eq!(t.fastpath_attempts, 1);
    assert_eq!(t.fastpath_switches, 0);
    assert_eq!(t.blocked_sends, 1);
    assert_eq!(t.queued_sends, 0);
    assert_eq!(t.rendezvous_handoffs, 0);
    assert_eq!(t.scheduler_fastpath_handoffs, 0);
}

#[test]
fn ipc_fastpath_on_buffered_endpoint_queues_without_switch() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(64).expect("sender");

    let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");

    let msg = Message::new(64, b"fp-queued").expect("msg");
    let result = state.ipc_send_fastpath(send_cap, msg).expect("fastpath");
    assert!(!result.switched_to_waiter);

    let delivered = state.ipc_recv(recv_cap).expect("recv").expect("msg");
    assert_eq!(delivered.as_slice(), b"fp-queued");

    let t = state.ipc_path_telemetry();
    assert_eq!(t.fastpath_attempts, 1);
    assert_eq!(t.fastpath_switches, 0);
    assert_eq!(t.queued_sends, 1);
    assert_eq!(t.blocked_sends, 0);
    assert_eq!(t.scheduler_fastpath_handoffs, 0);
}

#[test]
fn delegate_driver_bundle_uses_standard_window_and_revokes_caps() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(59).expect("task");
    let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let iova_cap = state.create_iova_space_cap().expect("iova");

    let bundle = state
        .delegate_driver_bundle(DriverBundlePlan {
            server_tid: ThreadId(59),
            irq_line: 12,
            mem_cap,
            dma_len: crate::kernel::vm::PAGE_SIZE,
            iova_cap,
            iova_base: crate::kernel::vm::PAGE_SIZE * 2,
            iova_len: crate::kernel::vm::PAGE_SIZE,
        })
        .expect("bundle");

    let driver_cnode = state.task_cnode(59).expect("driver cnode");
    assert!(
        state
            .capability_for_cnode(driver_cnode, bundle.irq_cap)
            .is_some()
    );
    assert!(
        state
            .capability_for_cnode(driver_cnode, bundle.dma_cap)
            .is_some()
    );
    assert!(
        state
            .capability_for_cnode(driver_cnode, bundle.iova_cap)
            .is_some()
    );

    state.revoke_driver_runtime_caps(59).expect("revoke");
    assert!(
        state
            .capability_for_cnode(driver_cnode, bundle.irq_cap)
            .is_none()
    );
    assert!(
        state
            .capability_for_cnode(driver_cnode, bundle.dma_cap)
            .is_none()
    );
    assert!(
        state
            .capability_for_cnode(driver_cnode, bundle.iova_cap)
            .is_none()
    );
}

#[test]
fn rendezvous_delivery_is_single_copy_and_no_sender_stuck() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(80).expect("sender");
    state.register_task(81).expect("receiver");

    let (_eid, send_cap, recv_cap) = state
        .create_endpoint_with_mode(1, EndpointMode::Synchronous)
        .expect("endpoint");
    let recv_cap_task81 = state
        .grant_capability_task_to_task(0, recv_cap, 81)
        .expect("dup recv to task81");
    let send_cap_task80 = state
        .grant_capability_task_to_task(0, send_cap, 80)
        .expect("dup send to task80");

    state.enqueue_current_cpu(81).expect("enqueue receiver");
    state.yield_current().expect("run receiver");
    assert_eq!(state.current_tid(), Some(81));
    let mut recv_tf = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcRecv as usize,
        [recv_cap_task81.0 as usize, 8, 0x1100, 0, 0, 0],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut recv_tf))
        .expect("recv trap");

    state.enqueue_current_cpu(80).expect("enqueue sender");
    state.yield_current().expect("run sender");
    assert_eq!(state.current_tid(), Some(80));
    state
        .ipc_send(send_cap_task80, Message::new(80, b"rv").expect("msg"))
        .expect("send");

    let delivered = state.ipc_recv(recv_cap_task81).expect("recv").expect("msg");
    assert_eq!(delivered.as_slice(), b"rv");
    assert!(state.ipc_recv(recv_cap_task81).expect("recv2").is_none());
    assert!(matches!(
        state.task_status(80),
        Some(TaskStatus::Runnable | TaskStatus::Running)
    ));

    let t = state.ipc_path_telemetry();
    assert!(t.rendezvous_handoffs >= 1);
    assert!(t.fastpath_attempts >= t.fastpath_switches);
}

#[test]
fn ipc_send_fastpath_detects_waiter() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(35).expect("sender");
    state.register_task(36).expect("receiver");

    let (_eid, send_cap, recv_cap) = state
        .create_endpoint_with_mode(1, EndpointMode::Synchronous)
        .expect("endpoint");
    let recv_cap_task36 = state
        .grant_capability_task_to_task(0, recv_cap, 36)
        .expect("dup recv to task36");
    let send_cap_task35 = state
        .grant_capability_task_to_task(0, send_cap, 35)
        .expect("dup send to task35");

    state.enqueue_current_cpu(36).expect("enqueue receiver");
    state.yield_current().expect("run receiver");
    assert_eq!(state.current_tid(), Some(36));
    let mut recv_tf = TrapFrame::new(
        crate::kernel::syscall::Syscall::IpcRecv as usize,
        [recv_cap_task36.0 as usize, 8, 0x7000, 0, 0, 0],
    );
    state
        .handle_trap(Trap::Syscall, Some(&mut recv_tf))
        .expect("recv trap");

    state.enqueue_current_cpu(35).expect("enqueue sender");
    state.yield_current().expect("run sender");
    assert_eq!(state.current_tid(), Some(35));
    let msg = Message::new(35, b"x").expect("msg");
    let result = state
        .ipc_send_fastpath(send_cap_task35, msg)
        .expect("fastpath");
    assert!(result.switched_to_waiter);
}

#[test]
fn driver_registration_and_capability_grants_work() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(3).expect("task");
    state.register_driver(3).expect("driver");

    let irq_cap = state.mint_irq_cap(9).expect("irq");
    state.grant_driver_irq(3, irq_cap).expect("grant irq");

    let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let dma_cap = state
        .mint_dma_region_cap(mem_cap, 0, crate::kernel::vm::PAGE_SIZE)
        .expect("dma");
    state.grant_driver_dma(3, dma_cap).expect("grant dma");
}

#[test]
fn driver_record_accepts_multiple_irq_and_dma_caps() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(44).expect("task");
    state.register_driver(44).expect("driver");

    let irq_a = state.mint_irq_cap(10).expect("irq a");
    let irq_b = state.mint_irq_cap(11).expect("irq b");
    let delegated_irq_a = state.grant_driver_irq(44, irq_a).expect("grant irq a");
    let delegated_irq_b = state.grant_driver_irq(44, irq_b).expect("grant irq b");

    let (_id_a, mem_a) = state.alloc_anonymous_memory_object().expect("mem a");
    let (_id_b, mem_b) = state.alloc_anonymous_memory_object().expect("mem b");
    let dma_a = state
        .mint_dma_region_cap(mem_a, 0, crate::kernel::vm::PAGE_SIZE)
        .expect("dma a");
    let dma_b = state
        .mint_dma_region_cap(mem_b, 0, crate::kernel::vm::PAGE_SIZE)
        .expect("dma b");
    let delegated_dma_a = state.grant_driver_dma(44, dma_a).expect("grant dma a");
    let delegated_dma_b = state.grant_driver_dma(44, dma_b).expect("grant dma b");

    state.revoke_driver_runtime_caps(44).expect("revoke");
    let driver_cnode = state.task_cnode(44).expect("driver cnode");
    assert!(
        state
            .capability_for_cnode(driver_cnode, delegated_irq_a)
            .is_none()
    );
    assert!(
        state
            .capability_for_cnode(driver_cnode, delegated_irq_b)
            .is_none()
    );
    assert!(
        state
            .capability_for_cnode(driver_cnode, delegated_dma_a)
            .is_none()
    );
    assert!(
        state
            .capability_for_cnode(driver_cnode, delegated_dma_b)
            .is_none()
    );
}

#[test]
fn supervisor_receives_task_exit_report() {
    let mut state = Bootstrap::init().expect("init");
    let (_e, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
    state
        .set_supervisor_endpoint(recv_cap)
        .expect("supervisor ep");
    state
        .report_task_exit_to_supervisor(7, 99, 55)
        .expect("report exit");

    let msg = state.ipc_recv(recv_cap).expect("recv").expect("msg");
    assert_eq!(msg.opcode, 0xEE);
    assert_eq!(msg.as_slice().len(), 24);
    let event =
        yarm_ipc_abi::supervisor_abi::TaskExitedEvent::decode(msg.as_slice()).expect("event");
    assert_eq!(event.tid, 7);
    assert_eq!(event.exit_code, 99);
    assert_eq!(event.restart_token, 55);
    assert_eq!(
        state
            .ipc_send(send_cap, Message::new(0, b"ok").expect("m"))
            .is_ok(),
        true
    );
}

#[test]
fn supervisor_receives_transfer_revoke_report() {
    let mut state = Bootstrap::init().expect("init");
    let (_e, _send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
    state
        .set_supervisor_endpoint(recv_cap)
        .expect("supervisor ep");
    state
        .report_transfer_revoke_to_supervisor(7, 12, 0xA000, crate::kernel::vm::PAGE_SIZE as u64)
        .expect("report revoke");

    let msg = state.ipc_recv(recv_cap).expect("recv").expect("msg");
    assert_eq!(
        msg.opcode,
        yarm_ipc_abi::supervisor_abi::SUPERVISOR_OP_TRANSFER_REVOKED
    );
    assert_eq!(msg.as_slice().len(), 32);
    let event =
        yarm_ipc_abi::supervisor_abi::TransferRevokedEvent::decode(msg.as_slice()).expect("event");
    assert_eq!(event.owner_pid, 7);
    assert_eq!(event.cap, 12);
    assert_eq!(event.base, 0xA000);
    assert_eq!(event.len, crate::kernel::vm::PAGE_SIZE as u64);
}

#[test]
fn exited_task_can_restart_with_token_and_then_be_marked_dead() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(9).expect("task");
    let token = state.exit_task(9, 12).expect("exit");
    assert_eq!(state.task_status(9), Some(TaskStatus::Exited(12)));

    assert!(state.restart_task(9, token).is_ok());
    assert_eq!(state.task_status(9), Some(TaskStatus::Runnable));

    state.mark_task_dead(9).expect("dead");
    assert_eq!(state.task_status(9), Some(TaskStatus::Dead));
}

#[test]
fn dma_region_cap_enforces_window_constraints() {
    let mut state = Bootstrap::init().expect("init");
    let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");

    assert!(
        state
            .mint_dma_region_cap(mem_cap, 0, crate::kernel::vm::PAGE_SIZE)
            .is_ok()
    );
    assert!(
        state
            .mint_dma_region_cap(mem_cap, 1, crate::kernel::vm::PAGE_SIZE)
            .is_err()
    );
    assert!(state.mint_dma_region_cap(mem_cap, 0, 0).is_err());
    assert!(
        state
            .mint_dma_region_cap(mem_cap, 0, crate::kernel::vm::PAGE_SIZE * 2)
            .is_err()
    );
}

#[test]
fn dma_region_cap_uses_parent_memory_object_length() {
    let mut state = Bootstrap::init().expect("init");
    let (id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");

    let entry = state
        .memory
        .memory_objects
        .iter_mut()
        .flatten()
        .find(|entry| entry.id == id)
        .expect("memory object present");
    entry.len = crate::kernel::vm::PAGE_SIZE * 4;

    assert!(
        state
            .mint_dma_region_cap(mem_cap, 0, crate::kernel::vm::PAGE_SIZE * 2)
            .is_ok()
    );
    assert!(
        state
            .mint_dma_region_cap(
                mem_cap,
                crate::kernel::vm::PAGE_SIZE * 3,
                crate::kernel::vm::PAGE_SIZE
            )
            .is_ok()
    );
    assert!(
        state
            .mint_dma_region_cap(
                mem_cap,
                crate::kernel::vm::PAGE_SIZE * 3,
                crate::kernel::vm::PAGE_SIZE * 2
            )
            .is_err()
    );
}

#[test]
fn deterministic_mixed_stress_sequence_is_stable() {
    let mut state = Bootstrap::init().expect("init");
    state.bring_up_cpu(CpuId(1)).expect("cpu1");
    let (_nidx, ncap, nrecv) = state.create_notification(8).expect("notif");
    state.bind_irq_notification(5, ncap).expect("bind irq");

    for i in 1..=10u64 {
        state.register_task(i).expect("task");
        state
            .enqueue_on_cpu(CpuId((i % 2) as u8), i)
            .expect("enqueue");
    }

    for _ in 0..8 {
        state
            .submit_cross_cpu_work(CpuId(1), WorkItem::Reschedule)
            .expect("work");
    }
    state
        .process_cross_cpu_work_for_cpu(CpuId(1))
        .expect("process");

    for _ in 0..5 {
        state
            .handle_trap_event(TrapEvent::ExternalInterrupt(5), None)
            .expect("irq");
    }

    let mut irq_msgs = 0usize;
    while state.ipc_recv(nrecv).expect("recv").is_some() {
        irq_msgs += 1;
        if irq_msgs > 16 {
            break;
        }
    }
    assert_eq!(irq_msgs, 5);
    assert_eq!(state.online_cpu_count(), 2);
}

#[test]
fn lock_order_snapshot_reads_scheduler_then_ipc_domains() {
    let mut state = Bootstrap::init().expect("init");
    state.bring_up_cpu(CpuId(1)).expect("cpu1");
    let (_eid, send_cap, _recv_cap) = state.create_endpoint(2).expect("ep");
    state
        .ipc_send(send_cap, Message::new(1, b"ok").expect("msg"))
        .expect("send");

    let (cpu, online, dispatch_calls) = state.lock_order_snapshot_for_test();
    assert_eq!(cpu, 0);
    assert!(online >= 1);
    assert_eq!(dispatch_calls, 1);
}

#[test]
fn lock_order_snapshot_reads_task_then_capability_domains() {
    let mut state = Bootstrap::init().expect("init");
    state
        .register_task_with_class(33, TaskClass::App)
        .expect("task");
    let cnode = CNodeId(33);
    state.ensure_cnode_space(cnode).expect("cnode");
    state
        .set_process_cnode_for_pid(33, cnode)
        .expect("process cnode");

    let (tasks, process_cnodes) = state.lock_order_task_capability_snapshot_for_test();
    assert!(tasks >= 2);
    assert!(process_cnodes >= 2);
}

#[test]
fn driver_restart_revokes_runtime_caps() {
    let mut state = Bootstrap::init().expect("init");
    state
        .register_task_with_class(22, TaskClass::Driver)
        .expect("task");
    state.register_driver(22).expect("driver");

    let irq_cap = state.mint_irq_cap(3).expect("irq");
    state.grant_driver_irq(22, irq_cap).expect("grant irq");

    let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let dma_cap = state
        .mint_dma_region_cap(mem_cap, 0, crate::kernel::vm::PAGE_SIZE)
        .expect("dma");
    state.grant_driver_dma(22, dma_cap).expect("grant dma");

    let iova_cap = state.create_iova_space_cap().expect("iova");
    state
        .grant_driver_iova_space(22, iova_cap)
        .expect("grant iova");
    state
        .configure_driver_dma_window(
            22,
            crate::kernel::vm::PAGE_SIZE * 8,
            crate::kernel::vm::PAGE_SIZE,
        )
        .expect("window");

    let token = state.exit_task(22, 1).expect("exit");
    state.restart_task(22, token).expect("restart");

    assert!(
        state
            .validate_driver_dma_iova(
                22,
                crate::kernel::vm::PAGE_SIZE * 8,
                crate::kernel::vm::PAGE_SIZE
            )
            .is_err()
    );
}

#[test]
fn driver_tasks_pin_to_first_enqueue_cpu() {
    let mut state = Bootstrap::init().expect("init");
    state.bring_up_cpu(CpuId(1)).expect("cpu1");
    state
        .register_task_with_class(71, TaskClass::Driver)
        .expect("driver");

    state.set_current_cpu(CpuId(0)).expect("cpu0");
    assert_eq!(state.enqueue_task(71).expect("enqueue first"), CpuId(0));
    assert!(state.runnable_count_on_for_test(CpuId(0)) >= 1);
    for _ in 0..4 {
        if state.current_tid() == Some(71) {
            break;
        }
        let _ = state.on_preempt_current_cpu();
    }
    if state.current_tid() == Some(71) {
        let _ = state.block_current_cpu();
    }

    state.set_current_cpu(CpuId(1)).expect("cpu1");
    assert_eq!(state.enqueue_task(71).expect("enqueue second"), CpuId(0));
    assert_eq!(state.runnable_count_on_for_test(CpuId(1)), 0);
    assert!(state.runnable_count_on_for_test(CpuId(0)) >= 1);

    state.set_current_cpu(CpuId(0)).expect("cpu0");
    assert_ne!(state.dispatch_next_current_cpu(), None);
}

#[test]
fn detach_iova_space_revokes_dma_window_validation() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(31).expect("task");
    state.register_driver(31).expect("driver");

    let iova = state.create_iova_space_cap().expect("iova");
    state.grant_driver_iova_space(31, iova).expect("grant");
    state
        .configure_driver_dma_window(
            31,
            crate::kernel::vm::PAGE_SIZE * 2,
            crate::kernel::vm::PAGE_SIZE,
        )
        .expect("window");
    assert!(
        state
            .validate_driver_dma_iova(
                31,
                crate::kernel::vm::PAGE_SIZE * 2,
                crate::kernel::vm::PAGE_SIZE
            )
            .is_ok()
    );

    state.detach_driver_iova_space(31).expect("detach");
    assert!(
        state
            .validate_driver_dma_iova(
                31,
                crate::kernel::vm::PAGE_SIZE * 2,
                crate::kernel::vm::PAGE_SIZE
            )
            .is_err()
    );
}

#[test]
fn revoke_driver_runtime_caps_revokes_from_driver_cnode() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(32).expect("task");
    state.register_driver(32).expect("driver");

    let irq = state.mint_irq_cap(4).expect("irq");
    let delegated_irq = state.grant_driver_irq(32, irq).expect("grant irq");

    let (_id, mem) = state.alloc_anonymous_memory_object().expect("mem");
    let dma = state
        .mint_dma_region_cap(mem, 0, crate::kernel::vm::PAGE_SIZE)
        .expect("dma");
    let delegated_dma = state.grant_driver_dma(32, dma).expect("grant dma");

    let iova = state.create_iova_space_cap().expect("iova");
    let delegated_iova = state.grant_driver_iova_space(32, iova).expect("grant iova");

    state.revoke_driver_runtime_caps(32).expect("revoke");
    let driver_cnode = state.task_cnode(32).expect("driver cnode");
    assert!(
        state
            .capability_for_cnode(driver_cnode, delegated_irq)
            .is_none()
    );
    assert!(
        state
            .capability_for_cnode(driver_cnode, delegated_dma)
            .is_none()
    );
    assert!(
        state
            .capability_for_cnode(driver_cnode, delegated_iova)
            .is_none()
    );
}

#[test]
fn stale_driver_caps_are_rejected_after_revocation() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(33).expect("task");
    state.register_driver(33).expect("driver");

    let irq = state.mint_irq_cap(8).expect("irq");
    let delegated_irq = state.grant_driver_irq(33, irq).expect("grant irq");
    state.revoke_driver_runtime_caps(33).expect("revoke");

    let driver_cnode = state.task_cnode(33).expect("driver cnode");
    assert!(
        state
            .capability_for_cnode(driver_cnode, delegated_irq)
            .is_none()
    );
    assert!(state.grant_driver_irq(33, irq).is_ok());
}

#[test]
fn delegation_checked_bundle_requires_redelegation_after_driver_restart() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(110).expect("init-task");
    state.register_task(111).expect("driver-task");

    let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let iova_cap = state.create_iova_space_cap().expect("iova");

    let first_bundle = state
        .delegate_driver_bundle(DriverBundlePlan::standard(
            ThreadId(111),
            14,
            mem_cap,
            crate::kernel::vm::PAGE_SIZE,
            iova_cap,
            crate::kernel::vm::PAGE_SIZE * 4,
            crate::kernel::vm::PAGE_SIZE * 4,
        ))
        .expect("first bundle");
    state
        .validate_driver_bundle_live(111, first_bundle)
        .expect("bundle live");
    let driver_cnode = state.task_cnode(111).expect("driver cnode");
    assert!(
        state
            .capability_for_cnode(driver_cnode, first_bundle.irq_cap)
            .is_some()
    );
    assert!(
        state
            .capability_for_cnode(driver_cnode, first_bundle.dma_cap)
            .is_some()
    );

    let token = state.exit_task(111, 5).expect("exit");
    state.restart_task(111, token).expect("restart");

    assert_eq!(
        state.validate_driver_bundle_live(111, first_bundle),
        Err(KernelError::StaleCapability)
    );
    let driver_cnode = state.task_cnode(111).expect("driver cnode");
    assert!(
        state
            .capability_for_cnode(driver_cnode, first_bundle.irq_cap)
            .is_none()
    );
    assert!(
        state
            .capability_for_cnode(driver_cnode, first_bundle.dma_cap)
            .is_none()
    );
    assert!(matches!(
        state.grant_driver_irq(111, first_bundle.irq_cap),
        Err(KernelError::InvalidCapability | KernelError::WrongObject)
    ));

    assert!(
        state
            .capability_for_cnode(driver_cnode, first_bundle.iova_cap)
            .is_none()
    );
    let iova_cap2 = state.create_iova_space_cap().expect("iova2");

    let second_bundle = state
        .delegate_driver_bundle(DriverBundlePlan::standard(
            ThreadId(111),
            14,
            mem_cap,
            crate::kernel::vm::PAGE_SIZE,
            iova_cap2,
            crate::kernel::vm::PAGE_SIZE * 4,
            crate::kernel::vm::PAGE_SIZE * 2,
        ))
        .expect("second bundle");
    state
        .validate_driver_bundle_live(111, second_bundle)
        .expect("bundle live after redelegation");

    assert_ne!(first_bundle.irq_cap, second_bundle.irq_cap);
    assert_ne!(first_bundle.dma_cap, second_bundle.dma_cap);
    let driver_cnode = state.task_cnode(111).expect("driver cnode");
    assert!(
        state
            .capability_for_cnode(driver_cnode, second_bundle.irq_cap)
            .is_some()
    );
    assert!(
        state
            .capability_for_cnode(driver_cnode, second_bundle.dma_cap)
            .is_some()
    );
}

#[test]
fn checked_bundle_helper_validates_bundle_and_dma_window() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(120).expect("init-task");
    state.register_task(121).expect("driver-task");

    let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let iova_cap = state.create_iova_space_cap().expect("iova");
    let plan = DriverBundlePlan::standard(
        ThreadId(121),
        16,
        mem_cap,
        crate::kernel::vm::PAGE_SIZE,
        iova_cap,
        crate::kernel::vm::PAGE_SIZE * 8,
        crate::kernel::vm::PAGE_SIZE * 8,
    );
    let bundle = state
        .delegate_driver_bundle_checked(plan)
        .expect("checked bundle");
    state
        .validate_driver_bundle_live(121, bundle)
        .expect("bundle live");
    assert!(
        state
            .validate_driver_dma_iova(
                121,
                crate::kernel::vm::PAGE_SIZE * 8,
                crate::kernel::vm::PAGE_SIZE
            )
            .is_ok()
    );
}

#[test]
fn redelegate_bundle_helper_revokes_old_caps_and_rejects_stale_bundle() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(130).expect("init-task");
    state.register_task(131).expect("driver-task");

    let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let first_iova = state.create_iova_space_cap().expect("iova1");
    let first_plan = DriverBundlePlan::standard(
        ThreadId(131),
        17,
        mem_cap,
        crate::kernel::vm::PAGE_SIZE,
        first_iova,
        crate::kernel::vm::PAGE_SIZE * 4,
        crate::kernel::vm::PAGE_SIZE * 4,
    );
    let first_bundle = state
        .delegate_driver_bundle_checked(first_plan)
        .expect("first bundle");
    state
        .validate_driver_bundle_live(131, first_bundle)
        .expect("first live");

    let second_iova = state.create_iova_space_cap().expect("iova2");
    let second_plan = DriverBundlePlan::standard(
        ThreadId(131),
        18,
        mem_cap,
        crate::kernel::vm::PAGE_SIZE,
        second_iova,
        crate::kernel::vm::PAGE_SIZE * 12,
        crate::kernel::vm::PAGE_SIZE * 4,
    );
    let second_bundle = state
        .redelegate_driver_bundle(second_plan)
        .expect("second bundle");
    assert_eq!(
        state.validate_driver_bundle_live(131, first_bundle),
        Err(KernelError::StaleCapability)
    );
    state
        .validate_driver_bundle_live(131, second_bundle)
        .expect("second live");
}

#[test]
fn iova_window_validation_requires_iova_space_and_range() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(12).expect("task");
    state.register_driver(12).expect("driver");

    let iova_cap = state.create_iova_space_cap().expect("iova");
    state
        .grant_driver_iova_space(12, iova_cap)
        .expect("grant iova");
    state
        .configure_driver_dma_window(
            12,
            crate::kernel::vm::PAGE_SIZE * 4,
            crate::kernel::vm::PAGE_SIZE,
        )
        .expect("window");

    assert!(
        state
            .validate_driver_dma_iova(
                12,
                crate::kernel::vm::PAGE_SIZE * 4,
                crate::kernel::vm::PAGE_SIZE
            )
            .is_ok()
    );
    assert!(
        state
            .validate_driver_dma_iova(
                12,
                crate::kernel::vm::PAGE_SIZE * 3,
                crate::kernel::vm::PAGE_SIZE
            )
            .is_err()
    );
}

#[test]
fn long_run_multi_core_simulation_is_deterministic() {
    let mut state = Bootstrap::init().expect("init");
    state.bring_up_cpu(CpuId(1)).expect("cpu1");
    let (_nidx, ncap, nrecv) = state.create_notification(64).expect("notif");
    state.bind_irq_notification(7, ncap).expect("bind");

    for i in 1..=20u64 {
        state.register_task(i).expect("task");
        state
            .enqueue_on_cpu(CpuId((i % 2) as u8), i)
            .expect("enqueue");
    }

    let mut seed = 0x1234_5678u64;
    for _ in 0..500 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        match seed % 3 {
            0 => state
                .submit_cross_cpu_work(CpuId((seed as u8) % 2), WorkItem::Reschedule)
                .expect("work"),
            1 => {
                if state
                    .handle_trap_event(TrapEvent::ExternalInterrupt(7), None)
                    .is_err()
                {
                    let _ = state.ipc_recv(nrecv);
                }
            }
            _ => {
                let cpu = CpuId((seed as u8) % 2);
                state.process_cross_cpu_work_for_cpu(cpu).expect("process");
            }
        }
    }

    let mut seen = 0usize;
    while state.ipc_recv(nrecv).expect("recv").is_some() {
        seen += 1;
        if seen > 2048 {
            break;
        }
    }
    assert!(seen > 0);
    assert_eq!(state.online_cpu_count(), 2);
}

#[test]
fn yield_current_rotates_to_next_runnable_task() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(40).expect("task");
    state.enqueue_current_cpu(40).expect("enqueue");

    assert_eq!(state.current_tid(), Some(0));
    state.yield_current().expect("yield");

    assert_eq!(state.current_tid(), Some(40));
    assert_eq!(state.task_status(40), Some(TaskStatus::Running));
    assert_eq!(state.task_status(0), Some(TaskStatus::Runnable));
}

#[test]
fn trap_event_page_fault_records_fault_then_faults_current_task() {
    let mut state = Bootstrap::init().expect("init");
    let fault = FaultInfo {
        addr: VirtAddr(0x4000),
        access: FaultAccess::Execute,
    };

    state
        .handle_trap_event(TrapEvent::PageFault(fault), None)
        .expect("handle page fault");

    assert_eq!(state.last_fault(), Some(fault));
    assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
}

#[test]
fn demand_page_fault_maps_heap_page_for_current_task() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(0, asid).expect("bind");
    state
        .set_task_brk_bounds(0, 0x4000, 0x8000)
        .expect("brk bounds");

    let fault = FaultInfo {
        addr: VirtAddr(0x5001),
        access: FaultAccess::Write,
    };
    state
        .handle_trap_event(TrapEvent::PageFault(fault), None)
        .expect("demand page fault");

    let mapping = state
        .user_spaces
        .get(asid)
        .expect("aspace")
        .resolve(VirtAddr(0x5000))
        .expect("mapped");
    assert!(mapping.flags.user);
    assert!(mapping.flags.read);
    assert!(mapping.flags.write);
    assert_eq!(state.task_status(0), Some(TaskStatus::Running));
}

#[test]
fn page_fault_outside_demand_regions_still_faults_task() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state.bind_task_asid(0, asid).expect("bind");

    let fault = FaultInfo {
        addr: VirtAddr(0x9000),
        access: FaultAccess::Read,
    };
    state
        .handle_trap_event(TrapEvent::PageFault(fault), None)
        .expect("page fault handled");

    assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
}

#[test]
fn cross_cpu_work_for_other_cpu_is_deferred_not_dropped() {
    let mut state = Bootstrap::init().expect("init");
    state.bring_up_cpu(CpuId(1)).expect("cpu1");

    state
        .submit_cross_cpu_work(CpuId(1), WorkItem::Reschedule)
        .expect("submit");

    let processed_cpu0 = state
        .process_cross_cpu_work_for_cpu(CpuId(0))
        .expect("process cpu0");
    assert_eq!(processed_cpu0, 0);

    let processed_cpu1 = state
        .process_cross_cpu_work_for_cpu(CpuId(1))
        .expect("process cpu1");
    assert_eq!(processed_cpu1, 1);
}

#[test]
fn spawn_user_thread_inherits_group_and_asid_and_sets_tls() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 7,
            entry: 0x4000,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("parent");

    let tid = state
        .spawn_user_thread(7, 0xDEAD_BEEF, 0x8000_0000, 0x4010)
        .expect("thread");

    assert_eq!(state.task_cnode(tid), state.task_cnode(7));
    assert_eq!(state.thread_group_id(tid), Some(ThreadGroupId(7)));
    assert_eq!(state.task_asid(tid), Some(asid));
    assert_eq!(state.thread_tls_base(tid), Some(0xDEAD_BEEF));
    assert_eq!(state.task_status(tid), Some(TaskStatus::Runnable));
}

#[test]
fn spawn_user_thread_rejects_misaligned_stack_top() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 8,
            entry: 0x4000,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("parent");

    assert_eq!(
        state.spawn_user_thread(8, 0xDEAD_BEEF, 0x8000_0008, 0x4010),
        Err(KernelError::WrongObject)
    );
}

#[test]
fn futex_wait_blocks_current_and_wake_requeues_waiter() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 1,
            entry: 0x4000,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("task1");
    let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    state
        .map_user_page_with_caps(
            aspace_cap,
            mem_cap,
            VirtAddr(0x1000),
            PageFlags {
                read: true,
                write: true,
                execute: false,
                user: true,
                cache_policy: CachePolicy::WriteBack,
            },
        )
        .expect("map");
    assert_eq!(state.current_tid(), Some(1));

    assert!(state.futex_wait_current(0x1000, 3, 3).expect("wait"));
    assert_eq!(
        state.task_status(1),
        Some(TaskStatus::Blocked(WaitReason::Futex(VirtAddr(0x1000))))
    );
    assert_eq!(state.futex_wake(0x1000, 1).expect("wake"), 1);
    assert_eq!(state.task_status(1), Some(TaskStatus::Runnable));
}

#[test]
fn futex_wait_and_wake_reject_kernel_space_address() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 2,
            entry: 0x4000,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("task2");
    let kernel_addr = crate::kernel::vm::KERNEL_SPACE_BASE as usize;
    assert_eq!(
        state
            .futex_wait_current(kernel_addr, 1, 1)
            .expect_err("kernel va rejected"),
        KernelError::UserMemoryFault
    );
    assert_eq!(
        state
            .futex_wake(kernel_addr, 1)
            .expect_err("kernel va rejected"),
        KernelError::UserMemoryFault
    );
}

#[test]
fn fork_child_preserves_parent_registers_except_arg0() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 33,
            entry: 0x8000,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("parent");
    let parent_ctx = UserRegisterContext {
        instruction_ptr: VirtAddr(0x8123),
        stack_ptr: VirtAddr(0x8FFF_0000),
        user_gprs: [0; 32],
        arg0: 0xAAAA,
        arg1: 0x1111,
        arg2: 0x2222,
        arg3: 0x3333,
        arg4: 0x4444,
        arg5: 0x5555,
    };
    state
        .set_thread_user_context(33, parent_ctx)
        .expect("set parent ctx");

    let child_tid = state.fork_user_process_cow(33).expect("fork");
    let child_ctx = state
        .thread_user_context(child_tid)
        .expect("child user context");

    assert_eq!(child_ctx.instruction_ptr, parent_ctx.instruction_ptr);
    assert_eq!(child_ctx.stack_ptr, parent_ctx.stack_ptr);
    assert_eq!(child_ctx.arg0, 0);
    assert_eq!(child_ctx.arg1, parent_ctx.arg1);
    assert_eq!(child_ctx.arg2, parent_ctx.arg2);
    assert_eq!(child_ctx.arg3, parent_ctx.arg3);
    assert_eq!(child_ctx.arg4, parent_ctx.arg4);
    assert_eq!(child_ctx.arg5, parent_ctx.arg5);
}

#[test]
fn fork_child_sets_tls_restore_pending_when_tls_present() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 34,
            entry: 0x8200,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("parent");
    state
        .set_thread_tls_base(34, 0xABCD_0000)
        .expect("set parent tls");

    let child_tid = state.fork_user_process_cow(34).expect("fork");
    assert_eq!(state.thread_tls_base(child_tid), Some(0xABCD_0000));
    assert_eq!(state.tls_restore_pending(child_tid), Some(true));
}

#[test]
fn fork_child_starts_with_empty_robust_futex_state() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 35,
            entry: 0x8300,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("parent");
    state
        .set_robust_futex_head(35, 0x5000, 8)
        .expect("parent robust futex");

    let child_tid = state.fork_user_process_cow(35).expect("fork");
    assert!(state.robust_futex_state(35).is_some());
    assert_eq!(state.robust_futex_state(child_tid), None);
}

#[test]
fn fork_child_inherits_brk_bounds() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 37,
            entry: 0x8400,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("parent");
    state
        .set_task_brk_bounds(37, 0x5000, 0x9000)
        .expect("set parent brk");
    let child_tid = state.fork_user_process_cow(37).expect("fork");
    assert_eq!(state.task_brk_bounds(child_tid), Some((0x5000, 0x9000)));
}

#[test]
fn fork_child_inherits_parent_endpoint_caps_with_same_rights() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 39,
            entry: 0x8600,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("parent");
    let (_eid, send_root, recv_root) = state.create_endpoint(2).expect("endpoint");
    let send_parent = state
        .grant_capability_task_to_task_with_rights(0, send_root, 39, CapRights::SEND)
        .expect("grant send");
    let recv_parent = state
        .grant_capability_task_to_task_with_rights(0, recv_root, 39, CapRights::RECEIVE)
        .expect("grant recv");

    let child_tid = state.fork_user_process_cow(39).expect("fork");
    let child_caps = state
        .snapshot_live_capabilities_for_task(child_tid)
        .expect("child caps");
    assert!(child_caps.iter().any(|(_id, cap)| {
        matches!(cap.object, CapObject::Endpoint { .. }) && cap.has_right(CapRights::SEND)
    }));
    assert!(child_caps.iter().any(|(_id, cap)| {
        matches!(cap.object, CapObject::Endpoint { .. }) && cap.has_right(CapRights::RECEIVE)
    }));
    let child_cnode = state.task_cnode(child_tid).expect("child cnode");
    let inherited_send = child_caps
        .iter()
        .find(|(_id, cap)| matches!(cap.object, CapObject::Endpoint { .. }) && cap.has_right(CapRights::SEND))
        .map(|(id, _)| *id)
        .expect("send cap");
    let inherited_recv = child_caps
        .iter()
        .find(|(_id, cap)| matches!(cap.object, CapObject::Endpoint { .. }) && cap.has_right(CapRights::RECEIVE))
        .map(|(id, _)| *id)
        .expect("recv cap");
    assert!(state.capability_for_cnode(child_cnode, inherited_send).is_some());
    assert!(state.capability_for_cnode(child_cnode, inherited_recv).is_some());
    let parent_cnode = state.task_cnode(39).expect("parent cnode");
    assert!(state.capability_for_cnode(parent_cnode, send_parent).is_some());
    assert!(state.capability_for_cnode(parent_cnode, recv_parent).is_some());
}

#[test]
fn fork_child_does_not_inherit_kernel_caps() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 40,
            entry: 0x8700,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("parent");
    let parent_cnode = state.task_cnode(40).expect("parent cnode");
    let kernel_cap = state
        .mint_capability_in_cnode(parent_cnode, Capability::new(CapObject::Kernel, CapRights::READ))
        .expect("mint kernel cap");

    let child_tid = state.fork_user_process_cow(40).expect("fork");
    let child_caps = state
        .snapshot_live_capabilities_for_task(child_tid)
        .expect("child caps");
    assert!(!child_caps
        .iter()
        .any(|(_id, cap)| matches!(cap.object, CapObject::Kernel)));
    let child_cnode = state.task_cnode(child_tid).expect("child cnode");
    assert!(state.capability_for_cnode(child_cnode, kernel_cap).is_none());
}

#[test]
fn spawn_thread_does_not_get_independent_brk_bounds() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 38,
            entry: 0x8500,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("leader");
    state
        .set_task_brk_bounds(38, 0x6000, 0xA000)
        .expect("leader brk");
    let thread_tid = state
        .spawn_user_thread(38, 0xABCD_0000, 0x8800_0000, 0x8510)
        .expect("thread");
    assert_eq!(state.task_brk_bounds(thread_tid), None);
}

#[test]
fn clone_user_address_space_cow_cleans_child_state_on_cow_capacity_exhaustion() {
    let mut state = Bootstrap::init().expect("init");
    let (parent_asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 36,
            entry: 0x8400,
            asid: Some(parent_asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("parent");

    let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
    let phys = state
        .resolve_memory_object_phys(mem_cap, PageFlags::USER_RW)
        .expect("phys");
    let writable_pages = (super::MAX_COW_PAGES / 2) + 1;
    for page in 0..writable_pages {
        let va = VirtAddr(0x20_0000 + (page * PAGE_SIZE) as u64);
        state
            .map_user_page_in_asid_raw(
                parent_asid,
                va,
                Mapping {
                    phys,
                    flags: PageFlags::USER_RW,
                },
            )
            .expect("map parent page");
    }

    assert_eq!(
        state.clone_user_address_space_cow(parent_asid),
        Err(KernelError::MemoryObjectFull)
    );

    let lingering_child_cow = state.with_memory_state(|memory| {
        memory
            .cow_pages
            .iter()
            .flatten()
            .any(|entry| entry.asid != parent_asid)
    });
    assert!(!lingering_child_cow);
}

#[test]
fn trap_frame_resume_and_tls_request_are_consumed_for_current_thread() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 20,
            entry: 0x7000,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("leader");
    let tid = state
        .spawn_user_thread(20, 0xABCD_0000, 0x8800_0000, 0x7010)
        .expect("thread");
    state.yield_current().expect("switch");
    assert_eq!(state.current_tid(), Some(tid));

    let mut frame = TrapFrame::new(0, [11, 22, 0, 0, 0, 0]);
    let tls = state
        .resume_current_thread_with_frame(&mut frame)
        .expect("resume");
    assert_eq!(tls, Some(0xABCD_0000));
    assert_eq!(frame.saved_pc(), 0x7010);
    assert_eq!(frame.saved_sp(), 0x8800_0000);

    frame.set_saved_pc(0x9000);
    frame.set_saved_sp(0x9900_0000);
    frame.set_arg(0, 33);
    frame.set_arg(1, 44);
    state
        .sync_current_thread_from_frame(&frame)
        .expect("capture");
    assert_eq!(
        state.thread_user_context(tid),
        Some(UserRegisterContext {
            instruction_ptr: VirtAddr(0x9000),
            stack_ptr: VirtAddr(0x9900_0000),
            user_gprs: [0; 32],
            arg0: 33,
            arg1: 44,
            arg2: 0,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        })
    );
}

#[test]
fn kernel_switch_frame_can_be_initialized_for_thread() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(55).expect("task");

    state
        .set_thread_kernel_stack(55, 0x9000_0000, 0x9000_4000)
        .expect("set stack");
    state
        .initialize_thread_kernel_switch_frame(55, 0x1234_5678)
        .expect("init frame");

    let context = state.thread_kernel_context(55).expect("context");
    assert_eq!(context.stack_base, Some(VirtAddr(0x9000_0000)));
    assert_eq!(context.stack_top, Some(VirtAddr(0x9000_4000)));
    assert_eq!(context.frame.instruction_ptr(), 0x1234_5678);
    assert_eq!(context.frame.stack_ptr() & 0xF, 0);
    assert!(context.initialized);
}

#[test]
fn kernel_stack_configuration_rejects_invalid_bounds() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(56).expect("task");

    assert_eq!(
        state.set_thread_kernel_stack(56, 0x1000, 0x1000),
        Err(KernelError::WrongObject)
    );
    assert_eq!(
        state.initialize_thread_kernel_switch_frame(56, 0),
        Err(KernelError::WrongObject)
    );
}

#[test]
fn kernel_context_initialized_threads_can_take_scheduler_switch_paths() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(57).expect("task");
    state.enqueue_current_cpu(57).expect("enqueue");
    crate::arch::selected_isa::context_switch::reset_switch_call_count_for_test();

    state
        .set_thread_kernel_stack(0, 0xA000_0000, 0xA000_4000)
        .expect("boot stack");
    state
        .initialize_thread_kernel_switch_frame(0, 0x1111_0000)
        .expect("boot frame");
    state
        .set_thread_kernel_stack(57, 0xA001_0000, 0xA001_4000)
        .expect("thread stack");
    state
        .initialize_thread_kernel_switch_frame(57, 0x2222_0000)
        .expect("thread frame");

    let _ = state.dispatch_next_task().expect("dispatch");
    state.yield_current().expect("yield");
    assert_eq!(state.current_tid(), Some(57));
    assert!(
        crate::arch::selected_isa::context_switch::switch_call_count_for_test() > 0,
        "scheduler transitions should invoke arch switch primitive when contexts are initialized"
    );
}

#[test]
fn register_task_provisions_kernel_stack_with_trampoline_entry() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(58).expect("task");

    let context = state.thread_kernel_context(58).expect("context");
    assert!(context.owns_stack);
    assert!(context.stack_base.is_some());
    assert!(context.stack_top.is_some());
    assert_ne!(context.frame.instruction_ptr(), 0);
    assert_eq!(context.initialized, false);
}

#[test]
fn mark_task_dead_releases_kernel_context_ownership() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(59).expect("task");
    assert!(state.thread_kernel_context(59).expect("context").owns_stack);

    state.mark_task_dead(59).expect("dead");
    let context = state.thread_kernel_context(59).expect("context");
    assert!(!context.owns_stack);
    assert!(context.stack_base.is_none());
    assert!(context.stack_top.is_none());
    assert!(!context.initialized);
}

#[test]
fn join_blocks_until_target_exits_and_detached_threads_reap_on_exit() {
    let mut state = Bootstrap::init().expect("init");
    let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
    state
        .spawn_user_task_from_image(UserImageSpec {
            tid: 30,
            entry: 0x4000,
            asid: Some(asid),
            class: TaskClass::App,
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            ..Default::default()
        })
        .expect("leader");
    let joiner = state
        .spawn_user_thread(30, 0xCAFE_1000, 0x8100_0000, 0x4010)
        .expect("joiner");
    state.yield_current().expect("switch to joiner");
    assert_eq!(state.current_tid(), Some(joiner));

    assert_eq!(state.join_thread(30).expect("join pending"), None);
    assert_eq!(
        state.task_status(joiner),
        Some(TaskStatus::Blocked(WaitReason::Join(ThreadId(30))))
    );

    state.exit_task(30, 5).expect("exit leader");
    assert_eq!(state.task_status(joiner), Some(TaskStatus::Runnable));

    state.mark_thread_detached(joiner).expect("detach");
    state.exit_task(joiner, 9).expect("exit detached");
    assert_eq!(state.task_status(joiner), Some(TaskStatus::Dead));
}

#[test]
fn process_cnode_entry_is_cleared_when_last_thread_is_dead() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(700).expect("leader");
    let thread = state
        .spawn_user_thread(700, 0xDEAD_1000, 0x8100_0000, 0x4000)
        .expect("spawn thread");

    assert!(state.process_cnode_for_pid(700).is_some());

    state.mark_task_dead(thread).expect("dead thread");
    assert!(state.process_cnode_for_pid(700).is_some());

    state.mark_task_dead(700).expect("dead leader");
    assert_eq!(state.process_cnode_for_pid(700), None);
}

#[test]
fn capability_minted_in_process_cnode_is_visible_to_sibling_thread() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(710).expect("leader");
    let sibling = state
        .spawn_user_thread(710, 0xDEAD_2000, 0x8200_0000, 0x4010)
        .expect("spawn sibling");
    let cnode = state.task_cnode(710).expect("process cnode");
    let cap = state
        .mint_capability_in_cnode(cnode, Capability::new(CapObject::Kernel, CapRights::READ))
        .expect("mint");

    assert!(state.resolve_capability_for_task(710, cap).is_ok());
    assert!(state.resolve_capability_for_task(sibling, cap).is_ok());
}

#[test]
fn capability_revoke_in_process_cnode_is_visible_to_sibling_thread() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(720).expect("leader");
    let sibling = state
        .spawn_user_thread(720, 0xDEAD_3000, 0x8300_0000, 0x4020)
        .expect("spawn sibling");
    let cnode = state.task_cnode(720).expect("process cnode");
    let cap = state
        .mint_capability_in_cnode(cnode, Capability::new(CapObject::Kernel, CapRights::READ))
        .expect("mint");

    state
        .revoke_capability_in_cnode(cnode, cap)
        .expect("revoke process cap");
    assert_eq!(
        state.resolve_capability_for_task(720, cap),
        Err(KernelError::InvalidCapability)
    );
    assert_eq!(
        state.resolve_capability_for_task(sibling, cap),
        Err(KernelError::InvalidCapability)
    );
}

#[test]
fn allocate_thread_id_enforces_dynamic_tid_gap_floor() {
    let mut state = Bootstrap::init().expect("init");
    state.set_dynamic_tid_cursor_for_test(42);

    let tid = state.allocate_thread_id().expect("allocate");
    assert_eq!(tid, INITIAL_DYNAMIC_TID);
    assert_eq!(state.next_dynamic_tid_for_test(), INITIAL_DYNAMIC_TID + 1);
}

#[test]
fn allocate_thread_id_wraps_to_dynamic_floor_after_u64_max() {
    let mut state = Bootstrap::init().expect("init");
    state.set_dynamic_tid_cursor_for_test(u64::MAX);

    let tid = state.allocate_thread_id().expect("allocate");
    assert_eq!(tid, u64::MAX);
    assert_eq!(state.next_dynamic_tid_for_test(), INITIAL_DYNAMIC_TID);

    let second = state.allocate_thread_id().expect("second allocate");
    assert_eq!(second, INITIAL_DYNAMIC_TID);
}

#[test]
fn tid_allocation_telemetry_tracks_repairs_allocations_and_wraps() {
    let mut state = Bootstrap::init().expect("init");
    state.set_dynamic_tid_cursor_for_test(7);
    let first = state.allocate_thread_id().expect("allocate first");
    assert_eq!(first, INITIAL_DYNAMIC_TID);
    state.set_dynamic_tid_cursor_for_test(u64::MAX);
    let second = state.allocate_thread_id().expect("allocate second");
    assert_eq!(second, u64::MAX);

    let telemetry = state.tid_allocation_telemetry();
    assert_eq!(telemetry.dynamic_tid_allocations, 2);
    assert_eq!(telemetry.gap_floor_repairs, 1);
    assert_eq!(telemetry.dynamic_tid_wraps, 1);
}

#[test]
fn dynamic_tid_classification_is_stable_across_wrap_boundaries() {
    let mut state = Bootstrap::init().expect("init");
    state.set_dynamic_tid_cursor_for_test(u64::MAX);
    let wrapped_edge = state.allocate_thread_id().expect("max allocate");
    let wrapped_floor = state.allocate_thread_id().expect("floor allocate");

    assert!(state.is_dynamic_tid(wrapped_edge));
    assert!(state.is_dynamic_tid(wrapped_floor));
    assert!(wrapped_floor < wrapped_edge);
    assert_eq!(
        state.static_tid_upper_bound() + 1,
        state.dynamic_tid_floor()
    );
}

#[test]
fn process_teardown_reclaims_process_cnode_space_and_delegated_descendants() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(730).expect("source process");
    state.register_task(731).expect("dest process");

    let source_cnode = state.task_cnode(730).expect("source cnode");
    let source_cap = state
        .mint_capability_in_cnode(
            source_cnode,
            Capability::new(CapObject::Kernel, CapRights::READ),
        )
        .expect("mint source cap");
    let delegated_cap = state
        .grant_capability_task_to_task_with_rights(730, source_cap, 731, CapRights::READ)
        .expect("delegate");
    assert!(
        state
            .resolve_capability_for_task(731, delegated_cap)
            .is_ok()
    );

    state.mark_task_dead(730).expect("teardown source process");

    assert_eq!(state.process_cnode_for_pid(730), None);
    assert!(state.cspace_for_cnode(source_cnode).is_none());
    assert_eq!(
        state.resolve_capability_for_task(731, delegated_cap),
        Err(KernelError::InvalidCapability)
    );
}

#[test]
fn process_teardown_reclaims_multi_hop_delegated_graph_without_touching_unrelated_process() {
    let mut state = Bootstrap::init().expect("init");
    state.register_task(740).expect("source");
    state.register_task(741).expect("mid");
    state.register_task(742).expect("leaf");
    state.register_task(743).expect("unrelated");

    let source_cnode = state.task_cnode(740).expect("source cnode");
    let source_cap = state
        .mint_capability_in_cnode(
            source_cnode,
            Capability::new(CapObject::Kernel, CapRights::READ),
        )
        .expect("mint source cap");
    let mid_cap = state
        .grant_capability_task_to_task_with_rights(740, source_cap, 741, CapRights::READ)
        .expect("delegate source->mid");
    let leaf_cap = state
        .grant_capability_task_to_task_with_rights(741, mid_cap, 742, CapRights::READ)
        .expect("delegate mid->leaf");

    let unrelated_cnode = state.task_cnode(743).expect("unrelated cnode");
    let unrelated_cap = state
        .mint_capability_in_cnode(
            unrelated_cnode,
            Capability::new(CapObject::Kernel, CapRights::READ),
        )
        .expect("mint unrelated cap");

    assert!(state.resolve_capability_for_task(741, mid_cap).is_ok());
    assert!(state.resolve_capability_for_task(742, leaf_cap).is_ok());
    assert!(
        state
            .resolve_capability_for_task(743, unrelated_cap)
            .is_ok()
    );

    state.mark_task_dead(740).expect("teardown source");

    assert_eq!(state.process_cnode_for_pid(740), None);
    assert_eq!(
        state.resolve_capability_for_task(741, mid_cap),
        Err(KernelError::InvalidCapability)
    );
    assert_eq!(
        state.resolve_capability_for_task(742, leaf_cap),
        Err(KernelError::InvalidCapability)
    );
    assert!(
        state
            .resolve_capability_for_task(743, unrelated_cap)
            .is_ok()
    );
}

#[test]
fn direct_legacy_global_cspace_access_patterns_are_forbidden() {
    fn visit_rs_files(root: &std::path::Path, f: &mut dyn FnMut(&std::path::Path, &str)) {
        let entries = std::fs::read_dir(root).expect("read_dir");
        for entry in entries {
            let entry = entry.expect("entry");
            let path = entry.path();
            if path.is_dir() {
                visit_rs_files(&path, f);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read file");
            f(&path, &source);
        }
    }

    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut offenders: Vec<String> = Vec::new();
    let mut check = |path: &std::path::Path, source: &str| {
        let rel = path
            .strip_prefix(&repo_root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        if rel == "src/kernel/boot/mod.rs" {
            // Contains this guard test's own pattern literals.
            return;
        }
        for pattern in [
            "self.cspace.get(",
            "self.cspace.revoke(",
            "self.cspace.has_right(",
        ] {
            if source.contains(pattern) {
                offenders.push(format!("{rel}: {pattern}"));
            }
        }
    };

    visit_rs_files(&repo_root.join("src/kernel"), &mut check);
    visit_rs_files(&repo_root.join("src/services"), &mut check);

    if !offenders.is_empty() {
        panic!(
            "legacy self.cspace access pattern found in runtime code:\n{}",
            offenders.join("\n")
        );
    }
}
