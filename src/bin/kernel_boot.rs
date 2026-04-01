#![no_std]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

use yarm::kernel::boot::Bootstrap;
#[cfg(not(test))]
use yarm::kernel::ipc::Message;
#[cfg(not(test))]
use yarm::kernel::process::{ProcessService, SpawnV2Result, WaitPidV2Result};
#[cfg(not(test))]
use yarm::kernel::process_abi::{PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, SpawnV2Args, WaitPidV2Args};
#[cfg(not(test))]
use yarm::kernel::vfs::{OpenAtRequest, ReadWriteRequest, openat_message, read_message};
#[cfg(not(test))]
use yarm::services::common::vfs_service::{VfsReply, VfsService};
#[cfg(not(test))]
use yarm::services::fs::initramfs::{INITRAMFS_BOOT_MARKER_PATH_PTR, InitramfsBackend};

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const MAX_PVH_MEMMAP_ENTRIES: usize = 128;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const MAX_PVH_PHYS_EXCLUSIVE: u64 = 1u64 << 52;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[repr(C)]
struct PvhStartInfo {
    _magic: u32,
    _version: u32,
    _flags: u32,
    _nr_modules: u32,
    _modlist_paddr: u64,
    _cmdline_paddr: u64,
    _rsdp_paddr: u64,
    memmap_paddr: u64,
    memmap_entries: u32,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[repr(C)]
#[derive(Clone, Copy)]
struct PvhMemMapEntry {
    addr: u64,
    size: u64,
    kind: u32,
    _reserved: u32,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn init_pt_allocator_from_pvh_memmap(start_info_ptr: usize) {
    #[derive(Default)]
    struct PvhMemmapTelemetry {
        total_entries: usize,
        usable_entries: usize,
        accepted_entries: usize,
        rejected_non_usable: usize,
        rejected_zero_size: usize,
        rejected_overflow: usize,
        rejected_bounds: usize,
        rejected_too_small: usize,
        rejected_overlap: usize,
        rejected_capacity: usize,
        clipped_reserved_low: usize,
        clipped_alignment: usize,
    }

    const PAGE_SIZE_U64: u64 = yarm::kernel::vm::PAGE_SIZE as u64;
    const RESERVED_LOW_EXCLUSIVE: u64 = yarm::arch::platform_layout::NEXT_ANON_PHYS_BASE;

    if start_info_ptr == 0 {
        return;
    }

    let start_info = unsafe { &*(start_info_ptr as *const PvhStartInfo) };
    if start_info.memmap_paddr == 0 || start_info.memmap_entries == 0 {
        return;
    }

    let count = core::cmp::min(start_info.memmap_entries as usize, MAX_PVH_MEMMAP_ENTRIES);
    let memmap = start_info.memmap_paddr as *const PvhMemMapEntry;
    let entries = unsafe { core::slice::from_raw_parts(memmap, count) };

    let mut regions = [yarm::kernel::frame_allocator::MemoryRegion {
        start: 0,
        len: 0,
        usable: false,
    }; MAX_PVH_MEMMAP_ENTRIES];
    let mut used = 0usize;
    let mut telemetry = PvhMemmapTelemetry::default();

    for entry in entries {
        telemetry.total_entries = telemetry.total_entries.saturating_add(1);
        if entry.kind != 1 {
            telemetry.rejected_non_usable = telemetry.rejected_non_usable.saturating_add(1);
            continue;
        }
        telemetry.usable_entries = telemetry.usable_entries.saturating_add(1);
        if entry.size == 0 {
            telemetry.rejected_zero_size = telemetry.rejected_zero_size.saturating_add(1);
            continue;
        }
        let Some(raw_end) = entry.addr.checked_add(entry.size) else {
            telemetry.rejected_overflow = telemetry.rejected_overflow.saturating_add(1);
            continue;
        };
        let mut start = entry.addr;
        let mut end = raw_end;

        if end > MAX_PVH_PHYS_EXCLUSIVE {
            end = MAX_PVH_PHYS_EXCLUSIVE;
        }
        if start >= end {
            telemetry.rejected_bounds = telemetry.rejected_bounds.saturating_add(1);
            continue;
        }

        if end <= RESERVED_LOW_EXCLUSIVE {
            telemetry.rejected_bounds = telemetry.rejected_bounds.saturating_add(1);
            continue;
        }
        if start < RESERVED_LOW_EXCLUSIVE {
            start = RESERVED_LOW_EXCLUSIVE;
            telemetry.clipped_reserved_low = telemetry.clipped_reserved_low.saturating_add(1);
        }

        let aligned_start = start
            .saturating_add(PAGE_SIZE_U64 - 1)
            .saturating_div(PAGE_SIZE_U64)
            .saturating_mul(PAGE_SIZE_U64);
        let aligned_end = end
            .saturating_div(PAGE_SIZE_U64)
            .saturating_mul(PAGE_SIZE_U64);
        if aligned_start != start || aligned_end != end {
            telemetry.clipped_alignment = telemetry.clipped_alignment.saturating_add(1);
        }
        if aligned_end <= aligned_start {
            telemetry.rejected_too_small = telemetry.rejected_too_small.saturating_add(1);
            continue;
        }

        if regions[..used].iter().any(|existing| {
            let existing_end = existing.start.saturating_add(existing.len);
            aligned_start < existing_end && aligned_end > existing.start
        }) {
            telemetry.rejected_overlap = telemetry.rejected_overlap.saturating_add(1);
            continue;
        }
        if used >= regions.len() {
            telemetry.rejected_capacity = telemetry.rejected_capacity.saturating_add(1);
            break;
        }
        regions[used] = yarm::kernel::frame_allocator::MemoryRegion {
            start: aligned_start,
            len: aligned_end - aligned_start,
            usable: true,
        };
        used += 1;
        telemetry.accepted_entries = telemetry.accepted_entries.saturating_add(1);
    }

    yarm::yarm_log!(
        "YARM_PVH_MEMMAP total={} usable={} accepted={} rej_nonusable={} rej_zero={} rej_overflow={} rej_bounds={} rej_small={} rej_overlap={} rej_capacity={} clip_low={} clip_align={}",
        telemetry.total_entries,
        telemetry.usable_entries,
        telemetry.accepted_entries,
        telemetry.rejected_non_usable,
        telemetry.rejected_zero_size,
        telemetry.rejected_overflow,
        telemetry.rejected_bounds,
        telemetry.rejected_too_small,
        telemetry.rejected_overlap,
        telemetry.rejected_capacity,
        telemetry.clipped_reserved_low,
        telemetry.clipped_alignment
    );

    if used == 0 {
        return;
    }

    if let Err(err) = yarm::kernel::frame_allocator::init_pt_frame_allocator(&regions[..used]) {
        yarm::yarm_log!("YARM_PVH_MEMMAP_INIT_ERR err={:?}", err);
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn debug_uart_marker(byte: u8) {
    unsafe {
        core::arch::asm!(
            "2:",
            "in al, dx",
            "test al, 0x20",
            "jz 2b",
            in("dx") 0x3FDu16,
            lateout("al") _,
            options(nomem, nostack)
        );
        core::arch::asm!(
            "out dx, al",
            in("dx") 0x3F8u16,
            in("al") byte,
            options(nomem, nostack)
        );
    }
}

#[inline]
fn run_boot_markers() {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    {
        debug_uart_marker(b'H');
        yarm::arch::x86_64::descriptor_tables::ensure_boot_descriptor_tables_scaffolded();
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    let mut kernel = Bootstrap::init().expect("kernel init");
    #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
    let kernel = Bootstrap::init().expect("kernel init");
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    {
        debug_uart_marker(b'I');
        yarm::arch::x86_64::descriptor_tables::register_trap_kernel_state(&mut kernel);
        kernel.program_timer_deadline_current_cpu(
            yarm::arch::platform_layout::BOOTSTRAP_TIMER_DEADLINE_TICKS,
        );
        debug_uart_marker(b'J');
    }
    yarm::yarm_log!(
        "YARM_BOOT_OK present_cpus={} present_bitmap=0x{:x} online_cpus={}",
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    debug_uart_marker(b'K');
}

#[cfg(not(test))]
fn run_process_vfs_smoke() {
    let mut proc = ProcessService::new();
    let spawn = Message::with_header(
        0,
        PROC_OP_SPAWN_V2,
        0,
        None,
        &SpawnV2Args::new(1, 99).encode(),
    )
    .expect("spawn");
    let spawn_rep = proc.handle(spawn).expect("spawn rep");
    let child = SpawnV2Result::decode(spawn_rep.as_slice()).expect("child");
    proc.mark_exit(child.pid, 7).expect("mark exit");

    let wait = Message::with_header(
        0,
        PROC_OP_WAITPID_V2,
        0,
        None,
        &WaitPidV2Args::new(1, child.pid.0).encode(),
    )
    .expect("wait");
    let wait_rep = proc.handle(wait).expect("wait rep");
    let waited = WaitPidV2Result::decode(wait_rep.as_slice()).expect("waited");

    let mut vfs = VfsService::with_backend(InitramfsBackend::new(4096));
    yarm::yarm_log!("YARM_INIT_START");
    let open = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: INITRAMFS_BOOT_MARKER_PATH_PTR,
        flags: 0,
        mode: 0,
    })
    .expect("open");
    let open_rep = vfs.handle_request(open).expect("open rep");
    let fd = match VfsReply::from_message(open_rep).expect("decode open reply") {
        VfsReply::OpenAtFd(fd) => fd,
        _ => panic!("unexpected open reply"),
    };
    let read = read_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: 16,
    })
    .expect("read");
    let read_rep = vfs.handle_request(read).expect("read rep");

    yarm::yarm_log!(
        "YARM_PROC_VFS_OK pid={} exit={} read_opcode={}",
        child.pid.0,
        waited.exit_code,
        read_rep.opcode
    );
    yarm::yarm_log!("YARM_INIT_DONE");
}

fn run() {
    run_boot_markers();

    #[cfg(not(test))]
    run_process_vfs_smoke();

    #[cfg(not(feature = "hosted-dev"))]
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(feature = "hosted-dev")]
fn main() {
    yarm::arch::boot_entry::run_kernel_boot(run);
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn yarm_kernel_main(start_info_ptr: usize) -> ! {
    #[cfg(target_arch = "x86_64")]
    yarm::arch::x86_64::console::write_line("KM0");
    #[cfg(target_arch = "x86_64")]
    init_pt_allocator_from_pvh_memmap(start_info_ptr);
    yarm::arch::boot_entry::run_kernel_boot(run);
    unreachable!("kernel run loop should not return");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_boot_markers_run() {
        run_boot_markers();
    }
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
