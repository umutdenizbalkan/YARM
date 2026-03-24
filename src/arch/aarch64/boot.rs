#[cfg(not(feature = "hosted-dev"))]
unsafe extern "C" {
    fn yarm_kernel_main() -> !;
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    unsafe { yarm_kernel_main() }
}
