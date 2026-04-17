// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcCodecError {
    Malformed,
}

pub const PROC_SERVER_ABI_VERSION: u16 = 1;
pub const PROC_CODEC_V2_VERSION: u16 = 2;
pub const PROC_CODEC_V3_VERSION: u16 = 3;
pub const PROC_CODEC_V4_VERSION: u16 = 4;

pub const PROC_OP_GETPID: u16 = 1;
pub const PROC_OP_EXIT: u16 = 2;
pub const PROC_OP_GETPPID: u16 = 3;
pub const PROC_OP_SPAWN_V2: u16 = 4;
pub const PROC_OP_WAITPID_V2: u16 = 5;
pub const PROC_OP_SPAWN_V3: u16 = 6;
pub const PROC_OP_SPAWN_V4: u16 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV2Args {
    pub parent_pid: u64,
    pub image_id: u64,
}

impl SpawnV2Args {
    pub const VERSION: u16 = PROC_CODEC_V2_VERSION;

    pub const fn new(parent_pid: u64, image_id: u64) -> Self {
        Self {
            parent_pid,
            image_id,
        }
    }

    pub const fn encode(self) -> [u8; ProcV2Args::ENCODED_LEN] {
        ProcV2Args::new(self.parent_pid, self.image_id).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        let args = ProcV2Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV3Args {
    pub parent_pid: u64,
    pub image_id: u64,
    pub requested_cnode_slots: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV4Args {
    pub parent_pid: u64,
    pub image_id: u64,
    pub requested_cnode_slots: u64,
    pub task_class_hint: u64,
}

impl SpawnV4Args {
    pub const VERSION: u16 = PROC_CODEC_V4_VERSION;

    pub const fn new(
        parent_pid: u64,
        image_id: u64,
        requested_cnode_slots: u64,
        task_class_hint: u64,
    ) -> Self {
        Self {
            parent_pid,
            image_id,
            requested_cnode_slots,
            task_class_hint,
        }
    }

    pub const fn encode(self) -> [u8; ProcV4Args::ENCODED_LEN] {
        ProcV4Args::new(
            self.parent_pid,
            self.image_id,
            self.requested_cnode_slots,
            self.task_class_hint,
        )
        .encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        let args = ProcV4Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1, args.arg2, args.arg3))
    }
}

impl SpawnV3Args {
    pub const VERSION: u16 = PROC_CODEC_V3_VERSION;

    pub const fn new(parent_pid: u64, image_id: u64, requested_cnode_slots: u64) -> Self {
        Self {
            parent_pid,
            image_id,
            requested_cnode_slots,
        }
    }

    pub const fn encode(self) -> [u8; ProcV3Args::ENCODED_LEN] {
        ProcV3Args::new(self.parent_pid, self.image_id, self.requested_cnode_slots).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        let args = ProcV3Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1, args.arg2))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPidV2Args {
    pub caller_pid: u64,
    pub target_pid: u64,
}

impl WaitPidV2Args {
    pub const VERSION: u16 = PROC_CODEC_V2_VERSION;

    pub const fn new(caller_pid: u64, target_pid: u64) -> Self {
        Self {
            caller_pid,
            target_pid,
        }
    }

