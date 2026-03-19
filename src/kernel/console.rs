#[cfg(not(feature = "hosted-dev"))]
const COM1_PORT: u16 = 0x3F8;
#[cfg(not(feature = "hosted-dev"))]
const COM1_LINE_STATUS: u16 = COM1_PORT + 5;
#[cfg(not(feature = "hosted-dev"))]
const LINE_STATUS_THR_EMPTY: u8 = 1 << 5;

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
    while (inb(COM1_LINE_STATUS) & LINE_STATUS_THR_EMPTY) == 0 {}
    outb(COM1_PORT, byte);
}

#[cfg(not(feature = "hosted-dev"))]
fn outb(port: u16, value: u8) {
    // SAFETY: Raw x86 port I/O is the required mechanism for the early serial console.
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[cfg(not(feature = "hosted-dev"))]
fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: Raw x86 port I/O is the required mechanism for the early serial console.
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}
