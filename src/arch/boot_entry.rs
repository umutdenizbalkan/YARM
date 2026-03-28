/// Selected-ISA boot entry facade used by top-level binaries.
#[inline]
pub fn run_kernel_boot(run: fn()) {
    #[cfg(feature = "hosted-dev")]
    {
        if let Ok(description) = crate::std::env::var("YARM_IRQ_CONTROLLER_DESCRIPTION") {
            let configured = super::irq_guard::configure_external_irq_controller_from_description(
                description.as_bytes(),
            );
            if !configured {
                super::irq_guard::configure_external_irq_controller_from_platform_layout();
            }
        } else {
            super::irq_guard::configure_external_irq_controller_from_platform_layout();
        }
    }
    #[cfg(not(feature = "hosted-dev"))]
    super::irq_guard::configure_external_irq_controller_from_platform_layout();
    run();
}
