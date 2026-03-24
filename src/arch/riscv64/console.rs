#[cfg(feature = "hosted-dev")]
pub fn write_line(_msg: &str) {}

#[cfg(not(feature = "hosted-dev"))]
pub fn write_line(msg: &str) {
    for &byte in msg.as_bytes() {
        if byte == b'\n' {
            write_byte(b'\r');
        }
        write_byte(byte);
    }
    write_byte(b'\r');
    write_byte(b'\n');
}

#[cfg(not(feature = "hosted-dev"))]
fn write_byte(byte: u8) {
    // Legacy SBI console_putchar (a7=1, a0=char, ecall).
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a0") byte as usize,
            in("a7") 1usize,
            options(nostack, preserves_flags)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_console_write_is_noop_safe() {
        #[cfg(feature = "hosted-dev")]
        write_line("riscv64-console");
    }
}
