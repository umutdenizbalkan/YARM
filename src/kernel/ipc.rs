#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub sender_tid: u64,
    pub opcode: u16,
    pub flags: u16,
    pub transferred_cap: Option<u64>,
    pub len: u8,
    pub payload: [u8; Self::MAX_PAYLOAD],
}

impl Message {
    pub const MAX_PAYLOAD: usize = 56;
    pub const FLAG_CAP_TRANSFER: u16 = 1 << 0;

    pub fn new(sender_tid: u64, bytes: &[u8]) -> Result<Self, ()> {
        Self::with_header(sender_tid, 0, 0, None, bytes)
    }

    pub fn with_header(
        sender_tid: u64,
        opcode: u16,
        flags: u16,
        transferred_cap: Option<u64>,
        bytes: &[u8],
    ) -> Result<Self, ()> {
        if bytes.len() > Self::MAX_PAYLOAD {
            return Err(());
        }

        if transferred_cap.is_some() && (flags & Self::FLAG_CAP_TRANSFER) == 0 {
            return Err(());
        }

        let mut payload = [0u8; Self::MAX_PAYLOAD];
        let mut i = 0;
        while i < bytes.len() {
            payload[i] = bytes[i];
            i += 1;
        }

        Ok(Self {
            sender_tid,
            opcode,
            flags,
            transferred_cap,
            len: bytes.len() as u8,
            payload,
        })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.payload[..self.len as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointMode {
    Buffered,
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
    pub fn new(max_depth: usize) -> Self {
        Self::new_with_mode(max_depth, EndpointMode::Buffered)
    }

    pub fn new_with_mode(max_depth: usize, mode: EndpointMode) -> Self {
        let bounded = if max_depth > MAX_ENDPOINT_DEPTH {
            MAX_ENDPOINT_DEPTH
        } else {
            max_depth
        };

        Self {
            queue: [None; MAX_ENDPOINT_DEPTH],
            head: 0,
            len: 0,
            max_depth: if bounded == 0 { 1 } else { bounded },
            mode,
        }
    }

    pub fn mode(&self) -> EndpointMode {
        self.mode
    }

    pub fn send(&mut self, msg: Message) -> Result<(), Message> {
        if self.len >= self.max_depth {
            return Err(msg);
        }

        let tail = (self.head + self.len) % MAX_ENDPOINT_DEPTH;
        self.queue[tail] = Some(msg);
        self.len += 1;
        Ok(())
    }

    pub fn recv(&mut self) -> Option<Message> {
        if self.len == 0 {
            return None;
        }

        let idx = self.head;
        self.head = (self.head + 1) % MAX_ENDPOINT_DEPTH;
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
        let mut ep = Endpoint::new(1);
        let first = Message::new(1, b"hello").expect("valid message");
        let second = Message::new(2, b"world").expect("valid message");

        assert!(ep.send(first).is_ok());
        assert!(ep.send(second).is_err());
        assert_eq!(ep.recv().expect("first msg").sender_tid, 1);
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
        assert_eq!(msg.transferred_cap, Some(77));
        assert_eq!(msg.as_slice(), b"xy");
    }

    #[test]
    fn message_transfer_requires_flag() {
        assert!(Message::with_header(1, 0, 0, Some(3), b"x").is_err());
    }
}
