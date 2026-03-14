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
    pub const MAX_PAYLOAD: usize = 56;
    pub const FASTPATH_INLINE_MAX: usize = 16;
    pub const FLAG_CAP_TRANSFER: u16 = 1 << 0;
    const NO_TRANSFER_CAP: u64 = u64::MAX;

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

    pub fn is_fastpath_inline(&self) -> bool {
        (self.len as usize) <= Self::FASTPATH_INLINE_MAX
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

    pub fn send(&mut self, msg: Message) -> Result<(), Message> {
        if self.len >= self.max_depth {
            return Err(msg);
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
    fn message_fastpath_inline_classification() {
        let small = Message::new(1, b"short").expect("small");
        let large = Message::new(1, &[0u8; Message::FASTPATH_INLINE_MAX + 1]).expect("large");
        assert!(small.is_fastpath_inline());
        assert!(!large.is_fastpath_inline());
    }
}
