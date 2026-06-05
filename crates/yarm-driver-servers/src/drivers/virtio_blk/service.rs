// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::device::{VirtioBlkMemoryDevice, build_write_chain};
use yarm_ipc_abi::block_abi::{
    BLK_OP_GET_INFO, BLK_OP_WRITE, BlkGetInfoReply, BlkGetInfoRequest, BlkSectorWriteAssembler,
    BlkStatus, BlkWriteReply, BlkWriteRequest,
};
use yarm_ipc_abi::block_backend_abi::*;
use yarm_user_rt::ipc::Message;

const SERVICE_SECTORS: usize = 8;

#[derive(Debug)]
pub struct VirtioBlkWriteService<const SECTORS: usize> {
    assembler: BlkSectorWriteAssembler,
    device: VirtioBlkMemoryDevice<SECTORS>,
}

impl<const SECTORS: usize> Default for VirtioBlkWriteService<SECTORS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SECTORS: usize> VirtioBlkWriteService<SECTORS> {
    pub const fn new() -> Self {
        Self {
            assembler: BlkSectorWriteAssembler::new(),
            device: VirtioBlkMemoryDevice::new(),
        }
    }

    pub fn handle_write(&mut self, request: &BlkWriteRequest) -> BlkWriteReply {
        if request.lba >= SECTORS as u64 {
            self.assembler.reset();
            return write_reply(request, BlkStatus::InvalidRequest, 0, false);
        }
        let completed = match self.assembler.accept(request) {
            Ok(completed) => completed,
            Err(status) => return write_reply(request, status, 0, false),
        };
        let Some(sector) = completed else {
            return write_reply(request, BlkStatus::Success, request.data_len, false);
        };

        let chain = build_write_chain(sector.request_id, sector.lba, sector.data.len() as u32);
        if chain.request.op != super::device::VIRTIO_BLK_OP_WRITE
            || self.device.write_sector(sector.lba, &sector.data).is_err()
        {
            return write_reply(request, BlkStatus::IOError, 0, false);
        }
        write_reply(request, BlkStatus::Success, request.data_len, true)
    }

    pub fn read_sector(&mut self, lba: u64) -> Result<[u8; 512], BlkStatus> {
        self.device
            .read_sector(lba)
            .map_err(|_| BlkStatus::InvalidRequest)
    }
}

fn write_reply(
    request: &BlkWriteRequest,
    status: BlkStatus,
    bytes_accepted: u32,
    committed: bool,
) -> BlkWriteReply {
    BlkWriteReply {
        request_id: request.request_id,
        status,
        bytes_accepted,
        sector_committed: u32::from(committed),
        lba: request.lba,
    }
}

fn decode_query_request(msg: &Message) -> Result<BlkBackendQueryRequest, i32> {
    BlkBackendQueryRequest::decode(msg.as_slice()).ok_or(BLK_BACKEND_STATUS_EINVAL)
}

fn decode_io_request(msg: &Message) -> Result<BlkBackendRequest, i32> {
    let req = BlkBackendRequest::decode(msg.as_slice()).ok_or(BLK_BACKEND_STATUS_EINVAL)?;
    if !req.is_valid_for_opcode(msg.opcode) {
        return Err(BLK_BACKEND_STATUS_EINVAL);
    }
    Ok(req)
}

fn build_resp(req_id: u32, status: i32) -> BlkBackendResponse {
    BlkBackendResponse {
        req_id,
        status,
        actual_bytes: 0,
        backend_generation: 0,
        logical_block_size: 512,
        physical_block_size: 512,
    }
}