    pub const fn encode(self) -> [u8; ProcV2Args::ENCODED_LEN] {
        ProcV2Args::new(self.caller_pid, self.target_pid).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        let args = ProcV2Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPidV2Reply {
    pub waited_pid: u64,
    pub exit_code: u64,
}

impl WaitPidV2Reply {
    pub const VERSION: u16 = PROC_CODEC_V2_VERSION;

    pub const fn new(waited_pid: u64, exit_code: u64) -> Self {
        Self {
            waited_pid,
            exit_code,
        }
    }

    pub const fn encode(self) -> [u8; ProcV2Args::ENCODED_LEN] {
        ProcV2Args::new(self.waited_pid, self.exit_code).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcCodecError> {
        let args = ProcV2Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcV2Args {
    pub arg0: u64,
    pub arg1: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcV3Args {
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcV4Args {
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
}

impl ProcV4Args {
    pub const VERSION: u16 = PROC_CODEC_V4_VERSION;
    pub const ENCODED_LEN: usize = 32;

    pub const fn new(arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> Self {
        Self {
            arg0,
            arg1,
            arg2,
            arg3,
        }
    }

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let a0 = self.arg0.to_le_bytes();
        let a1 = self.arg1.to_le_bytes();
        let a2 = self.arg2.to_le_bytes();
        let a3 = self.arg3.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            payload[i] = a0[i];
            payload[8 + i] = a1[i];
            payload[16 + i] = a2[i];
            payload[24 + i] = a3[i];
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
        let mut a2 = [0u8; 8];
        let mut a3 = [0u8; 8];
        a0.copy_from_slice(&payload[..8]);
        a1.copy_from_slice(&payload[8..16]);
        a2.copy_from_slice(&payload[16..24]);
        a3.copy_from_slice(&payload[24..Self::ENCODED_LEN]);
        Ok(Self {
            arg0: u64::from_le_bytes(a0),
            arg1: u64::from_le_bytes(a1),
            arg2: u64::from_le_bytes(a2),
            arg3: u64::from_le_bytes(a3),
        })
    }
}

impl ProcV3Args {
    pub const VERSION: u16 = PROC_CODEC_V3_VERSION;
    pub const ENCODED_LEN: usize = 24;

    pub const fn new(arg0: u64, arg1: u64, arg2: u64) -> Self {
        Self { arg0, arg1, arg2 }
    }

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let a0 = self.arg0.to_le_bytes();
        let a1 = self.arg1.to_le_bytes();
        let a2 = self.arg2.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            payload[i] = a0[i];
            payload[8 + i] = a1[i];
            payload[16 + i] = a2[i];
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
        let mut a2 = [0u8; 8];
        a0.copy_from_slice(&payload[..8]);
        a1.copy_from_slice(&payload[8..16]);
        a2.copy_from_slice(&payload[16..Self::ENCODED_LEN]);
        Ok(Self {
            arg0: u64::from_le_bytes(a0),
            arg1: u64::from_le_bytes(a1),
            arg2: u64::from_le_bytes(a2),
        })
    }
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
        assert_eq!(SpawnV2Args::VERSION, PROC_CODEC_V2_VERSION);
        assert_eq!(WaitPidV2Args::VERSION, PROC_CODEC_V2_VERSION);
        assert_eq!(WaitPidV2Reply::VERSION, PROC_CODEC_V2_VERSION);
        assert_eq!(PROC_OP_SPAWN_V2, 4);
        assert_eq!(PROC_OP_WAITPID_V2, 5);
        assert_eq!(PROC_CODEC_V3_VERSION, 3);
        assert_eq!(ProcV3Args::VERSION, PROC_CODEC_V3_VERSION);
        assert_eq!(ProcV3Args::ENCODED_LEN, 24);
        assert_eq!(SpawnV3Args::VERSION, PROC_CODEC_V3_VERSION);
        assert_eq!(PROC_OP_SPAWN_V3, 6);
        assert_eq!(PROC_CODEC_V4_VERSION, 4);
        assert_eq!(ProcV4Args::VERSION, PROC_CODEC_V4_VERSION);
        assert_eq!(ProcV4Args::ENCODED_LEN, 32);
        assert_eq!(SpawnV4Args::VERSION, PROC_CODEC_V4_VERSION);
        assert_eq!(PROC_OP_SPAWN_V4, 7);
    }

    #[test]
    fn typed_proc_v2_wrappers_roundtrip_via_frozen_codec() {
        let spawn = SpawnV2Args::new(7, 9);
        assert_eq!(SpawnV2Args::decode(&spawn.encode()), Ok(spawn));

        let wait = WaitPidV2Args::new(3, 4);
        assert_eq!(WaitPidV2Args::decode(&wait.encode()), Ok(wait));

        let reply = WaitPidV2Reply::new(4, 255);
        assert_eq!(WaitPidV2Reply::decode(&reply.encode()), Ok(reply));

        let spawn_v3 = SpawnV3Args::new(7, 9, 64);
        assert_eq!(SpawnV3Args::decode(&spawn_v3.encode()), Ok(spawn_v3));

        let spawn_v4 = SpawnV4Args::new(7, 9, 64, 2);
        assert_eq!(SpawnV4Args::decode(&spawn_v4.encode()), Ok(spawn_v4));
    }

    #[test]
    fn proc_v2_golden_vector_is_stable() {
        let args = ProcV2Args::new(0x1122_3344_5566_7788, 0x99aa_bbcc_ddee_ff00);
        let expected = [
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, // arg0 LE
            0x00, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, // arg1 LE
        ];
        assert_eq!(args.encode(), expected);
        assert_eq!(ProcV2Args::decode(&expected), Ok(args));
    }

    #[test]
    fn proc_v3_golden_vector_is_stable() {
        let args = ProcV3Args::new(
            0x1122_3344_5566_7788,
            0x99aa_bbcc_ddee_ff00,
            0x0102_0304_0506_0708,
        );
        let expected = [
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, // arg0 LE
            0x00, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, // arg1 LE
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // arg2 LE
        ];
        assert_eq!(args.encode(), expected);
        assert_eq!(ProcV3Args::decode(&expected), Ok(args));
    }

    #[test]
    fn proc_v4_golden_vector_is_stable() {
        let args = ProcV4Args::new(
            0x1122_3344_5566_7788,
            0x99aa_bbcc_ddee_ff00,
            0x0102_0304_0506_0708,
            0x0a0b_0c0d_0e0f_1011,
        );
        let expected = [
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, // arg0
            0x00, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, // arg1
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // arg2
            0x11, 0x10, 0x0f, 0x0e, 0x0d, 0x0c, 0x0b, 0x0a, // arg3
        ];
        assert_eq!(args.encode(), expected);
        assert_eq!(ProcV4Args::decode(&expected), Ok(args));
    }
}
