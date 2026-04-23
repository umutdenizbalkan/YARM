// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::ipc::Message;
use yarm_fs_servers::common::vfs_ipc::{FilesystemService, VfsError};

use super::device::{
    VirtQueue, VirtioBlkDevice, VirtioBlkReqFrame, VirtioBlkRequest, VirtioBlkRespFrame,
    VirtqChain, VIRTIO_BLK_OP_READ, VIRTIO_BLK_OP_WRITE,
};

#[derive(Debug)]
pub struct VirtioBlkService {
    dev: VirtioBlkDevice,
    queue: VirtQueue,
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
            queue: VirtQueue::new(),
        }
    }

    pub const fn stats(&self) -> (u64, u64) {
        (self.dev.reads, self.dev.writes)
    }

    fn process_once(&mut self) -> Result<VirtioBlkRespFrame, VfsError> {
        let chain = self.queue.pop_next_chain().ok_or(VfsError::Unsupported)?;
        let req = chain.request;
        let io = VirtioBlkRequest {
            sector: req.sector,
            len: req.len,
        };
        let result = match req.op {
            VIRTIO_BLK_OP_READ => self.dev.read(io),
            VIRTIO_BLK_OP_WRITE => self.dev.write(io),
            _ => Err(()),
        }
        .map_err(|_| VfsError::BadFd)?;

        let resp = VirtioBlkRespFrame {
            status: 0,
            _pad: [0; 3],
            done_len: result,
            tag: req.tag,
        };
        self.queue.push_used(resp);
        self.queue.take_last_used().ok_or(VfsError::Unsupported)
    }
}

impl FilesystemService for VirtioBlkService {
    fn service_name(&self) -> &'static str {
        "virtio_blk"
    }

    fn dispatch(&mut self, request: Message) -> Result<Message, VfsError> {
        let req = VirtioBlkReqFrame::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
        let chain = VirtqChain::from_request(req);
        self.queue.push_chain(chain).map_err(|_| VfsError::NoFd)?;
        let resp = self.process_once()?;
        Message::with_header(0, request.opcode, 0, None, &resp.encode())
            .map_err(|_| VfsError::Malformed)
    }
}

pub fn run() {
    let mut svc = VirtioBlkService::new();
    let read = VirtioBlkReqFrame {
        op: VIRTIO_BLK_OP_READ,
        _reserved: 0,
        sector: 1,
        len: 512,
        tag: 11,
    };
    let write = VirtioBlkReqFrame {
        op: VIRTIO_BLK_OP_WRITE,
        _reserved: 0,
        sector: 1,
        len: 512,
        tag: 12,
    };
    let read_msg = Message::with_header(0, 1, 0, None, &read.encode()).expect("read");
    let write_msg = Message::with_header(0, 2, 0, None, &write.encode()).expect("write");
    let _ = svc.dispatch(read_msg).expect("read rep");
    let _ = svc.dispatch(write_msg).expect("write rep");
    let (reads, writes) = svc.stats();
    yarm_user_rt::user_log!("virtio_blk.srv demo: reads={}, writes={}", reads, writes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_roundtrips_frame_contract() {
        let mut svc = VirtioBlkService::new();
        let req = VirtioBlkReqFrame {
            op: VIRTIO_BLK_OP_READ,
            _reserved: 0,
            sector: 2,
            len: 128,
            tag: 77,
        };
        let msg = Message::with_header(0, 1, 0, None, &req.encode()).expect("msg");
        let rep = svc.dispatch(msg).expect("dispatch");
        let resp = VirtioBlkRespFrame::decode(rep.as_slice()).expect("decode");
        assert_eq!(resp.status, 0);
        assert_eq!(resp.done_len, 128);
        assert_eq!(resp.tag, 77);
    }
}
