// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

use yarm::kernel::boot::Bootstrap;
#[cfg(not(test))]
use yarm::services::control_plane::init;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const MAX_PVH_MEMMAP_ENTRIES: usize = 128;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const PVH_MAGIC: u32 = 0x336e_c578;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const MAX_PVH_PHYS_EXCLUSIVE: u64 = 1u64 << 52;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[repr(C)]
struct PvhStartInfo {
    _magic: u32,
    _version: u32,
    _flags: u32,
    nr_modules: u32,
    modlist_paddr: u64,
    cmdline_paddr: u64,
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
fn log_pvh_boot_metadata(start_info_ptr: usize) {
    use yarm::arch::x86_64::console::write_line;

    if start_info_ptr == 0 {
        write_line("PVH: null ptr");
        return;
    }
    let start_info = unsafe { &*(start_info_ptr as *const PvhStartInfo) };
    if start_info._magic != PVH_MAGIC {
        write_line("PVH: bad magic");
        return;
    }
    write_line("PVH: magic OK");
    // Intentionally avoid formatter-based yarm_log! while diagnosing early-boot faults.
    // cmdline/modlist parsing is intentionally skipped in this temporary path.
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn init_pt_allocator_from_pvh_memmap(start_info_ptr: usize) {
    const PAGE_SIZE_U64: u64 = yarm::kernel::vm::PAGE_SIZE as u64;
    const RESERVED_LOW_EXCLUSIVE: u64 = yarm::arch::platform_layout::NEXT_ANON_PHYS_BASE;
    const MEMMAP_ENTRY_SIZE: u64 = core::mem::size_of::<PvhMemMapEntry>() as u64;
    const MEMMAP_ENTRY_ALIGN: u64 = core::mem::align_of::<PvhMemMapEntry>() as u64;

    if start_info_ptr == 0 {
        return;
    }

    let start_info = unsafe { &*(start_info_ptr as *const PvhStartInfo) };
    if start_info._magic != PVH_MAGIC {
        return;
    }
    if start_info.memmap_paddr == 0 || start_info.memmap_entries == 0 {
        return;
    }
    if !start_info.memmap_paddr.is_multiple_of(MEMMAP_ENTRY_ALIGN) {
        return;
    }

    let count = core::cmp::min(start_info.memmap_entries as usize, MAX_PVH_MEMMAP_ENTRIES);
    let Some(memmap_bytes) = (count as u64).checked_mul(MEMMAP_ENTRY_SIZE) else {
        return;
    };
    let Some(memmap_end) = start_info.memmap_paddr.checked_add(memmap_bytes) else {
        return;
    };
    if memmap_end > MAX_PVH_PHYS_EXCLUSIVE {
        return;
    }

    let mut regions = [yarm::kernel::frame_allocator::MemoryRegion {
        start: 0,
        len: 0,
        usable: false,
    }; MAX_PVH_MEMMAP_ENTRIES];
    let mut used = 0usize;

    for idx in 0..count {
        let Some(entry_paddr) = start_info
            .memmap_paddr
            .checked_add((idx as u64).saturating_mul(MEMMAP_ENTRY_SIZE))
        else {
            break;
        };
        let entry_ptr = entry_paddr as *const PvhMemMapEntry;
        let entry = unsafe { core::ptr::read_unaligned(entry_ptr) };
        if entry.kind != 1 {
            continue;
        }
        if entry.size == 0 {
            continue;
        }
        let Some(raw_end) = entry.addr.checked_add(entry.size) else {
            continue;
        };
        let mut start = entry.addr;
        let mut end = raw_end;

        if end > MAX_PVH_PHYS_EXCLUSIVE {
            end = MAX_PVH_PHYS_EXCLUSIVE;
        }
        if start >= end {
            continue;
        }

        if end <= RESERVED_LOW_EXCLUSIVE {
            continue;
        }
        if start < RESERVED_LOW_EXCLUSIVE {
            start = RESERVED_LOW_EXCLUSIVE;
        }

        let aligned_start = start
            .saturating_add(PAGE_SIZE_U64 - 1)
            .saturating_div(PAGE_SIZE_U64)
            .saturating_mul(PAGE_SIZE_U64);
        let aligned_end = end
            .saturating_div(PAGE_SIZE_U64)
            .saturating_mul(PAGE_SIZE_U64);
        if aligned_end <= aligned_start {
            continue;
        }

        if regions[..used].iter().any(|existing| {
            let existing_end = existing.start.saturating_add(existing.len);
            aligned_start < existing_end && aligned_end > existing.start
        }) {
            continue;
        }
        if used >= regions.len() {
            break;
        }
        regions[used] = yarm::kernel::frame_allocator::MemoryRegion {
            start: aligned_start,
            len: aligned_end - aligned_start,
            usable: true,
        };
        used += 1;
    }

    if used == 0 {
        return;
    }

    let _ = yarm::kernel::frame_allocator::init_pt_frame_allocator(&regions[..used]);
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
        yarm::arch::x86_64::irq::enable_interrupts_for_boot();
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
    yarm::yarm_log!("YARM_INIT_START");
    let mut kernel = Bootstrap::init().expect("init");
    let summary = match init::run_minimum_profile_with_kernel(
        &mut kernel,
        init::InitRuntimeBootConfig::baseline(),
    ) {
        Ok(summary) => summary,
        Err(err) => {
            yarm::yarm_log!("YARM_INIT_FAIL stage=minimum_profile err={:?}", err);
            return;
        }
    };
    let dispatched = match kernel.dispatch_ready_task() {
        Ok(dispatched) => dispatched,
        Err(err) => {
            yarm::yarm_log!("YARM_INIT_FAIL stage=dispatch err={:?}", err);
            return;
        }
    };
    yarm::yarm_log!(
        "YARM_INIT_RUNTIME phase={:?} supervisor_managed={} initramfs_reads={} dispatched_tid={:?}",
        summary.init_phase,
        summary.supervisor_managed_services,
        summary.initramfs_handled,
        dispatched
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
    yarm::arch::x86_64::descriptor_tables::ensure_boot_descriptor_tables_scaffolded();
    #[cfg(target_arch = "x86_64")]
    yarm::arch::x86_64::console::write_line("KM0");
    #[cfg(target_arch = "x86_64")]
    yarm::arch::x86_64::console::write_line("KM1");
    #[cfg(target_arch = "x86_64")]
    log_pvh_boot_metadata(start_info_ptr);
    #[cfg(target_arch = "x86_64")]
    yarm::arch::x86_64::console::write_line("KM2");
    #[cfg(target_arch = "x86_64")]
    init_pt_allocator_from_pvh_memmap(start_info_ptr);
    #[cfg(target_arch = "x86_64")]
    yarm::arch::x86_64::console::write_line("KM3");
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
