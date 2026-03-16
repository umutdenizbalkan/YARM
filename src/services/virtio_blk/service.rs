extern crate std;

use std::println;

use crate::kernel::ipc::Message;
use crate::kernel::vfs::{FilesystemService, VfsLiteError};

use super::device::{VirtioBlkDevice, VirtioBlkRequest};

#[derive(Debug)]
pub struct VirtioBlkService {
    dev: VirtioBlkDevice,
}

impl Default for VirtioBlkService {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtioBlkService {
    pub const fn new() -> Self {
        Self {
            dev: VirtioBlkDevice::new(4096, 512),
        }
    }

    pub const fn stats(&self) -> (u64, u64) {
        (self.dev.reads, self.dev.writes)
    }
}

impl FilesystemService for VirtioBlkService {
    fn service_name(&self) -> &'static str {
        "virtio_blk"
    }

    fn dispatch(&mut self, request: Message) -> Result<Message, VfsLiteError> {
        let mut sector = [0u8; 8];
        let payload = request.as_slice();
        if payload.len() < 16 {
            return Err(VfsLiteError::Malformed);
        }
        sector.copy_from_slice(&payload[..8]);
        let mut len = [0u8; 8];
        len.copy_from_slice(&payload[8..16]);
        let req = VirtioBlkRequest {
            sector: u64::from_le_bytes(sector),
            len: u64::from_le_bytes(len),
        };
        let done = if request.opcode == 1 {
            self.dev.read(req).map_err(|_| VfsLiteError::BadFd)?
        } else {
            self.dev.write(req).map_err(|_| VfsLiteError::BadFd)?
        };
        Message::with_header(0, request.opcode, 0, None, &done.to_le_bytes())
            .map_err(|_| VfsLiteError::Malformed)
    }
}

pub fn run() {
    let mut svc = VirtioBlkService::new();
    let mut payload = [0u8; 16];
    payload[..8].copy_from_slice(&1u64.to_le_bytes());
    payload[8..16].copy_from_slice(&512u64.to_le_bytes());
    let read = Message::with_header(0, 1, 0, None, &payload).expect("read");
    let write = Message::with_header(0, 2, 0, None, &payload).expect("write");
    let _ = svc.dispatch(read).expect("read rep");
    let _ = svc.dispatch(write).expect("write rep");
    let (reads, writes) = svc.stats();
    println!("virtio_blk.srv demo: reads={}, writes={}", reads, writes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_blk_service_counts_rw() {
        let mut svc = VirtioBlkService::new();
        let mut payload = [0u8; 16];
        payload[..8].copy_from_slice(&1u64.to_le_bytes());
        payload[8..16].copy_from_slice(&64u64.to_le_bytes());
        let read = Message::with_header(0, 1, 0, None, &payload).expect("read");
        let write = Message::with_header(0, 2, 0, None, &payload).expect("write");
        let _ = svc.dispatch(read).expect("read rep");
        let _ = svc.dispatch(write).expect("write rep");
        assert_eq!(svc.stats(), (1, 1));
    }
}
