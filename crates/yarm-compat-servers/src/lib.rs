// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

#[cfg(test)]
mod tests {
    use yarm_server_runtime::ipc_abi::process_abi::PROC_OP_GETPID;
    const PROC_GETPID_REPLY_REQUIRED_BYTES: usize = 8;

    fn decode_getpid_reply(opcode: u16, payload: &[u8]) -> Result<u64, ()> {
        if opcode != PROC_OP_GETPID || payload.len() < PROC_GETPID_REPLY_REQUIRED_BYTES {
            return Err(());
        }
        let mut pid_bytes = [0u8; PROC_GETPID_REPLY_REQUIRED_BYTES];
        pid_bytes.copy_from_slice(&payload[..PROC_GETPID_REPLY_REQUIRED_BYTES]);
        Ok(u64::from_le_bytes(pid_bytes))
    }

    #[test]
    fn getpid_ipc_rejects_malformed_reply_payload() {
        assert_eq!(decode_getpid_reply(PROC_OP_GETPID, &[1, 2, 3]), Err(()));
    }
}
