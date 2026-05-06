// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

const MAX_IRQ_DESCRIPTION_BYTES: usize = 256;
const MAX_STAGED_BOOT_RAM_REGIONS: usize = 16;
static IRQ_DESCRIPTION_LEN: AtomicUsize = AtomicUsize::new(0);
static IRQ_DESCRIPTION_LOCK: AtomicBool = AtomicBool::new(false);
static mut IRQ_DESCRIPTION_BUF: [u8; MAX_IRQ_DESCRIPTION_BYTES] = [0; MAX_IRQ_DESCRIPTION_BYTES];
static FIRMWARE_BLOB_PROVIDER_PTR: AtomicUsize = AtomicUsize::new(0);
static STAGED_BOOT_RAM_REGIONS_LEN: AtomicUsize = AtomicUsize::new(0);
static STAGED_BOOT_RAM_REGIONS_LOCK: AtomicBool = AtomicBool::new(false);
static STAGED_PRESENT_CPU_BITMAP: AtomicU64 = AtomicU64::new(0);
static mut STAGED_CPU_APIC_IDS: [u8; crate::kernel::scheduler::MAX_CPUS] = [0xff; crate::kernel::scheduler::MAX_CPUS];
static mut STAGED_BOOT_RAM_REGIONS: [crate::kernel::frame_allocator::MemoryRegion;
    MAX_STAGED_BOOT_RAM_REGIONS] = [crate::kernel::frame_allocator::MemoryRegion {
    start: 0,
    len: 0,
    usable: false,
}; MAX_STAGED_BOOT_RAM_REGIONS];

pub fn bootstrap_first_user_task(
    kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    crate::arch::selected_isa::boot::bootstrap_first_user_task(kernel)
}

pub fn release_secondary_cpus_after_bootstrap() {
    crate::arch::selected_isa::boot::release_secondary_cpus_after_bootstrap()
}

pub fn enter_dispatched_user_task_if_available(
    kernel: &crate::kernel::boot::KernelState,
    dispatched_tid: Option<u64>,
) {
    crate::arch::selected_isa::boot::enter_dispatched_user_task_if_available(kernel, dispatched_tid)
}

pub fn run_with_prepared_kernel(run: fn(&mut crate::kernel::boot::KernelState)) {
    crate::arch::selected_isa::boot::run_with_prepared_kernel(run)
}

pub fn prepare_arch_boot(start_info_ptr: usize) {
    crate::arch::selected_isa::boot::prepare_arch_boot(start_info_ptr)
}

pub fn emit_panic(info: &core::panic::PanicInfo<'_>) {
    crate::arch::selected_isa::boot::emit_panic(info)
}

struct IrqDescriptionLockGuard;

impl IrqDescriptionLockGuard {
    fn acquire() -> Self {
        while IRQ_DESCRIPTION_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Self
    }
}

impl Drop for IrqDescriptionLockGuard {
    fn drop(&mut self) {
        IRQ_DESCRIPTION_LOCK.store(false, Ordering::Release);
    }
}

struct StagedBootRamLockGuard;

impl StagedBootRamLockGuard {
    fn acquire() -> Self {
        while STAGED_BOOT_RAM_REGIONS_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Self
    }
}

impl Drop for StagedBootRamLockGuard {
    fn drop(&mut self) {
        STAGED_BOOT_RAM_REGIONS_LOCK.store(false, Ordering::Release);
    }
}

pub fn stage_irq_controller_description_for_boot(description: &[u8]) -> bool {
    if description.is_empty() || description.len() > MAX_IRQ_DESCRIPTION_BYTES {
        return false;
    }
    let _guard = IrqDescriptionLockGuard::acquire();
    unsafe {
        IRQ_DESCRIPTION_BUF[..description.len()].copy_from_slice(description);
    }
    IRQ_DESCRIPTION_LEN.store(description.len(), Ordering::Release);
    true
}

pub fn stage_irq_controller_description_from_firmware_blob(blob: &[u8]) -> bool {
    let mut canonical = [0u8; MAX_IRQ_DESCRIPTION_BYTES];
    let Some(canonical_len) =
        crate::arch::topology::discover_irq_controller_description(blob, &mut canonical)
    else {
        return false;
    };
    stage_irq_controller_description_for_boot(&canonical[..canonical_len])
}

