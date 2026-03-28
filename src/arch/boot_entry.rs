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
}
