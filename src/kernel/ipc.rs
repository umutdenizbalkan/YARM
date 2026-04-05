// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::syscall_abi;
pub use yarm_kernel::ipc::{IpcError, Message, SharedMemoryRegion, ThreadId, TransferCapId};

pub const IPC_REGISTER_WORDS: usize = syscall_abi::IPC_REGISTER_WORDS;
pub const IPC_REGISTER_BYTES: usize = IPC_REGISTER_WORDS * core::mem::size_of::<usize>();

/// Returns all `IPC_REGISTER_BYTES` unpacked bytes from the register lanes.
///
/// Only the first `len` bytes are semantically meaningful to the caller; any
/// remaining bytes are valid unpacked register contents but are considered
/// unused payload. In other words, `len` is a validity bound, not a trim hint.
pub fn unpack_register_payload(
    words: [usize; IPC_REGISTER_WORDS],
    len: usize,
) -> Option<[u8; IPC_REGISTER_BYTES]> {
    if len > IPC_REGISTER_BYTES {
        return None;
    }

    let mut out = [0u8; IPC_REGISTER_BYTES];
    for (i, word) in words.iter().enumerate() {
        let bytes = (*word).to_le_bytes();
        let start = i * core::mem::size_of::<usize>();
        let end = start + core::mem::size_of::<usize>();
        out[start..end].copy_from_slice(&bytes);
    }
    Some(out)
}

