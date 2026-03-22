use crate::arch::syscall_abi;
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThreadId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TransferCapId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    PayloadTooLarge,
    MissingCapTransferFlag,
    InconsistentCapTransferFlag,
    InvalidEndpointDepth,
    EndpointFull,
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::PayloadTooLarge => "IPC payload exceeds register/message capacity",
            Self::MissingCapTransferFlag => "transferred capability is missing the transfer flag",
            Self::InconsistentCapTransferFlag => {
                "capability transfer flag set without a transferred capability"
            }
            Self::InvalidEndpointDepth => "endpoint depth is outside the supported range",
            Self::EndpointFull => "endpoint queue is full",
        };
        f.write_str(message)
    }
}

pub const IPC_REGISTER_WORDS: usize = syscall_abi::IPC_REGISTER_WORDS;
pub const IPC_REGISTER_BYTES: usize = IPC_REGISTER_WORDS * core::mem::size_of::<usize>();

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
pub struct SharedMemoryRegion {
    pub offset: u64,
    pub len: u64,
}

impl SharedMemoryRegion {
    pub const ENCODED_LEN: usize = 16;

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        let off = self.offset.to_le_bytes();
        let len = self.len.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            out[i] = off[i];
            out[8 + i] = len[i];
            i += 1;
        }
        out
    }

    pub const fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::ENCODED_LEN {
            return None;
        }
        let mut off = [0u8; 8];
        let mut len = [0u8; 8];
        let mut i = 0;
        while i < 8 {
            off[i] = payload[i];
            len[i] = payload[8 + i];
            i += 1;
        }
        Some(Self {
            offset: u64::from_le_bytes(off),
            len: u64::from_le_bytes(len),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub sender_tid: ThreadId,
    pub opcode: u16,
    pub flags: u16,
    transferred_cap: u64,
    pub len: u8,
    pub payload: [u8; Self::MAX_PAYLOAD],
}

const _: () = assert!(Message::MAX_PAYLOAD <= (u8::MAX as usize));

impl Message {
    /// Two register-width payload lanes (16 bytes on 64-bit, 8 bytes on 32-bit)
    /// are reserved for the syscall fast path, leaving 56 payload bytes in the
    /// fixed in-kernel `Message` envelope on 64-bit targets.
    pub const MAX_PAYLOAD: usize = 56;
    pub const FLAG_CAP_TRANSFER: u16 = 1 << 0;
    pub const NO_TRANSFER_CAP: u64 = u64::MAX;

    pub fn new(sender_tid: u64, bytes: &[u8]) -> Result<Self, IpcError> {
        Self::with_header(sender_tid, 0, 0, None, bytes)
    }

    pub fn with_header(
        sender_tid: u64,
        opcode: u16,
        flags: u16,
        transferred_cap: Option<u64>,
        bytes: &[u8],
    ) -> Result<Self, IpcError> {
        if bytes.len() > Self::MAX_PAYLOAD {
            return Err(IpcError::PayloadTooLarge);
        }

        let has_cap = transferred_cap.is_some();
        let flag_set = (flags & Self::FLAG_CAP_TRANSFER) != 0;

        if has_cap && !flag_set {
            return Err(IpcError::MissingCapTransferFlag);
        }
        if !has_cap && flag_set {
            return Err(IpcError::InconsistentCapTransferFlag);
        }

        let mut payload = [0u8; Self::MAX_PAYLOAD];
        payload[..bytes.len()].copy_from_slice(bytes);

        Ok(Self {
            sender_tid: ThreadId(sender_tid),
            opcode,
            flags,
            transferred_cap: transferred_cap.unwrap_or(Self::NO_TRANSFER_CAP),
            len: bytes.len() as u8,
            payload,
        })
    }

    pub const fn transferred_cap(&self) -> Option<TransferCapId> {
        if self.transferred_cap == Self::NO_TRANSFER_CAP {
            None
        } else {
            Some(TransferCapId(self.transferred_cap))
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.payload[..self.len as usize]
    }
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
}
