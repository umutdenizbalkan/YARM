/// Selected-ISA boot entry facade used by top-level binaries.
#[inline]
pub fn run_kernel_boot(run: fn()) {
    super::irq_guard::configure_external_irq_controller_from_platform_layout();
    run();
}
