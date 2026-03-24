/// Selected-ISA boot entry facade used by top-level binaries.
#[inline]
pub fn run_kernel_boot(run: fn()) {
    run();
}
