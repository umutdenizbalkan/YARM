use crate::kernel::bootstrap::KernelError;
use crate::kernel::init::InitServerLite;
use crate::kernel::vfs::{MountRouter, OpenAtRequest, VfsService, openat_message};
use crate::services::fs::initramfs::{INITRAMFS_BUSYBOX_PATH_PTR, InitramfsBackend};
use crate::services::fs::ramfs::RamFsBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountOrchestrationSummary {
    pub low_mount_opcode: u16,
    pub high_mount_opcode: u16,
}

pub fn run_mount_orchestration_scenario() -> Result<MountOrchestrationSummary, KernelError> {
    let router = MountRouter::new(0x8000, RamFsBackend::new(), InitramfsBackend::new(4096));
    let mut vfs = VfsService::with_backend(router);

    let open_low = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: 0x1000,
        flags: 0,
        mode: 0,
    })
    .map_err(|_| KernelError::WrongObject)?;
    let low_rep = vfs
        .handle_request(open_low)
        .map_err(|_| KernelError::WrongObject)?;

    let open_high = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: INITRAMFS_BUSYBOX_PATH_PTR,
        flags: 0,
        mode: 0,
    })
    .map_err(|_| KernelError::WrongObject)?;
    let high_rep = vfs
        .handle_request(open_high)
        .map_err(|_| KernelError::WrongObject)?;

    Ok(MountOrchestrationSummary {
        low_mount_opcode: low_rep.opcode,
        high_mount_opcode: high_rep.opcode,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountFallbackTelemetry {
    pub recovered_with_fat: bool,
    pub mounted_count: usize,
}

pub fn run_mount_fallback_telemetry_scenario() -> Result<MountFallbackTelemetry, KernelError> {
    let init = InitServerLite::new();
    let report = init.execute_mount_plan_with_fail_at(Some(3))?;
    Ok(MountFallbackTelemetry {
        recovered_with_fat: report.recovered_with_fat,
        mounted_count: report.mounted_count,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountMatrixRow {
    pub fail_at: Option<usize>,
    pub allow_fallback: bool,
    pub result: Result<MountFallbackTelemetry, KernelError>,
}

pub fn run_mount_failure_matrix_scenarios() -> [MountMatrixRow; 10] {
    core::array::from_fn(|i| {
        let allow_fallback = i >= 5;
        let fail_at = match i % 5 {
            0 => None,
            1 => Some(0),
            2 => Some(1),
            3 => Some(2),
            _ => Some(3),
        };
        let mut init = InitServerLite::new();
        let mut plan = init.mount_plan();
        plan.allow_fallback_to_fat = allow_fallback;
        let _ = init.set_mount_plan(plan);
        MountMatrixRow {
            fail_at,
            allow_fallback,
            result: init.execute_mount_plan_with_fail_at(fail_at).map(|report| {
                MountFallbackTelemetry {
                    recovered_with_fat: report.recovered_with_fat,
                    mounted_count: report.mounted_count,
                }
            }),
        }
    })
}
