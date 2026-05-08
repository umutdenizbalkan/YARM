// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub fn write_line(msg: &str) {
    crate::arch::selected_isa::console::write_line(msg);
}

pub fn write_byte(byte: u8) {
    crate::arch::selected_isa::console::write_byte(byte);
}

pub fn try_write_byte(byte: u8) -> bool {
    crate::arch::selected_isa::console::try_write_byte(byte)
}

pub fn try_write_bytes(bytes: &[u8]) -> bool {
    #[cfg(feature = "hosted-dev")]
    {
        let _ = bytes;
        false
    }
    #[cfg(not(feature = "hosted-dev"))]
    {
        use crate::kernel::lock::SpinLockIrq;
        static DEBUG_SERIAL_WRITE_LOCK: SpinLockIrq<()> = SpinLockIrq::new(());
        let _guard = DEBUG_SERIAL_WRITE_LOCK.lock();
        for &byte in bytes {
            if !crate::arch::selected_isa::console::try_write_byte(byte) {
                return false;
            }
        }
        true
    }
}
