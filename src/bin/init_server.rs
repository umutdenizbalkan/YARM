#![no_std]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

#[inline]
fn run() {
    yarm::services::control_plane::init::run();
}

#[cfg(feature = "hosted-dev")]
fn main() {
    run();
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    run();
    loop {}
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