pub fn stage_detected_ram_for_bootstrap(
    regions: &[crate::kernel::frame_allocator::MemoryRegion],
) -> bool {
    if regions.is_empty() || regions.len() > MAX_STAGED_BOOT_RAM_REGIONS {
        return false;
    }
    let mut validated = [crate::kernel::frame_allocator::MemoryRegion {
        start: 0,
        len: 0,
        usable: false,
    }; MAX_STAGED_BOOT_RAM_REGIONS];
    let mut validated_len = 0usize;
    for region in regions.iter().copied() {
        if !region.usable || region.len == 0 {
            continue;
        }
        if validated_len >= MAX_STAGED_BOOT_RAM_REGIONS {
            return false;
        }
        validated[validated_len] = region;
        validated_len += 1;
    }
    if validated_len == 0 {
        return false;
    }

    let _guard = StagedBootRamLockGuard::acquire();
    unsafe {
        STAGED_BOOT_RAM_REGIONS[..validated_len].copy_from_slice(&validated[..validated_len]);
    }
    STAGED_BOOT_RAM_REGIONS_LEN.store(validated_len, Ordering::Release);
    true
}

pub fn take_staged_ram_for_bootstrap<'a>(
    scratch: &'a mut [crate::kernel::frame_allocator::MemoryRegion],
) -> Option<&'a [crate::kernel::frame_allocator::MemoryRegion]> {
    let len = STAGED_BOOT_RAM_REGIONS_LEN.swap(0, Ordering::AcqRel);
    if len == 0 || len > MAX_STAGED_BOOT_RAM_REGIONS || len > scratch.len() {
        return None;
    }

    let _guard = StagedBootRamLockGuard::acquire();
    unsafe {
        scratch[..len].copy_from_slice(&STAGED_BOOT_RAM_REGIONS[..len]);
    }
    Some(&scratch[..len])
}

pub fn stage_present_cpu_bitmap_for_bootstrap(bitmap: u64) -> bool {
    if bitmap == 0 {
        return false;
    }
    STAGED_PRESENT_CPU_BITMAP.store(bitmap, Ordering::Release);
    true
}

pub fn take_staged_present_cpu_bitmap_for_bootstrap() -> Option<u64> {
    let bitmap = STAGED_PRESENT_CPU_BITMAP.swap(0, Ordering::AcqRel);
    if bitmap == 0 { None } else { Some(bitmap) }
}

pub fn stage_cpu_apic_ids_for_bootstrap(apic_ids: &[Option<u8>; crate::kernel::scheduler::MAX_CPUS]) {
    unsafe {
        let base = core::ptr::addr_of_mut!(STAGED_CPU_APIC_IDS).cast::<u8>();
        let mut idx = 0usize;
        while idx < crate::kernel::scheduler::MAX_CPUS {
            core::ptr::write(base.add(idx), apic_ids[idx].unwrap_or(0xff));
            idx += 1;
        }
    }
}

pub fn take_staged_cpu_apic_ids_for_bootstrap() -> [Option<u8>; crate::kernel::scheduler::MAX_CPUS] {
    let mut out = [None; crate::kernel::scheduler::MAX_CPUS];
    unsafe {
        let base = core::ptr::addr_of_mut!(STAGED_CPU_APIC_IDS).cast::<u8>();
        let mut idx = 0usize;
        while idx < crate::kernel::scheduler::MAX_CPUS {
            let raw = core::ptr::read(base.add(idx));
            if raw != 0xff {
                out[idx] = Some(raw);
            }
            core::ptr::write(base.add(idx), 0xff);
            idx += 1;
        }
    }
    out
}

pub fn set_firmware_blob_provider_for_boot(provider: fn(&mut [u8]) -> usize) {
    FIRMWARE_BLOB_PROVIDER_PTR.store(provider as usize, Ordering::Release);
}

#[inline]
pub fn run_kernel_boot_with_firmware_blob(run: fn(), firmware_blob: Option<&[u8]>) {
    if let Some(blob) = firmware_blob {
        let present = crate::arch::topology::discover_present_cpu_bitmap(blob);
        let _ = stage_present_cpu_bitmap_for_bootstrap(present);
        stage_cpu_apic_ids_for_bootstrap(&crate::arch::topology::discover_cpu_apic_ids(blob));
        let mut canonical = [0u8; MAX_IRQ_DESCRIPTION_BYTES];
        if let Some(canonical_len) =
            crate::arch::topology::discover_irq_controller_description(blob, &mut canonical)
        {
            return run_kernel_boot_with_irq_description(run, Some(&canonical[..canonical_len]));
        }
    }
    run_kernel_boot_with_irq_description(run, None);
}

