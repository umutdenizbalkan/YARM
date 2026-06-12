// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::block_abi::{
    BLK_OP_GET_INFO, BLK_OP_WRITE, BlkGetInfoReply, BlkGetInfoRequest, BlkSectorWriteAssembler,
    BlkStatus, BlkWriteReply, BlkWriteRequest,
};
use yarm_ipc_abi::block_backend_abi::*;
use yarm_user_rt::ipc::Message;

pub const SERVICE_SECTORS: usize = 8;

pub use super::VirtioBlkWriteService;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockDeviceInfo {
    pub logical_block_size: u32,
    pub total_blocks: u64,
    pub feature_flags: u64,
}

/// Backend-neutral sector operations required by the existing block service.
pub trait BlockDeviceOps {
    fn get_info(&self) -> BlockDeviceInfo;
    fn read_sector(&mut self, lba: u64) -> Result<[u8; 512], ()>;
    fn write_sector(&mut self, lba: u64, data: &[u8; 512]) -> Result<u32, ()>;
}

#[derive(Debug)]
pub struct BlockWriteService<D> {
    assembler: BlkSectorWriteAssembler,
    device: D,
}

impl<D: BlockDeviceOps + Default> Default for BlockWriteService<D> {
    fn default() -> Self {
        Self::new()
    }
}

impl<D: BlockDeviceOps + Default> BlockWriteService<D> {
    pub fn new() -> Self {
        Self::with_backend(D::default())
    }
}

impl<D: BlockDeviceOps> BlockWriteService<D> {
    pub const fn with_backend(device: D) -> Self {
        Self {
            assembler: BlkSectorWriteAssembler::new(),
            device,
        }
    }

    pub fn backend(&self) -> &D {
        &self.device
    }

    pub fn handle_get_info(&self, request: &[u8]) -> BlkGetInfoReply {
        let status = match BlkGetInfoRequest::decode(request) {
            Some(_) => BlkStatus::Success,
            None => BlkStatus::InvalidRequest,
        };
        let info = self.device.get_info();
        BlkGetInfoReply {
            status,
            _reserved0: 0,
            logical_block_size: info.logical_block_size,
            _reserved1: 0,
            total_blocks: info.total_blocks,
            feature_flags: info.feature_flags,
        }
    }

    pub fn handle_write(&mut self, request: &BlkWriteRequest) -> BlkWriteReply {
        if request.lba >= self.device.get_info().total_blocks {
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

        if self.device.write_sector(sector.lba, &sector.data).is_err() {
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

pub fn run_with_backend<D: BlockDeviceOps>(device: D) {
    let ctx = yarm_user_rt::runtime::startup_context();
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        return;
    };
    let mut write_service = BlockWriteService::with_backend(device);
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
                        let reply = write_service.handle_get_info(msg.as_slice());
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

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct MockBlockDevice<const SECTORS: usize> {
        storage: [[u8; 512]; SECTORS],
        reads: usize,
        writes: usize,
    }

    impl<const SECTORS: usize> Default for MockBlockDevice<SECTORS> {
        fn default() -> Self {
            Self {
                storage: [[0; 512]; SECTORS],
                reads: 0,
                writes: 0,
            }
        }
    }

    impl<const SECTORS: usize> BlockDeviceOps for MockBlockDevice<SECTORS> {
        fn get_info(&self) -> BlockDeviceInfo {
            BlockDeviceInfo {
                logical_block_size: 512,
                total_blocks: SECTORS as u64,
                feature_flags: 0,
            }
        }

        fn read_sector(&mut self, lba: u64) -> Result<[u8; 512], ()> {
            let data = self.storage.get(lba as usize).copied().ok_or(())?;
            self.reads += 1;
            Ok(data)
        }

        fn write_sector(&mut self, lba: u64, data: &[u8; 512]) -> Result<u32, ()> {
            let slot = self.storage.get_mut(lba as usize).ok_or(())?;
            *slot = *data;
            self.writes += 1;
            Ok(512)
        }
    }

    fn write_sector<D: BlockDeviceOps>(
        service: &mut BlockWriteService<D>,
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
    fn trait_backed_get_info_preserves_geometry_and_malformed_status() {
        let service = BlockWriteService::with_backend(MockBlockDevice::<4>::default());
        let valid = service.handle_get_info(&BlkGetInfoRequest { device_id: 1 }.encode());
        assert_eq!(valid.status, BlkStatus::Success);
        assert_eq!(valid.logical_block_size, 512);
        assert_eq!(valid.total_blocks, 4);
        assert_eq!(valid.feature_flags, 0);
        assert_eq!(
            service.handle_get_info(&[]).status,
            BlkStatus::InvalidRequest
        );
    }

    #[test]
    fn trait_backed_write_then_read_and_overwrite_are_exact() {
        let mut service = BlockWriteService::with_backend(MockBlockDevice::<4>::default());
        let first = [0x5a; 512];
        let second = core::array::from_fn(|index| index as u8);
        assert_eq!(
            write_sector(&mut service, 1, 2, &first).status,
            BlkStatus::Success
        );
        assert_eq!(service.read_sector(2), Ok(first));
        assert_eq!(
            write_sector(&mut service, 2, 2, &second).status,
            BlkStatus::Success
        );
        assert_eq!(service.read_sector(2), Ok(second));
        assert_eq!(service.backend().writes, 2);
        assert_eq!(service.backend().reads, 2);
    }

    #[test]
    fn trait_backed_out_of_range_write_is_rejected_without_mutation() {
        let mut service = BlockWriteService::with_backend(MockBlockDevice::<4>::default());
        let data = [0x11; 512];
        let reply = write_sector(&mut service, 9, 4, &data);
        assert_eq!(reply.status, BlkStatus::InvalidRequest);
        assert_eq!(service.backend().writes, 0);
    }

    #[test]
    fn generic_service_has_no_concrete_virtio_queue_or_device_dependency() {
        let source = include_str!("service.rs");
        for forbidden in [
            ["VirtioBlk", "MemoryDevice"].concat(),
            ["Virtq", "Chain"].concat(),
            ["build_", "write_chain"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "generic service contains {forbidden}"
            );
        }
        assert!(source.contains("BlockDeviceOps"));
        assert!(source.contains("VIRTIO_BLK_SRV_READY"));
        assert!(source.contains("VIRTIO_BLK_GET_INFO_REQUEST"));
    }
}
