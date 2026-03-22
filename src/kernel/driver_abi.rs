//! Driver-manager IPC protocol constants.

pub const DRIVER_SERVER_ABI_VERSION: u16 = 1;

pub const DRIVER_OP_REGISTER: u16 = 1;
pub const DRIVER_OP_GRANT_IRQ: u16 = 2;
pub const DRIVER_OP_GRANT_DMA: u16 = 3;
pub const DRIVER_OP_RESTARTED: u16 = 4;

pub fn pack_driver_pair(a: u64, b: u64) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&a.to_le_bytes());
    out[8..16].copy_from_slice(&b.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_abi_is_frozen() {
        assert_eq!(DRIVER_SERVER_ABI_VERSION, 1);
        assert_eq!(DRIVER_OP_REGISTER, 1);
        assert_eq!(DRIVER_OP_GRANT_IRQ, 2);
        assert_eq!(DRIVER_OP_GRANT_DMA, 3);
        assert_eq!(DRIVER_OP_RESTARTED, 4);
    }
}