fn take_staged_irq_description<'a>(
    scratch: &'a mut [u8; MAX_IRQ_DESCRIPTION_BYTES],
) -> Option<&'a [u8]> {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    crate::arch::x86_64::console::write_line("TS0");
    let len = IRQ_DESCRIPTION_LEN.swap(0, Ordering::AcqRel);
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    crate::arch::x86_64::console::write_line("TS1");
    if len == 0 || len > MAX_IRQ_DESCRIPTION_BYTES {
        return None;
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    crate::arch::x86_64::console::write_line("TS2");
    let _guard = IrqDescriptionLockGuard::acquire();
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    crate::arch::x86_64::console::write_line("TS3");
    unsafe {
        scratch[..len].copy_from_slice(&IRQ_DESCRIPTION_BUF[..len]);
    }
    Some(&scratch[..len])
}

fn take_irq_firmware_blob_from_provider<'a>(
    scratch: &'a mut [u8; MAX_IRQ_DESCRIPTION_BYTES],
) -> Option<&'a [u8]> {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    crate::arch::x86_64::console::write_line("TP0");
    let provider_ptr = FIRMWARE_BLOB_PROVIDER_PTR.load(Ordering::Acquire);
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    crate::arch::x86_64::console::write_line("TP1");
    if provider_ptr == 0 {
        return None;
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    crate::arch::x86_64::console::write_line("TP2");
    let provider: fn(&mut [u8]) -> usize = unsafe { core::mem::transmute(provider_ptr) };
    let len = provider(scratch);
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    crate::arch::x86_64::console::write_line("TP3");
    if len == 0 || len > scratch.len() {
        return None;
    }
    Some(&scratch[..len])
}

/// Selected-ISA boot entry facade used by top-level binaries.
#[inline]
pub fn run_kernel_boot_with_irq_description(run: fn(), irq_description: Option<&[u8]>) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    crate::arch::x86_64::console::write_line("BE0");
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    let _ = irq_description;
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    {
        // Defer external IRQ controller programming until kernel state is installed.
        // Early pre-kernel MMIO programming can fault before run_with_prepared_kernel executes.
    }
    #[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
    {
        let configured_from_description = irq_description.is_some_and(|description| {
            super::irq_guard::configure_external_irq_controller_from_description(description)
        });
        if !configured_from_description {
            super::irq_guard::configure_external_irq_controller_from_platform_layout();
        }
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    crate::arch::x86_64::console::write_line("BE1");
    run();
}

