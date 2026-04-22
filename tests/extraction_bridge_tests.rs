// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use core::mem;

use yarm::kernel::boot::{CapacityTelemetry, IpcPathTelemetry, KernelCapacityProfile};
use yarm::kernel::capabilities::{CNodeId, CapId, CapRights, CapabilityDeriveError};
use yarm::kernel::ipc::{Message, SharedMemoryRegion, ThreadId, TransferCapId};
use yarm::kernel::scheduler::{CpuId, SchedulerError, TaskPriority};

#[test]
fn extracted_kernel_type_families_match_yarm_kernel_layouts() {
    assert_eq!(
        mem::size_of::<ThreadId>(),
        mem::size_of::<yarm_kernel::ipc::ThreadId>()
    );
    assert_eq!(
        mem::size_of::<TransferCapId>(),
        mem::size_of::<yarm_kernel::ipc::TransferCapId>()
    );
    assert_eq!(
        mem::size_of::<Message>(),
        mem::size_of::<yarm_kernel::ipc::Message>()
    );
    assert_eq!(
        mem::size_of::<SharedMemoryRegion>(),
        mem::size_of::<yarm_kernel::ipc::SharedMemoryRegion>()
    );

    assert_eq!(
        mem::size_of::<CapId>(),
        mem::size_of::<yarm_kernel::capability::CapId>()
    );
    assert_eq!(
        mem::size_of::<CNodeId>(),
        mem::size_of::<yarm_kernel::capability::CNodeId>()
    );
    assert_eq!(
        mem::size_of::<CapRights>(),
        mem::size_of::<yarm_kernel::capability::CapRights>()
    );

    assert_eq!(
        mem::size_of::<CpuId>(),
        mem::size_of::<yarm_kernel::scheduler::CpuId>()
    );
    assert_eq!(
        TaskPriority::Normal as u8,
        yarm_kernel::scheduler::TaskPriority::Normal as u8
    );
    let _sched_err: yarm_kernel::scheduler::SchedulerError = SchedulerError::QueueFull;

    assert_eq!(
        mem::size_of::<IpcPathTelemetry>(),
        mem::size_of::<yarm_kernel::boot::IpcPathTelemetry>()
    );
    assert_eq!(
        mem::size_of::<CapacityTelemetry>(),
        mem::size_of::<yarm_kernel::boot::CapacityTelemetry>()
    );
    assert_eq!(
        KernelCapacityProfile::HostedDefault as u8,
        yarm_kernel::boot::KernelCapacityProfile::HostedDefault as u8
    );

    let _cap_err: yarm_kernel::capability::CapabilityDeriveError =
        CapabilityDeriveError::RightsEscalation;
}

#[test]
fn extraction_bridge_sources_keep_yarm_kernel_reexports() {
    let ipc_src = include_str!("../src/kernel/ipc.rs");
    let cap_src = include_str!("../src/kernel/capabilities.rs");
    let sched_src = include_str!("../src/kernel/scheduler.rs");
    let boot_types_src = include_str!("../src/kernel/boot/types.rs");

    assert!(ipc_src.contains("pub use yarm_kernel::ipc::{"));
    assert!(cap_src.contains("pub use yarm_kernel::capability::{"));
    assert!(sched_src.contains("pub use yarm_kernel::scheduler::{"));
    assert!(boot_types_src.contains("pub use yarm_kernel::boot::{"));
}
