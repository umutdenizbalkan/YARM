#![no_std]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

use yarm::kernel::boot::Bootstrap;
use yarm::kernel::ipc::Message;
use yarm::kernel::process::{ProcessService, SpawnV2Result, WaitPidV2Result};
use yarm::kernel::process_abi::{PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, SpawnV2Args, WaitPidV2Args};
use yarm::kernel::vfs::{OpenAtRequest, ReadWriteRequest, openat_message, read_message};
use yarm::services::common::vfs_service::{VfsReply, VfsService};
use yarm::services::fs::initramfs::{INITRAMFS_BOOT_MARKER_PATH_PTR, InitramfsBackend};

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const MAX_PVH_MEMMAP_ENTRIES: usize = 128;

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

    for entry in entries {
        if entry.size == 0 || entry.kind != 1 {
            continue;
        }
        if used >= regions.len() {
            break;
        }
        regions[used] = yarm::kernel::frame_allocator::MemoryRegion {
            start: entry.addr,
            len: entry.size,
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
fn run() {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    let mut kernel = Bootstrap::init().expect("kernel init");
    #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
    let kernel = Bootstrap::init().expect("kernel init");
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    {
        debug_uart_marker(b'I');
        yarm::arch::x86_64::descriptor_tables::register_trap_kernel_state(&mut kernel);
        yarm::arch::x86_64::descriptor_tables::ensure_boot_descriptor_tables_scaffolded();
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
        run();
    }
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
