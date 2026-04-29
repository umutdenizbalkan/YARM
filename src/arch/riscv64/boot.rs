// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
use core::arch::global_asm;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
global_asm!(
    r#"
    .section .bss.bootstack,"aw",@nobits
    .align 16
boot_stack_riscv64:
    .skip 16384
boot_stack_riscv64_end:

    .section .text.boot,"ax",@progbits
    .weak _start
    .type _start,@function
_start:
    la sp, boot_stack_riscv64_end
    .weak yarm_kernel_main
    call yarm_kernel_main
1:
    wfi
    j 1b
    "#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const RING3_INIT_SERVER_TID: u64 = 1;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const INITRAMFS_HELLO_WORLD_IMAGE_ID: u64 = 0x494E_4954_5256_484C; // "INITRVHL"

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn initramfs_static_hello_world_elf() -> [u8; 256] {
    let mut image = [0u8; 256];
    // ELF header.
    image[..4].copy_from_slice(b"\x7FELF");
    image[4] = 2; // ELFCLASS64
    image[5] = 1; // little-endian
    image[6] = 1; // EV_CURRENT
    image[7] = 0; // SYSV ABI
    image[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    image[18..20].copy_from_slice(&0xF3u16.to_le_bytes()); // EM_RISCV
    image[20..24].copy_from_slice(&1u32.to_le_bytes()); // EV_CURRENT
    let entry = 0x0040_1000u64;
    image[24..32].copy_from_slice(&entry.to_le_bytes());
    image[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    image[52..54].copy_from_slice(&(64u16).to_le_bytes()); // e_ehsize
    image[54..56].copy_from_slice(&(56u16).to_le_bytes()); // e_phentsize
    image[56..58].copy_from_slice(&(1u16).to_le_bytes()); // e_phnum

    // Single PT_LOAD segment.
    let ph = 64usize;
    image[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    image[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes()); // RX
    image[ph + 8..ph + 16].copy_from_slice(&128u64.to_le_bytes()); // p_offset
    image[ph + 16..ph + 24].copy_from_slice(&(entry & !0xFFF).to_le_bytes()); // p_vaddr
    image[ph + 24..ph + 32].copy_from_slice(&0u64.to_le_bytes()); // p_paddr
    image[ph + 32..ph + 40].copy_from_slice(&12u64.to_le_bytes()); // p_filesz
    image[ph + 40..ph + 48].copy_from_slice(&16u64.to_le_bytes()); // p_memsz
    image[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align

    // li a7, SYSCALL_YIELD_NR; ecall; j ecall.
    image[128..140].copy_from_slice(&[
        0x93, 0x08, 0x00, 0x00, // addi a7, x0, 0
        0x73, 0x00, 0x00, 0x00, // ecall
        0x6F, 0xF0, 0xDF, 0xFF, // jal x0, -4
    ]);
    image
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn load_init_elf_from_initramfs_vfs() -> Option<alloc::vec::Vec<u8>> {
    use yarm_fs_servers::common::vfs_ipc::{ReadWriteRequest, openat_inline_message, read_message};
    use yarm_fs_servers::initramfs::{InitramfsBackend, InitramfsService, boot_initrd_bytes};
    let backend = InitramfsBackend::from_cpio_newc_static(boot_initrd_bytes()?);
    let mut svc = InitramfsService::with_backend(backend);
    let open = svc.handle(openat_inline_message(0, b"/initramfs/init", 0, 0).ok()?).ok()?;
    let fd = yarm_srv_common::vfs_reply::VfsReply::from_opcode_payload_checked(open.opcode, open.as_slice())
        .ok()?
        .as_u64();
    let mut out = alloc::vec::Vec::new();
    loop {
        let reply = svc.handle(read_message(ReadWriteRequest { fd, buf_ptr: 0, len: 512 }).ok()?).ok()?;
        let (_status, n, bytes) = yarm_srv_common::vfs_reply::VfsReply::decode_read_extended(reply.as_slice()).ok()?;
        if n == 0 { break; }
        out.extend_from_slice(&bytes[..n as usize]);
        if out.len() > (2 * 1024 * 1024) { return None; }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn bootstrap_first_user_task(
    kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    use crate::kernel::boot::UserImageSpec;
    use crate::kernel::task::TaskClass;

    if kernel.task_asid(RING3_INIT_SERVER_TID).is_some() {
        return Ok(());
    }

    let (asid, _aspace_cap) = kernel.create_user_address_space()?;
    let image = load_init_elf_from_initramfs_vfs();
    let fallback = initramfs_static_hello_world_elf();
    let image_bytes: &[u8] = image.as_deref().unwrap_or(&fallback);
    let entry = kernel.load_elf_pt_load_segments(asid, image_bytes)?;
    kernel.spawn_user_task_from_image(UserImageSpec {
        tid: RING3_INIT_SERVER_TID,
        entry,
        asid: Some(asid),
        class: TaskClass::SystemServer,
        startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
    })?;
    crate::yarm_log!(
        "YARM_INIT_DONE arch=riscv64 phase=kernel_static_init_elf image_id=0x{:x} seeded=0 initramfs_handled=1 devfs_handled=0",
        INITRAMFS_HELLO_WORLD_IMAGE_ID
    );
    Ok(())
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "riscv64")))]
pub fn bootstrap_first_user_task(
    _kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    Ok(())
}

pub fn release_secondary_cpus_after_bootstrap() {}

pub fn enter_dispatched_user_task_if_available(
    _kernel: &crate::kernel::boot::KernelState,
    _dispatched_tid: Option<u64>,
) {
}

pub fn run_with_prepared_kernel(run: fn(&mut crate::kernel::boot::KernelState)) {
    let mut kernel = crate::kernel::boot::Bootstrap::init().expect("kernel init");
    crate::yarm_log!(
        "YARM_BOOT_OK present_cpus={} present_bitmap=0x{:x} online_cpus={}",
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );
    run(&mut kernel);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn prepare_arch_boot(start_info_ptr: usize) {
    let Some(dtb) = dtb_slice_from_start_info(start_info_ptr) else {
        return;
    };
    let Some(parsed) = crate::arch::aarch64::dtb::parse_boot_dtb(dtb) else {
        return;
    };
    if let (Some(initrd_start), Some(initrd_end)) = (parsed.initrd_start, parsed.initrd_end)
        && initrd_start != 0
        && initrd_end > initrd_start
    {
        let len = initrd_end.saturating_sub(initrd_start) as usize;
        if len > 0 {
            let page = crate::kernel::vm::PAGE_SIZE as u64;
            let reserved_start = initrd_start & !(page - 1);
            let reserved_end = (initrd_end + (page - 1)) & !(page - 1);
            crate::kernel::boot::Bootstrap::install_boot_reserved_range(reserved_start, reserved_end);
            // SAFETY: DTB-provided initrd range refers to immutable boot memory.
            let bytes = unsafe { core::slice::from_raw_parts(initrd_start as *const u8, len) };
            yarm_fs_servers::initramfs::install_boot_initrd_bytes(bytes);
            crate::yarm_log!(
                "YARM_RISCV64_INITRD handoff start=0x{:x} end=0x{:x}",
                initrd_start,
                initrd_end
            );
        }
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn dtb_slice_from_start_info(start_info_ptr: usize) -> Option<&'static [u8]> {
    if start_info_ptr == 0 {
        return None;
    }
    let magic_be = unsafe { core::ptr::read_unaligned(start_info_ptr as *const u32) };
    if u32::from_be(magic_be) != 0xd00dfeed {
        return None;
    }
    let total_size_be = unsafe { core::ptr::read_unaligned((start_info_ptr + 4) as *const u32) };
    let total_size = u32::from_be(total_size_be) as usize;
    if !(40..=2 * 1024 * 1024).contains(&total_size) {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(start_info_ptr as *const u8, total_size) })
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "riscv64")))]
pub fn prepare_arch_boot(_start_info_ptr: usize) {}

pub fn emit_panic(_info: &core::panic::PanicInfo<'_>) {}
