#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcCodecError {
    Malformed,
}

pub const PROC_SERVER_ABI_VERSION: u16 = 1;
pub const PROC_CODEC_V2_VERSION: u16 = 2;

pub const PROC_OP_GETPID: u16 = 1;
pub const PROC_OP_EXIT: u16 = 2;
pub const PROC_OP_GETPPID: u16 = 3;
pub const PROC_OP_SPAWN_V2: u16 = 4;
pub const PROC_OP_WAITPID_V2: u16 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcV2Args {
    pub arg0: u64,
    pub arg1: u64,
}

impl ProcV2Args {
    pub const VERSION: u16 = PROC_CODEC_V2_VERSION;
    pub const ENCODED_LEN: usize = 16;

    pub const fn new(arg0: u64, arg1: u64) -> Self {
        Self { arg0, arg1 }
    }

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let a0 = self.arg0.to_le_bytes();
        let a1 = self.arg1.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            payload[i] = a0[i];
            payload[8 + i] = a1[i];
            i += 1;
        }
        payload
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(ProcCodecError::Malformed);
        }
        let mut a0 = [0u8; 8];
        let mut a1 = [0u8; 8];
        a0.copy_from_slice(&payload[..8]);
        a1.copy_from_slice(&payload[8..Self::ENCODED_LEN]);
        Ok(Self {
            arg0: u64::from_le_bytes(a0),
            arg1: u64::from_le_bytes(a1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_v2_roundtrip() {
        let args = ProcV2Args::new(9, 11);
        let enc = args.encode();
        assert_eq!(ProcV2Args::decode(&enc), Ok(args));
    }


    #[test]
    fn proc_v2_rejects_non_exact_payload_lengths() {
        let short = [0u8; ProcV2Args::ENCODED_LEN - 1];
        assert_eq!(ProcV2Args::decode(&short), Err(ProcCodecError::Malformed));

        let long = [0u8; ProcV2Args::ENCODED_LEN + 1];
        assert_eq!(ProcV2Args::decode(&long), Err(ProcCodecError::Malformed));
    }

    #[test]
    fn proc_v2_constants_are_stable() {
        assert_eq!(PROC_SERVER_ABI_VERSION, 1);
        assert_eq!(PROC_CODEC_V2_VERSION, 2);
        assert_eq!(ProcV2Args::VERSION, PROC_CODEC_V2_VERSION);
        assert_eq!(ProcV2Args::ENCODED_LEN, 16);
        assert_eq!(PROC_OP_SPAWN_V2, 4);
        assert_eq!(PROC_OP_WAITPID_V2, 5);
    }
}
