#![no_std]

fn main() {
    yarm::services::control_plane::init::run();
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
