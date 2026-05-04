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
const INITRD_INIT_ELF_MAX_SIZE: usize = 16 * 1024 * 1024;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn load_init_elf_from_initramfs_vfs() -> Option<alloc::vec::Vec<u8>> {
    let bytes = crate::kernel::boot::Bootstrap::boot_initrd_bytes()?;
    let entry = yarm_srv_common::cpio::CpioArchive::new(bytes)
        .find("/init")
        .ok()
        .flatten()
        .or_else(|| yarm_srv_common::cpio::CpioArchive::new(bytes).find("init").ok().flatten())?;
    let file_data = entry.file_data();
    if file_data.len() > INITRD_INIT_ELF_MAX_SIZE {
        crate::yarm_log!(
            "YARM_INITRD_INIT_TOO_LARGE len={} cap={}",
            file_data.len(),
            INITRD_INIT_ELF_MAX_SIZE
        );
        return None;
    }
    Some(alloc::vec::Vec::from(file_data))
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
    let (entry, heap_base) = kernel.load_elf_pt_load_segments(asid, image_bytes)?;
    kernel.register_task_with_class(RING3_INIT_SERVER_TID, TaskClass::SystemServer)?;
    let (_pm_eid, pm_send_cap_root, pm_recv_cap_root) = kernel.create_endpoint(8)?;
    let pm_request_send_init = kernel.grant_capability_task_to_task_with_rights(
        0,
        pm_send_cap_root,
        RING3_INIT_SERVER_TID,
        crate::kernel::capabilities::CapRights::SEND,
    )?;
    let pm_reply_recv_init = kernel.grant_capability_task_to_task_with_rights(
        0,
        pm_recv_cap_root,
        RING3_INIT_SERVER_TID,
        crate::kernel::capabilities::CapRights::RECEIVE,
    )?;
    let (_sup_eid, _sup_send_root, sup_fault_recv_root) = kernel.create_endpoint(8)?;
    let supervisor_fault_recv_init = kernel.grant_capability_task_to_task_with_rights(
        0,
        sup_fault_recv_root,
        RING3_INIT_SERVER_TID,
        crate::kernel::capabilities::CapRights::RECEIVE,
    )?;
    kernel.set_supervisor_endpoint_for_task(RING3_INIT_SERVER_TID, supervisor_fault_recv_init)?;
    let mut startup_args = UserImageSpec::DEFAULT_STARTUP_ARGS;
    startup_args[0] = RING3_INIT_SERVER_TID;
    startup_args[1] = pm_request_send_init.0;
    startup_args[2] = pm_reply_recv_init.0;
    if startup_args.len() > 3 {
        startup_args[3] = supervisor_fault_recv_init.0;
    }
    crate::yarm_log!(
        "YARM_FIRST_USER_STARTUP_ARGS tid={} arg0={} arg1={} arg2={} arg3={}",
        RING3_INIT_SERVER_TID,
        startup_args[0],
        startup_args[1],
        startup_args[2],
        startup_args[3]
    );
    kernel.spawn_user_task_from_image(UserImageSpec {
        tid: RING3_INIT_SERVER_TID,
        entry,
        asid: Some(asid),
        class: TaskClass::SystemServer,
        startup_args,
    })?;
    kernel.set_task_brk_bounds(RING3_INIT_SERVER_TID, heap_base, heap_base)?;
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
    let _ = dtb;
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