pub fn pack_register_payload(payload: &[u8]) -> Result<[usize; IPC_REGISTER_WORDS], IpcError> {
    if payload.len() > IPC_REGISTER_BYTES {
        return Err(IpcError::PayloadTooLarge);
    }
    let mut words = [0usize; IPC_REGISTER_WORDS];
    for (i, slot) in words.iter_mut().enumerate() {
        let start = i * core::mem::size_of::<usize>();
        let end = start + core::mem::size_of::<usize>();
        let mut lane = [0u8; core::mem::size_of::<usize>()];
        if start < payload.len() {
            let copy_end = core::cmp::min(end, payload.len());
            lane[..copy_end - start].copy_from_slice(&payload[start..copy_end]);
        }
        *slot = usize::from_le_bytes(lane);
    }
    Ok(words)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointMode {
    Buffered,
    /// Scheduling-level rendezvous behavior is enforced by `KernelState::ipc_send/ipc_recv`.
    /// The endpoint itself remains a bounded queue primitive.
    Synchronous,
}

pub const MAX_ENDPOINT_DEPTH: usize = 64;
const _: () = assert!(
    MAX_ENDPOINT_DEPTH.is_power_of_two(),
    "MAX_ENDPOINT_DEPTH must be a power of two for bitmask indexing",
);

#[derive(Debug)]
pub struct Endpoint {
    queue: [Option<Message>; MAX_ENDPOINT_DEPTH],
    head: usize,
    len: usize,
    max_depth: usize,
    mode: EndpointMode,
}

impl Endpoint {
    pub fn new(max_depth: usize) -> Result<Self, IpcError> {
        Self::new_with_mode(max_depth, EndpointMode::Buffered)
    }

    pub fn new_with_mode(max_depth: usize, mode: EndpointMode) -> Result<Self, IpcError> {
        if max_depth == 0 || max_depth > MAX_ENDPOINT_DEPTH {
            return Err(IpcError::InvalidEndpointDepth);
        }

        Ok(Self {
            queue: [const { None }; MAX_ENDPOINT_DEPTH],
            head: 0,
            len: 0,
            max_depth,
            mode,
        })
    }

    pub fn mode(&self) -> EndpointMode {
        self.mode
    }

    pub fn send(&mut self, msg: Message) -> Result<(), IpcError> {
        if self.len >= self.max_depth {
            return Err(IpcError::EndpointFull);
        }

        let tail = (self.head + self.len) & (MAX_ENDPOINT_DEPTH - 1);
        self.queue[tail] = Some(msg);
        self.len += 1;
        Ok(())
    }

    pub fn recv(&mut self) -> Option<Message> {
        if self.len == 0 {
            return None;
        }

        let idx = self.head;
        self.head = (self.head + 1) & (MAX_ENDPOINT_DEPTH - 1);
        self.len -= 1;
        self.queue[idx].take()
    }

    pub fn queued(&self) -> usize {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem;

    #[test]
    fn endpoint_enforces_queue_limit() {
        let mut ep = Endpoint::new(1).expect("endpoint");
        let first = Message::new(1, b"hello").expect("valid message");
        let second = Message::new(2, b"world").expect("valid message");

        assert!(ep.send(first).is_ok());
        assert!(ep.send(second).is_err());
        assert_eq!(ep.recv().expect("first msg").sender_tid, ThreadId(1));
        assert!(ep.send(second).is_ok());
    }

    #[test]
    fn message_header_and_transfer_metadata_roundtrip() {
        let msg = Message::with_header(9, 0x33, Message::FLAG_CAP_TRANSFER, Some(77), b"xy")
            .expect("header message");

        assert_eq!(msg.opcode, 0x33);
        assert_eq!(
            msg.flags & Message::FLAG_CAP_TRANSFER,
            Message::FLAG_CAP_TRANSFER
        );
        assert_eq!(msg.transferred_cap(), Some(TransferCapId(77)));
        assert_eq!(msg.as_slice(), b"xy");
    }

    #[test]
    fn message_transfer_requires_flag() {
        assert_eq!(
            Message::with_header(1, 0, 0, Some(3), b"x"),
            Err(IpcError::MissingCapTransferFlag)
        );
    }

    #[test]
    fn message_transfer_flag_requires_cap() {
        assert_eq!(
            Message::with_header(1, 0, Message::FLAG_CAP_TRANSFER, None, b"x"),
            Err(IpcError::InconsistentCapTransferFlag)
        );
    }

    #[test]
    fn register_payload_roundtrip() {
        let source = [0xAAu8; IPC_REGISTER_BYTES];
        let words = pack_register_payload(&source).expect("pack");
        let decoded = unpack_register_payload(words, IPC_REGISTER_BYTES).expect("decode");
        assert_eq!(decoded, source);
    }

    #[test]
    fn shared_memory_region_codec_roundtrip() {
        let region = SharedMemoryRegion {
            offset: 0x1000,
            len: 8192,
        };
        let encoded = region.encode();
        assert_eq!(SharedMemoryRegion::decode(&encoded), Some(region));
    }

    #[test]
    fn endpoint_rejects_invalid_depths() {
        assert!(matches!(
            Endpoint::new(0),
            Err(IpcError::InvalidEndpointDepth)
        ));
        assert!(matches!(
            Endpoint::new(MAX_ENDPOINT_DEPTH + 1),
            Err(IpcError::InvalidEndpointDepth)
        ));
    }

    #[test]
    fn extracted_ipc_types_are_reexported_without_layout_drift() {
        assert_eq!(
            mem::size_of::<Message>(),
            mem::size_of::<yarm_kernel::ipc::Message>()
        );
        assert_eq!(
            mem::size_of::<ThreadId>(),
            mem::size_of::<yarm_kernel::ipc::ThreadId>()
        );
        assert_eq!(
            mem::size_of::<TransferCapId>(),
            mem::size_of::<yarm_kernel::ipc::TransferCapId>()
        );

        let message = Message::new(7, b"pass-a").expect("message");
        let _kernel_message: yarm_kernel::ipc::Message = message;
    }

    #[test]
    fn pass_a_guardrail_keeps_ipc_core_owned_by_yarm_kernel_crate() {
        let src = include_str!("ipc.rs");
        assert!(
            src.contains("pub use yarm_kernel::ipc::{"),
            "kernel ipc module must keep yarm-kernel re-export bridge"
        );
    }
}
