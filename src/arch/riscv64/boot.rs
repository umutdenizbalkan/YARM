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
const RING3_SUPERVISOR_TID: u64 = 2;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const RING3_PM_SERVER_TID: u64 = 3;
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
fn load_supervisor_elf_from_initramfs_vfs() -> Option<alloc::vec::Vec<u8>> {
    let bytes = crate::kernel::boot::Bootstrap::boot_initrd_bytes()?;
    let entry = yarm_srv_common::cpio::CpioArchive::new(bytes)
        .find("sbin/supervisor")
        .ok()
        .flatten()?;
    let file_data = entry.file_data();
    if file_data.len() > INITRD_INIT_ELF_MAX_SIZE {
        return None;
    }
    Some(alloc::vec::Vec::from(file_data))
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn load_pm_elf_from_initramfs_vfs() -> Option<alloc::vec::Vec<u8>> {
    let bytes = crate::kernel::boot::Bootstrap::boot_initrd_bytes()?;
    let entry = yarm_srv_common::cpio::CpioArchive::new(bytes)
        .find("sbin/process_manager")
        .ok()
        .flatten()?;
    let file_data = entry.file_data();
    if file_data.len() > INITRD_INIT_ELF_MAX_SIZE {
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

    let (init_asid, _) = kernel.create_user_address_space()?;
    let init_image = load_init_elf_from_initramfs_vfs();
    let init_fallback = initramfs_static_hello_world_elf();
    let (init_bytes, init_source): (&[u8], &str) = match init_image.as_deref() {
        Some(img) => (img, "initrd"),
        None => (&init_fallback, "synthetic"),
    };
    let init_elf_info = yarm_srv_common::elf::ElfImageInfo::parse(0, init_bytes)
        .map_err(|_| crate::kernel::boot::KernelError::WrongObject)?;
    let (_, init_first_pt_load, init_heap) =
        kernel.load_elf_pt_load_segments(init_asid, init_bytes)?;
    let init_entry = init_elf_info.entry as usize;
    crate::yarm_log!("INIT_ELF_HEADER_ENTRY value={:#x}", init_elf_info.entry);
    crate::yarm_log!("INIT_FIRST_PT_LOAD_VADDR value={:#x}", init_first_pt_load);
    crate::yarm_log!("INIT_SELECTED_ENTRY value={:#x}", init_entry);
    if init_entry == init_first_pt_load {
        crate::yarm_log!("INIT_ENTRY_EQUALS_FIRST_PT_LOAD_WARN: ELF e_entry matches first PT_LOAD base; entry may be wrong");
    }
    crate::yarm_log!(
        "YARM_INITRD_INIT_ELF_SELECTED entry={:#x} source={}",
        init_entry,
        init_source
    );

    let supervisor_image = load_supervisor_elf_from_initramfs_vfs();
    let supervisor_aei: Option<(_, usize, usize)> = if let Some(ref sup_bytes) = supervisor_image {
        let sup_elf_info = yarm_srv_common::elf::ElfImageInfo::parse(1, sup_bytes)
            .map_err(|_| crate::kernel::boot::KernelError::WrongObject)?;
        let (sup_asid, _) = kernel.create_user_address_space()?;
        let (_, sup_first_pt_load, sup_heap) =
            kernel.load_elf_pt_load_segments(sup_asid, sup_bytes)?;
        let sup_entry = sup_elf_info.entry as usize;
        crate::yarm_log!("SUPERVISOR_ELF_HEADER_ENTRY value={:#x}", sup_elf_info.entry);
        crate::yarm_log!("SUPERVISOR_FIRST_PT_LOAD_VADDR value={:#x}", sup_first_pt_load);
        crate::yarm_log!("SUPERVISOR_SELECTED_ENTRY value={:#x}", sup_entry);
        if sup_entry == sup_first_pt_load {
            crate::yarm_log!("SUPERVISOR_ENTRY_EQUALS_FIRST_PT_LOAD_WARN: ELF e_entry matches first PT_LOAD base; entry may be wrong");
        }
        Some((sup_asid, sup_entry, sup_heap))
    } else {
        crate::yarm_log!("YARM_SUPERVISOR_ELF_MISSING path=sbin/supervisor");
        return Err(crate::kernel::boot::KernelError::MemoryObjectMissing);
    };

    let pm_image = load_pm_elf_from_initramfs_vfs();
    let pm_aei: Option<(_, usize, usize)> = if let Some(ref pm_bytes) = pm_image {
        let pm_elf_info = yarm_srv_common::elf::ElfImageInfo::parse(2, pm_bytes)
            .map_err(|_| crate::kernel::boot::KernelError::WrongObject)?;
        let (pm_asid, _) = kernel.create_user_address_space()?;
        let (_, pm_first_pt_load, pm_heap) =
            kernel.load_elf_pt_load_segments(pm_asid, pm_bytes)?;
        let pm_entry = pm_elf_info.entry as usize;
        crate::yarm_log!("PM_ELF_HEADER_ENTRY value={:#x}", pm_elf_info.entry);
        crate::yarm_log!("PM_FIRST_PT_LOAD_VADDR value={:#x}", pm_first_pt_load);
        crate::yarm_log!("PM_SELECTED_ENTRY value={:#x}", pm_entry);
        if pm_entry == pm_first_pt_load {
            crate::yarm_log!("PM_ENTRY_EQUALS_FIRST_PT_LOAD_WARN: ELF e_entry matches first PT_LOAD base; entry may be wrong");
        }
        Some((pm_asid, pm_entry, pm_heap))
    } else {
        crate::yarm_log!("YARM_PM_ELF_MISSING path=sbin/process_manager");
        return Err(crate::kernel::boot::KernelError::MemoryObjectMissing);
    };

    if supervisor_aei.is_some() {
        kernel.register_task_with_class(RING3_SUPERVISOR_TID, TaskClass::SystemServer)?;
    }
    if pm_aei.is_some() {
        kernel.register_task_with_class(RING3_PM_SERVER_TID, TaskClass::SystemServer)?;
    }
    kernel.register_task_with_class(RING3_INIT_SERVER_TID, TaskClass::SystemServer)?;

    let (_, pm_inbound_send_root, pm_inbound_recv_root) = kernel.create_endpoint(16)?;
    let pm_inbound_send_init = kernel.grant_capability_task_to_task_with_rights(
        0, pm_inbound_send_root, RING3_INIT_SERVER_TID,
        crate::kernel::capabilities::CapRights::SEND,
    )?;
    let pm_inbound_send_sup = if supervisor_aei.is_some() {
        Some(kernel.grant_capability_task_to_task_with_rights(
            0, pm_inbound_send_root, RING3_SUPERVISOR_TID,
            crate::kernel::capabilities::CapRights::SEND,
        )?)
    } else { None };
    let pm_inbound_recv_pm = if pm_aei.is_some() {
        Some(kernel.grant_capability_task_to_task_with_rights(
            0, pm_inbound_recv_root, RING3_PM_SERVER_TID,
            crate::kernel::capabilities::CapRights::RECEIVE,
        )?)
    } else { None };

    let (_, _, init_reply_recv_root) = kernel.create_endpoint(8)?;
    let init_reply_recv_init = kernel.grant_capability_task_to_task_with_rights(
        0, init_reply_recv_root, RING3_INIT_SERVER_TID,
        crate::kernel::capabilities::CapRights::RECEIVE,
    )?;

    let (_, _, sup_fault_recv_root) = kernel.create_endpoint(8)?;
    let sup_fault_recv_sup = if supervisor_aei.is_some() {
        Some(kernel.grant_capability_task_to_task_with_rights(
            0, sup_fault_recv_root, RING3_SUPERVISOR_TID,
            crate::kernel::capabilities::CapRights::RECEIVE,
        )?)
    } else { None };

    // EP4: Supervisor control — supervisor SEND (slot 4), supervisor RECV (slot 5).
    let (_, sup_ctrl_send_root, sup_ctrl_recv_root) = kernel.create_endpoint(8)?;
    let sup_ctrl_send_sup = if supervisor_aei.is_some() {
        Some(kernel.grant_capability_task_to_task_with_rights(
            0, sup_ctrl_send_root, RING3_SUPERVISOR_TID,
            crate::kernel::capabilities::CapRights::SEND,
        )?)
    } else { None };
    let sup_ctrl_recv_sup = if supervisor_aei.is_some() {
        Some(kernel.grant_capability_task_to_task_with_rights(
            0, sup_ctrl_recv_root, RING3_SUPERVISOR_TID,
            crate::kernel::capabilities::CapRights::RECEIVE,
        )?)
    } else { None };

    // EP5: Supervisor PM reply — supervisor gets RECV (slot 2); distinct from init's EP2.
    let (_, _, sup_pm_reply_recv_root) = kernel.create_endpoint(8)?;
    let sup_pm_reply_recv_sup = if supervisor_aei.is_some() {
        Some(kernel.grant_capability_task_to_task_with_rights(
            0, sup_pm_reply_recv_root, RING3_SUPERVISOR_TID,
            crate::kernel::capabilities::CapRights::RECEIVE,
        )?)
    } else { None };

    if let Some(fault_cap) = sup_fault_recv_sup {
        kernel.set_supervisor_endpoint_for_task(RING3_SUPERVISOR_TID, fault_cap)?;
    }

    if let Some((sup_asid, sup_entry, sup_heap)) = supervisor_aei {
        let mut sup_args = UserImageSpec::DEFAULT_STARTUP_ARGS;
        sup_args[0] = RING3_SUPERVISOR_TID;
        if let Some(c) = pm_inbound_send_sup   { sup_args[1] = c.0; }
        if let Some(c) = sup_pm_reply_recv_sup { sup_args[2] = c.0; }
        if let Some(c) = sup_fault_recv_sup    { sup_args[3] = c.0; }
        if let Some(c) = sup_ctrl_send_sup     { sup_args[4] = c.0; }
        if let Some(c) = sup_ctrl_recv_sup     { sup_args[5] = c.0; }
        sup_args[8] = RING3_INIT_SERVER_TID;
        for n in 0..10usize {
            crate::yarm_log!("SUP_STARTUP_SLOT slot={} value={}", n, sup_args[n]);
        }
        kernel.spawn_user_task_from_image(UserImageSpec {
            tid: RING3_SUPERVISOR_TID,
            entry: sup_entry,
            asid: Some(sup_asid),
            class: TaskClass::SystemServer,
            startup_args: sup_args,
        })?;
        kernel.set_task_brk_bounds(RING3_SUPERVISOR_TID, sup_heap, sup_heap)?;
        crate::yarm_log!("YARM_SUPERVISOR_TID2_SPAWNED tid={}", RING3_SUPERVISOR_TID);
    }

    if let Some((pm_asid, pm_entry, pm_heap)) = pm_aei {
        let mut pm_args = UserImageSpec::DEFAULT_STARTUP_ARGS;
        pm_args[0] = RING3_PM_SERVER_TID;
        if let Some(c) = pm_inbound_recv_pm { pm_args[17] = c.0; }
        kernel.spawn_user_task_from_image(UserImageSpec {
            tid: RING3_PM_SERVER_TID,
            entry: pm_entry,
            asid: Some(pm_asid),
            class: TaskClass::SystemServer,
            startup_args: pm_args,
        })?;
        kernel.set_task_brk_bounds(RING3_PM_SERVER_TID, pm_heap, pm_heap)?;
        crate::yarm_log!("YARM_PM_TID3_SPAWNED tid={}", RING3_PM_SERVER_TID);
    }

    let mut init_args = UserImageSpec::DEFAULT_STARTUP_ARGS;
    init_args[0] = RING3_INIT_SERVER_TID;
    init_args[1] = pm_inbound_send_init.0;
    init_args[2] = init_reply_recv_init.0;
    init_args[9] = RING3_SUPERVISOR_TID;
    crate::yarm_log!(
        "YARM_FIRST_USER_STARTUP_ARGS tid={} arg0={} arg1={} arg2={} arg3={}",
        RING3_INIT_SERVER_TID,
        init_args[0], init_args[1], init_args[2], init_args[3]
    );
    kernel.spawn_user_task_from_image(UserImageSpec {
        tid: RING3_INIT_SERVER_TID,
        entry: init_entry,
        asid: Some(init_asid),
        class: TaskClass::SystemServer,
        startup_args: init_args,
    })?;
    kernel.set_task_brk_bounds(RING3_INIT_SERVER_TID, init_heap, init_heap)?;
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