#[inline]
pub fn run_kernel_boot(run: fn()) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    crate::arch::x86_64::console::write_line("BK0");
    let mut staged = [0u8; MAX_IRQ_DESCRIPTION_BYTES];
    if let Some(description) = take_staged_irq_description(&mut staged) {
        #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
        crate::arch::x86_64::console::write_line("BK1");
        return run_kernel_boot_with_irq_description(run, Some(description));
    }
    if let Some(blob) = take_irq_firmware_blob_from_provider(&mut staged) {
        #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
        crate::arch::x86_64::console::write_line("BK2");
        return run_kernel_boot_with_firmware_blob(run, Some(blob));
    }

    #[cfg(feature = "hosted-dev")]
    let irq_description = crate::std::env::var("YARM_IRQ_CONTROLLER_DESCRIPTION").ok();
    #[cfg(feature = "hosted-dev")]
    if let Some(irq_description) = irq_description {
        return run_kernel_boot_with_irq_description(run, Some(irq_description.as_bytes()));
    }

    #[cfg(feature = "hosted-dev")]
    if let Ok(firmware_blob) = crate::std::env::var("YARM_IRQ_FIRMWARE_BLOB") {
        return run_kernel_boot_with_firmware_blob(run, Some(firmware_blob.as_bytes()));
    }

    #[cfg(not(feature = "hosted-dev"))]
    #[cfg(target_arch = "x86_64")]
    crate::arch::x86_64::console::write_line("BK9");
    #[cfg(not(feature = "hosted-dev"))]
    run_kernel_boot_with_irq_description(run, None);
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;
    #[cfg(target_arch = "x86_64")]
    use crate::std::string::String;

    #[cfg(target_arch = "x86_64")]
    fn lapic_description_for_test(base: usize) -> String {
        crate::std::format!("lapic_mmio_base=0x{base:x}")
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn boot_entry_accepts_explicit_irq_description() {
        let mut regs = [0u32; 512];
        let desc = crate::std::format!(
            "{},ignored=1",
            lapic_description_for_test(regs.as_mut_ptr() as usize)
        );
        crate::arch::x86_64::irq::reset_lapic_config_for_test();
        run_kernel_boot_with_irq_description(|| {}, Some(desc.as_bytes()));
        assert_eq!(
            crate::arch::x86_64::irq::lapic_mmio_base_for_test(),
            regs.as_mut_ptr() as usize
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn staged_description_is_consumed_once() {
        let mut regs = [0u32; 512];
        let desc = lapic_description_for_test(regs.as_mut_ptr() as usize);
        crate::arch::x86_64::irq::reset_lapic_config_for_test();
        assert!(stage_irq_controller_description_for_boot(desc.as_bytes()));
        run_kernel_boot(|| {});
        assert_eq!(
            crate::arch::x86_64::irq::lapic_mmio_base_for_test(),
            regs.as_mut_ptr() as usize
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn firmware_blob_staging_validates_required_fields() {
        assert!(!stage_irq_controller_description_from_firmware_blob(
            b"cpu@0 enabled=1"
        ));
        assert!(stage_irq_controller_description_from_firmware_blob(
            b"lapic_mmio_base=0xfee03000"
        ));
        assert!(stage_irq_controller_description_from_firmware_blob(
            b"LAPIC_BASE=0xfee04000"
        ));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn boot_entry_accepts_firmware_blob_path() {
        let mut regs = [0u32; 512];
        let blob = crate::std::format!("LAPIC_BASE=0x{:x}", regs.as_mut_ptr() as usize);
        crate::arch::x86_64::irq::reset_lapic_config_for_test();
        run_kernel_boot_with_firmware_blob(|| {}, Some(blob.as_bytes()));
        assert_eq!(
            crate::arch::x86_64::irq::lapic_mmio_base_for_test(),
            regs.as_mut_ptr() as usize
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn boot_entry_uses_registered_firmware_blob_provider() {
        static mut TEST_LAPIC_REGS: [u32; 512] = [0; 512];

        fn provider(buf: &mut [u8]) -> usize {
            let base = core::ptr::addr_of_mut!(TEST_LAPIC_REGS) as usize;
            let blob = crate::std::format!("LAPIC_BASE=0x{base:x}");
            buf[..blob.len()].copy_from_slice(blob.as_bytes());
            blob.len()
        }
        crate::arch::x86_64::irq::reset_lapic_config_for_test();
        set_firmware_blob_provider_for_boot(provider);
        run_kernel_boot(|| {});
        assert_eq!(
            crate::arch::x86_64::irq::lapic_mmio_base_for_test(),
            core::ptr::addr_of_mut!(TEST_LAPIC_REGS) as usize
        );
    }

    #[test]
    fn staged_bootstrap_ram_regions_are_consumed_once() {
        let regions = [crate::kernel::frame_allocator::MemoryRegion {
            start: 0x4000_0000,
            len: 0x2000_0000,
            usable: true,
        }];
        assert!(stage_detected_ram_for_bootstrap(&regions));
        let mut scratch = [crate::kernel::frame_allocator::MemoryRegion {
            start: 0,
            len: 0,
            usable: false,
        }; 4];
        let consumed = take_staged_ram_for_bootstrap(&mut scratch).expect("staged ram");
        assert_eq!(consumed, &regions);
        assert!(take_staged_ram_for_bootstrap(&mut scratch).is_none());
    }

    #[test]
    fn stage_detected_ram_rejects_empty_or_unusable_regions() {
        assert!(!stage_detected_ram_for_bootstrap(&[]));
        let unusable = [crate::kernel::frame_allocator::MemoryRegion {
            start: 0x4000_0000,
            len: 0x1000,
            usable: false,
        }];
        assert!(!stage_detected_ram_for_bootstrap(&unusable));
    }

    #[test]
    fn staged_present_cpu_bitmap_is_consumed_once() {
        assert!(stage_present_cpu_bitmap_for_bootstrap(0b1111));
        assert_eq!(take_staged_present_cpu_bitmap_for_bootstrap(), Some(0b1111));
        assert_eq!(take_staged_present_cpu_bitmap_for_bootstrap(), None);
        assert!(!stage_present_cpu_bitmap_for_bootstrap(0));
    }
}