pub fn run() {
    let ctx = yarm_user_rt::runtime::startup_context();
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        return;
    };
    let mut write_service = VirtioBlkWriteService::<SERVICE_SECTORS>::new();
    yarm_user_rt::user_log!("VIRTIO_BLK_SRV_READY");
    loop {
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some(received)) => {
                let msg = received.message;
                let Some(reply_cap) = received.reply_cap else {
                    continue;
                };
                match msg.opcode {
                    BLK_OP_GET_INFO => {
                        yarm_user_rt::user_log!("VIRTIO_BLK_GET_INFO_REQUEST");
                        let status = match BlkGetInfoRequest::decode(msg.as_slice()) {
                            Some(_) => BlkStatus::Success,
                            None => BlkStatus::InvalidRequest,
                        };
                        let reply = BlkGetInfoReply {
                            status,
                            _reserved0: 0,
                            logical_block_size: 512,
                            _reserved1: 0,
                            total_blocks: SERVICE_SECTORS as u64,
                            feature_flags: 0,
                        };
                        if let Ok(message) =
                            Message::with_header(0, BLK_OP_GET_INFO, 0, None, &reply.encode())
                        {
                            let _ =
                                unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &message) };
                        }
                    }
                    BLK_OP_WRITE => {
                        yarm_user_rt::user_log!("VIRTIO_BLK_OP_WRITE_INLINE");
                        let reply = match BlkWriteRequest::decode(msg.as_slice()) {
                            Ok(request) => write_service.handle_write(&request),
                            Err(status) => BlkWriteReply {
                                request_id: 0,
                                status,
                                bytes_accepted: 0,
                                sector_committed: 0,
                                lba: 0,
                            },
                        };
                        if let Ok(message) =
                            Message::with_header(0, BLK_OP_WRITE, 0, None, &reply.encode())
                        {
                            let _ =
                                unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &message) };
                        }
                    }
                    opcode => {
                        let (req_id, status) = match opcode {
                            BLK_BACKEND_OP_QUERY_STATE => match decode_query_request(&msg) {
                                Ok(req) => (req.req_id, BLK_BACKEND_STATUS_EAGAIN),
                                Err(error) => (0, error),
                            },
                            BLK_BACKEND_OP_READ | BLK_BACKEND_OP_WRITE | BLK_BACKEND_OP_FLUSH => {
                                match decode_io_request(&msg) {
                                    Ok(req) => (req.req_id, BLK_BACKEND_STATUS_ENOSYS),
                                    Err(error) => (0, error),
                                }
                            }
                            BLK_BACKEND_OP_GET_GEOM => match decode_query_request(&msg) {
                                Ok(req) => (req.req_id, BLK_BACKEND_STATUS_EAGAIN),
                                Err(error) => (0, error),
                            },
                            _ => (0, BLK_BACKEND_STATUS_EINVAL),
                        };
                        let response = build_resp(req_id, status);
                        if let Ok(reply) =
                            Message::with_header(0, opcode, 0, None, &response.encode())
                        {
                            let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(_) => {
                let _ = yarm_user_rt::syscall::yield_now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_ipc_abi::block_abi::{
        BLK_SECTOR_SIZE, BLK_WRITE_F_FIRST, BLK_WRITE_F_LAST, BLK_WRITE_MAX_CHUNK_BYTES,
    };

    fn write_sector(
        service: &mut VirtioBlkWriteService<4>,
        request_id: u32,
        lba: u64,
        data: &[u8; 512],
    ) -> BlkWriteReply {
        let mut offset = 0usize;
        let mut final_reply = None;
        while offset < BLK_SECTOR_SIZE as usize {
            let len = core::cmp::min(BLK_WRITE_MAX_CHUNK_BYTES, BLK_SECTOR_SIZE as usize - offset);
            let mut chunk = [0u8; BLK_WRITE_MAX_CHUNK_BYTES];
            chunk[..len].copy_from_slice(&data[offset..offset + len]);
            let request = BlkWriteRequest {
                request_id,
                flags: (if offset == 0 { BLK_WRITE_F_FIRST } else { 0 })
                    | (if offset + len == BLK_SECTOR_SIZE as usize {
                        BLK_WRITE_F_LAST
                    } else {
                        0
                    }),
                device_id: 1,
                lba,
                sector_offset: offset as u32,
                data_len: len as u32,
                data: chunk,
            };
            final_reply = Some(service.handle_write(&request));
            offset += len;
        }
        final_reply.expect("write reply")
    }

    #[test]
    fn virtio_service_write_then_read_and_overwrite_are_exact() {
        let mut service = VirtioBlkWriteService::<4>::new();
        let first = [0x33; 512];
        let second = core::array::from_fn(|index| (index * 3) as u8);
        let reply = write_sector(&mut service, 1, 2, &first);
        assert_eq!(reply.status, BlkStatus::Success);
        assert_eq!(reply.sector_committed, 1);
        assert_eq!(service.read_sector(2), Ok(first));
        assert_eq!(
            write_sector(&mut service, 2, 2, &second).status,
            BlkStatus::Success
        );
        assert_eq!(service.read_sector(2), Ok(second));
    }

    #[test]
    fn virtio_service_rejects_out_of_range_sector_without_mutation() {
        let mut service = VirtioBlkWriteService::<2>::new();
        let request = BlkWriteRequest {
            request_id: 1,
            flags: BLK_WRITE_F_FIRST,
            device_id: 1,
            lba: 2,
            sector_offset: 0,
            data_len: 1,
            data: [0xaa; BLK_WRITE_MAX_CHUNK_BYTES],
        };
        assert_eq!(
            service.handle_write(&request).status,
            BlkStatus::InvalidRequest
        );
        assert_eq!(service.read_sector(0), Ok([0; 512]));
    }
}
