// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

use yarm::kernel::boot::Bootstrap;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const MAX_PVH_MEMMAP_ENTRIES: usize = 128;
const MAX_PVH_MODULES: usize = 32;
const PVH_MAGIC: u32 = 0x336e_c578;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const MAX_PVH_PHYS_EXCLUSIVE: u64 = 1u64 << 52;

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

#[repr(C)]
#[derive(Clone, Copy)]
struct PvhModule {
    paddr_start: u64,
    paddr_end: u64,
    cmdline_paddr: u64,
    _reserved: u64,
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

#[derive(Clone, Copy)]
struct PvhModuleWindow {
    start: u64,
    end: u64,
}

#[derive(Clone, Copy)]
struct PvhModuleSummary {
    module_count: usize,
    initramfs: Option<PvhModuleWindow>,
}

fn read_pvh_module_summary(start_info_ptr: usize) -> Option<PvhModuleSummary> {
    if start_info_ptr == 0 {
        return None;
    }
    let start_info = unsafe { &*(start_info_ptr as *const PvhStartInfo) };
    if start_info._magic != PVH_MAGIC {
        return None;
    }
    if start_info.nr_modules == 0 || start_info.modlist_paddr == 0 {
        return Some(PvhModuleSummary {
            module_count: 0,
            initramfs: None,
        });
    }

    let module_count = core::cmp::min(start_info.nr_modules as usize, MAX_PVH_MODULES);
    let mut initramfs = None;
    for idx in 0..module_count {
        let module_ptr = (start_info.modlist_paddr as *const PvhModule).wrapping_add(idx);
        let module = unsafe { core::ptr::read_unaligned(module_ptr) };
        if module.paddr_start == 0 || module.paddr_end <= module.paddr_start {
            continue;
        }
        if initramfs.is_none() {
            initramfs = Some(PvhModuleWindow {
                start: module.paddr_start,
                end: module.paddr_end,
            });
        }
    }

    Some(PvhModuleSummary {
        module_count,
        initramfs,
    })
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
    if let Some(summary) = read_pvh_module_summary(start_info_ptr) {
        yarm::yarm_log!(
            "YARM_BOOT_PVH_MODULES total={} initramfs_start=0x{:x} initramfs_end=0x{:x}",
            summary.module_count,
            summary.initramfs.map(|window| window.start).unwrap_or(0),
            summary.initramfs.map(|window| window.end).unwrap_or(0)
        );
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn init_pt_allocator_from_pvh_memmap(start_info_ptr: usize) {
    const PAGE_SIZE_U64: u64 = yarm::kernel::vm::PAGE_SIZE as u64;
    const RESERVED_LOW_EXCLUSIVE: u64 = yarm::arch::platform_layout::NEXT_ANON_PHYS_BASE;
    const DIRECT_MAP_LIMIT: u64 = yarm::arch::platform_layout::KERNEL_PHYS_DIRECT_MAP_BYTES;
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
        if end > DIRECT_MAP_LIMIT {
            end = DIRECT_MAP_LIMIT;
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
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn run_boot_markers() -> &'static mut yarm::kernel::boot::KernelState {
    debug_uart_marker(b'H');
    yarm::arch::x86_64::descriptor_tables::ensure_boot_descriptor_tables_scaffolded();
    let kernel = yarm::arch::x86_64::descriptor_tables::install_trap_kernel_state(
        Bootstrap::init().expect("kernel init"),
    );
    debug_uart_marker(b'I');
    let started_secondary = yarm::arch::x86_64::smp::start_secondary_cpus(kernel).unwrap_or(0);
    yarm::yarm_log!(
        "YARM_SMP_STARTUP started_secondary={} online_cpus={} present_cpus={}",
        started_secondary,
        kernel.online_cpu_count(),
        kernel.present_cpu_count()
    );
    kernel.program_timer_deadline_current_cpu(
        yarm::arch::platform_layout::BOOTSTRAP_TIMER_DEADLINE_TICKS,
    );
    yarm::arch::x86_64::irq::enable_interrupts_for_boot();
    debug_uart_marker(b'J');
    yarm::yarm_log!(
        "YARM_BOOT_OK present_cpus={} present_bitmap=0x{:x} online_cpus={}",
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );
    debug_uart_marker(b'K');
    kernel
}

#[inline]
#[cfg(not(all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
fn run_boot_markers() -> yarm::kernel::boot::KernelState {
    let kernel = Bootstrap::init().expect("kernel init");
    yarm::yarm_log!(
        "YARM_BOOT_OK present_cpus={} present_bitmap=0x{:x} online_cpus={}",
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );
    kernel
}

#[cfg(not(test))]
fn run_scheduler_loop(kernel: &mut yarm::kernel::boot::KernelState) {
    if let Err(err) = yarm::arch::boot_entry::bootstrap_first_user_task(kernel) {
        yarm::pr_err!("failed to bootstrap first user task: {:?}", err);
    }

    let initial = kernel.dispatch_ready_task().ok().flatten();
    yarm::yarm_log!("YARM_SCHED_LOOP_START dispatched_tid={:?}", initial);
    yarm::arch::boot_entry::enter_dispatched_user_task_if_available(kernel, initial);
}

fn run() {
    #[cfg(not(test))]
    {
        #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
        run_scheduler_loop(run_boot_markers());
        #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
        {
            let mut kernel = run_boot_markers();
            run_scheduler_loop(&mut kernel);
        }
    }
    #[cfg(test)]
    let _ = run_boot_markers();

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
    fn pvh_module_summary_selects_first_valid_module_as_initramfs() {
        let modules = [
            PvhModule {
                paddr_start: 0x4000_0000,
                paddr_end: 0x4008_0000,
                cmdline_paddr: 0,
                _reserved: 0,
            },
            PvhModule {
                paddr_start: 0x5000_0000,
                paddr_end: 0x5008_0000,
                cmdline_paddr: 0,
                _reserved: 0,
            },
        ];
        let start_info = PvhStartInfo {
            _magic: PVH_MAGIC,
            _version: 0,
            _flags: 0,
            nr_modules: modules.len() as u32,
            modlist_paddr: modules.as_ptr() as u64,
            cmdline_paddr: 0,
            _rsdp_paddr: 0,
            memmap_paddr: 0,
            memmap_entries: 0,
        };
        let summary =
            read_pvh_module_summary((&start_info as *const PvhStartInfo) as usize).expect("pvh");
        assert_eq!(summary.module_count, 2);
        let initramfs = summary.initramfs.expect("initramfs window");
        assert_eq!(initramfs.start, 0x4000_0000);
        assert_eq!(initramfs.end, 0x4008_0000);
    }

    #[test]
    fn pvh_module_summary_ignores_invalid_windows() {
        let modules = [PvhModule {
            paddr_start: 0x6000_0000,
            paddr_end: 0x6000_0000,
            cmdline_paddr: 0,
            _reserved: 0,
        }];
        let start_info = PvhStartInfo {
            _magic: PVH_MAGIC,
            _version: 0,
            _flags: 0,
            nr_modules: modules.len() as u32,
            modlist_paddr: modules.as_ptr() as u64,
            cmdline_paddr: 0,
            _rsdp_paddr: 0,
            memmap_paddr: 0,
            memmap_entries: 0,
        };
        let summary =
            read_pvh_module_summary((&start_info as *const PvhStartInfo) as usize).expect("pvh");
        assert_eq!(summary.module_count, 1);
        assert!(summary.initramfs.is_none());
    }

    #[test]
    fn kernel_boot_markers_run() {
        run_boot_markers();
    }
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    #[cfg(target_arch = "x86_64")]
    {
        use core::fmt::Write;

        struct PanicUartWriter;

        impl Write for PanicUartWriter {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                for &byte in s.as_bytes() {
                    debug_uart_marker(byte);
                }
                Ok(())
            }
        }

        let mut writer = PanicUartWriter;
        let _ = writer.write_str("PANIC ");
        if let Some(location) = info.location() {
            let _ = write!(
                writer,
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            );
        } else {
            let _ = writer.write_str("<unknown>");
        }
        let _ = writer.write_str(": ");
        let _ = write!(writer, "{}", info.message());
        let _ = writer.write_str("\n");
    }
    loop {}
}
