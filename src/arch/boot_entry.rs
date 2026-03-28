use core::sync::atomic::{AtomicUsize, Ordering};

const MAX_IRQ_DESCRIPTION_BYTES: usize = 256;
static IRQ_DESCRIPTION_LEN: AtomicUsize = AtomicUsize::new(0);
static mut IRQ_DESCRIPTION_BUF: [u8; MAX_IRQ_DESCRIPTION_BYTES] = [0; MAX_IRQ_DESCRIPTION_BYTES];

pub fn stage_irq_controller_description_for_boot(description: &[u8]) -> bool {
    if description.is_empty() || description.len() > MAX_IRQ_DESCRIPTION_BYTES {
        return false;
    }
    unsafe {
        IRQ_DESCRIPTION_BUF[..description.len()].copy_from_slice(description);
    }
    IRQ_DESCRIPTION_LEN.store(description.len(), Ordering::Release);
    true
}

pub fn stage_irq_controller_description_from_firmware_blob(blob: &[u8]) -> bool {
    fn parse_first(blob: &[u8], keys: &[&str]) -> Option<usize> {
        for key in keys {
            if let Some(value) = crate::arch::irq_description::parse_usize_token(blob, key) {
                return Some(value);
            }
        }
        None
    }

    fn push_byte(dst: &mut [u8], len: &mut usize, byte: u8) -> bool {
        if *len >= dst.len() {
            return false;
        }
        dst[*len] = byte;
        *len += 1;
        true
    }

    fn push_str(dst: &mut [u8], len: &mut usize, value: &str) -> bool {
        for byte in value.as_bytes() {
            if !push_byte(dst, len, *byte) {
                return false;
            }
        }
        true
    }

    fn push_hex(dst: &mut [u8], len: &mut usize, value: usize) -> bool {
        if !push_str(dst, len, "0x") {
            return false;
        }
        let mut started = false;
        for shift in (0..(core::mem::size_of::<usize>() * 8)).rev().step_by(4) {
            let nibble = ((value >> shift) & 0xF) as u8;
            if nibble == 0 && !started && shift != 0 {
                continue;
            }
            started = true;
            let ch = if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + (nibble - 10)
            };
            if !push_byte(dst, len, ch) {
                return false;
            }
        }
        true
    }

    let mut canonical = [0u8; MAX_IRQ_DESCRIPTION_BYTES];
    let mut canonical_len = 0usize;

    #[cfg(target_arch = "x86_64")]
    let valid = if let Some(base) = parse_first(
        blob,
        &["lapic_mmio_base", "lapic_base", "apic_base", "LAPIC_BASE"],
    ) {
        push_str(&mut canonical, &mut canonical_len, "lapic_mmio_base=")
            && push_hex(&mut canonical, &mut canonical_len, base)
    } else {
        false
    };
    #[cfg(target_arch = "riscv64")]
    let valid = if let (Some(base), Some(context)) = (
        parse_first(blob, &["plic_mmio_base", "plic_base", "PLIC_BASE"]),
        parse_first(
            blob,
            &["plic_smode_context", "plic_context", "PLIC_CONTEXT"],
        ),
    ) {
        push_str(&mut canonical, &mut canonical_len, "plic_mmio_base=")
            && push_hex(&mut canonical, &mut canonical_len, base)
            && push_byte(&mut canonical, &mut canonical_len, b' ')
            && push_str(&mut canonical, &mut canonical_len, "plic_smode_context=")
            && push_hex(&mut canonical, &mut canonical_len, context)
    } else {
        false
    };
    #[cfg(target_arch = "aarch64")]
    let valid = if let Some(base) = parse_first(
        blob,
        &["gic_cpu_if_base", "gicc_base", "gic_cpu_base", "GICC_BASE"],
    ) {
        push_str(&mut canonical, &mut canonical_len, "gic_cpu_if_base=")
            && push_hex(&mut canonical, &mut canonical_len, base)
    } else {
        false
    };
    if !valid {
        return false;
    }
    stage_irq_controller_description_for_boot(&canonical[..canonical_len])
}

fn take_staged_irq_description<'a>(
    scratch: &'a mut [u8; MAX_IRQ_DESCRIPTION_BYTES],
) -> Option<&'a [u8]> {
    let len = IRQ_DESCRIPTION_LEN.swap(0, Ordering::AcqRel);
    if len == 0 || len > MAX_IRQ_DESCRIPTION_BYTES {
        return None;
    }
    unsafe {
        scratch[..len].copy_from_slice(&IRQ_DESCRIPTION_BUF[..len]);
    }
    Some(&scratch[..len])
}

/// Selected-ISA boot entry facade used by top-level binaries.
#[inline]
pub fn run_kernel_boot_with_irq_description(run: fn(), irq_description: Option<&[u8]>) {
    let configured_from_description = irq_description.is_some_and(|description| {
        super::irq_guard::configure_external_irq_controller_from_description(description)
    });
    if !configured_from_description {
        super::irq_guard::configure_external_irq_controller_from_platform_layout();
    }
    run();
}

#[inline]
pub fn run_kernel_boot(run: fn()) {
    let mut staged = [0u8; MAX_IRQ_DESCRIPTION_BYTES];
    if let Some(description) = take_staged_irq_description(&mut staged) {
        return run_kernel_boot_with_irq_description(run, Some(description));
    }

    #[cfg(feature = "hosted-dev")]
    let irq_description = crate::std::env::var("YARM_IRQ_CONTROLLER_DESCRIPTION")
        .ok()
        .map(|s| s.into_bytes());
    #[cfg(feature = "hosted-dev")]
    return run_kernel_boot_with_irq_description(run, irq_description.as_deref());

    #[cfg(not(feature = "hosted-dev"))]
    run_kernel_boot_with_irq_description(run, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn boot_entry_accepts_explicit_irq_description() {
        crate::arch::x86_64::irq::reset_lapic_config_for_test();
        run_kernel_boot_with_irq_description(|| {}, Some(b"lapic_mmio_base=0xfee01000,ignored=1"));
        assert_eq!(
            crate::arch::x86_64::irq::lapic_mmio_base_for_test(),
            0xFEE0_1000
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn staged_description_is_consumed_once() {
        crate::arch::x86_64::irq::reset_lapic_config_for_test();
        assert!(stage_irq_controller_description_for_boot(
            b"lapic_mmio_base=0xfee02000"
        ));
        run_kernel_boot(|| {});
        assert_eq!(
            crate::arch::x86_64::irq::lapic_mmio_base_for_test(),
            0xFEE0_2000
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
}
