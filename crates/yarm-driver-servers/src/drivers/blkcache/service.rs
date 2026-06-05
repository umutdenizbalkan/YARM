// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::blkcache_abi::*;
use yarm_ipc_abi::block_abi::{
    BLK_OP_GET_INFO, BLK_OP_WRITE, BlkAssembledSector, BlkGetInfoReply, BlkGetInfoRequest,
    BlkSectorWriteAssembler, BlkStatus, BlkWriteReply, BlkWriteRequest,
};
use yarm_ipc_abi::block_backend_abi::{
    BLK_BACKEND_OP_QUERY_STATE, BlkBackendQueryRequest, BlkBackendResponse,
};
use yarm_user_rt::ipc::Message;

#[derive(Clone, Copy)]
struct BackendRecord {
    backend_id: u64,
    backend_send_cap: u64,
    block_size: u32,
    _flags: u32,
    block_count: u64,
    registered: bool,
}

impl BackendRecord {
    const fn empty() -> Self {
        Self {
            backend_id: 0,
            backend_send_cap: 0,
            block_size: 0,
            _flags: 0,
            block_count: 0,
            registered: false,
        }
    }
}

const MAX_BACKENDS: usize = 8;
const CACHE_SLOTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CachedSector {
    valid: bool,
    backend_id: u64,
    lba: u64,
    data: [u8; 512],
}

impl CachedSector {
    const fn empty() -> Self {
        Self {
            valid: false,
            backend_id: 0,
            lba: 0,
            data: [0; 512],
        }
    }
}

#[derive(Debug)]
struct WriteThroughCache {
    assembler: BlkSectorWriteAssembler,
    sectors: [CachedSector; CACHE_SLOTS],
    next_slot: usize,
}

impl WriteThroughCache {
    const fn new() -> Self {
        Self {
            assembler: BlkSectorWriteAssembler::new(),
            sectors: [CachedSector::empty(); CACHE_SLOTS],
            next_slot: 0,
        }
    }

    fn write<F>(
        &mut self,
        backend: BackendRecord,
        request: &BlkWriteRequest,
        mut forward: F,
    ) -> BlkWriteReply
    where
        F: FnMut(&BlkWriteRequest) -> BlkWriteReply,
    {
        if backend.block_size != 512 || request.lba >= backend.block_count {
            self.assembler.reset();
            return block_reply(request, BlkStatus::InvalidRequest, 0, false);
        }
        let completed = match self.assembler.accept(request) {
            Ok(completed) => completed,
            Err(status) => return block_reply(request, status, 0, false),
        };
        let reply = forward(request);
        if reply.status != BlkStatus::Success
            || reply.request_id != request.request_id
            || reply.lba != request.lba
            || reply.bytes_accepted != request.data_len
        {
            self.assembler.reset();
            return block_reply(request, BlkStatus::IOError, 0, false);
        }
        if let Some(sector) = completed {
            if reply.sector_committed != 1 {
                self.assembler.reset();
                return block_reply(request, BlkStatus::IOError, 0, false);
            }
            self.store_sector(sector);
        }
        reply
    }

    fn store_sector(&mut self, sector: BlkAssembledSector) {
        let index = self
            .sectors
            .iter()
            .position(|entry| {
                entry.valid && entry.backend_id == sector.device_id && entry.lba == sector.lba
            })
            .unwrap_or_else(|| {
                let index = self.next_slot;
                self.next_slot = (self.next_slot + 1) % CACHE_SLOTS;
                index
            });
        self.sectors[index] = CachedSector {
            valid: true,
            backend_id: sector.device_id,
            lba: sector.lba,
            data: sector.data,
        };
    }

    #[cfg(test)]
    fn read_sector(&self, backend_id: u64, lba: u64) -> Option<[u8; 512]> {
        self.sectors
            .iter()
            .find(|entry| entry.valid && entry.backend_id == backend_id && entry.lba == lba)
            .map(|entry| entry.data)
    }
}

fn block_reply(
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

fn return_reply(reply_cap: u32, opcode: u16, request_id: u64, status: u32) {
    let response = BlkCacheResponse {
        request_id,
        status,
        bytes_moved: 0,
        flags: 0,
    };
    if let Ok(reply) = Message::with_header(0, opcode, 0, None, &response.encode()) {
        let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
    }
}

fn find_backend(table: &[BackendRecord; MAX_BACKENDS], backend_id: u64) -> Option<BackendRecord> {
    table
        .iter()
        .copied()
        .find(|r| r.registered && r.backend_id == backend_id)
}

fn first_registered_backend(table: &[BackendRecord; MAX_BACKENDS]) -> Option<BackendRecord> {
    table.iter().copied().find(|r| r.registered)
}

fn call_backend_get_info(
    backend_send_cap: u64,
    reply_recv_cap: u32,
) -> Result<BlkGetInfoReply, ()> {
    let req = BlkGetInfoRequest { device_id: 0 };
    let Ok(msg) = Message::with_header(0, BLK_OP_GET_INFO, 0, None, &req.encode()) else {
        return Err(());
    };
    unsafe { yarm_user_rt::syscall::ipc_call(backend_send_cap as u32, reply_recv_cap, &msg) }
        .map_err(|_| ())?;
    let reply = unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(reply_recv_cap, 0) }
        .map_err(|_| ())?;
    match reply {
        Some(reply_msg) => BlkGetInfoReply::decode(reply_msg.as_slice()).ok_or(()),
        None => Err(()),
    }
}

fn call_backend_write(
    backend_send_cap: u64,
    reply_recv_cap: u32,
    request: &BlkWriteRequest,
) -> Result<BlkWriteReply, ()> {
    let payload = request.encode().map_err(|_| ())?;
    let message = Message::with_header(0, BLK_OP_WRITE, 0, None, &payload).map_err(|_| ())?;
    unsafe { yarm_user_rt::syscall::ipc_call(backend_send_cap as u32, reply_recv_cap, &message) }
        .map_err(|_| ())?;
    let reply = unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(reply_recv_cap, 0) }
        .map_err(|_| ())?;
    reply
        .and_then(|message| BlkWriteReply::decode(message.as_slice()))
        .ok_or(())
}

fn probe_backend_query_state(backend_id: u64, backend_send_cap: u64, reply_recv_cap: u32) {
    yarm_user_rt::user_log!(
        "BLKCACHE_BACKEND_QUERY_STATE_BEGIN backend_id={}",
        backend_id
    );
    let req = BlkBackendQueryRequest {
        req_id: 0xB10C,
        flags: 0,
        device_id: backend_id,
    };
    let payload = req.encode();
    let Ok(msg) = Message::with_header(0, BLK_BACKEND_OP_QUERY_STATE, 0, None, &payload) else {
        yarm_user_rt::user_log!(
            "BLKCACHE_BACKEND_QUERY_STATE_ERR backend_id={} err=BuildMessageFailed",
            backend_id
        );
        return;
    };
    match unsafe { yarm_user_rt::syscall::ipc_call(backend_send_cap as u32, reply_recv_cap, &msg) }
    {
        Ok(()) => {}
        Err(e) => {
            yarm_user_rt::user_log!(
                "BLKCACHE_BACKEND_QUERY_STATE_ERR backend_id={} err={:?}",
                backend_id,
                e
            );
            return;
        }
    }
    match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(reply_recv_cap, 0) } {
        Ok(Some(reply_msg)) => {
            if let Some(resp) = BlkBackendResponse::decode(reply_msg.as_slice()) {
                yarm_user_rt::user_log!(
                    "BLKCACHE_BACKEND_QUERY_STATE_RETURN backend_id={} status={} logical_block_size={} physical_block_size={}",
                    backend_id,
                    resp.status,
                    resp.logical_block_size,
                    resp.physical_block_size
                );
            }
        }
        Ok(None) => {
            yarm_user_rt::user_log!(
                "BLKCACHE_BACKEND_QUERY_STATE_ERR backend_id={} err=NoReply",
                backend_id
            );
        }
        Err(e) => {
            yarm_user_rt::user_log!(
                "BLKCACHE_BACKEND_QUERY_STATE_ERR backend_id={} err={:?}",
                backend_id,
                e
            );
        }
    }
}

fn register_backend(
    table: &mut [BackendRecord; MAX_BACKENDS],
    args: RegisterBackendArgs,
    transferred_backend_send_cap: u64,
) -> u32 {
    if args.backend_id == 0 || transferred_backend_send_cap == 0 || args.block_size == 0 {
        return BLKCACHE_STATUS_ERR_BAD_REQUEST;
    }
    for rec in table.iter() {
        if rec.registered && rec.backend_id == args.backend_id {
            return BLKCACHE_STATUS_ERR_BUSY;
        }
    }
    for rec in table.iter_mut() {
        if !rec.registered {
            *rec = BackendRecord {
                backend_id: args.backend_id,
                backend_send_cap: transferred_backend_send_cap,
                block_size: args.block_size,
                _flags: args.flags,
                block_count: args.block_count,
                registered: true,
            };
            yarm_user_rt::user_log!(
                "BLKCACHE_BACKEND_REGISTERED backend_id={} block_size={}",
                args.backend_id,
                args.block_size
            );
            yarm_user_rt::user_log!(
                "BLKCACHE_BACKEND_REGISTER_CAP_TRANSFER backend_id={} cap={}",
                args.backend_id,
                transferred_backend_send_cap
            );
            return BLKCACHE_STATUS_OK;
        }
    }
    BLKCACHE_STATUS_ERR_NO_MEMORY
}

pub fn run() {
    yarm_user_rt::user_log!("BLKCACHE_SRV_ENTRY");
    let ctx = yarm_user_rt::runtime::startup_context();
    let pm_reply_recv_cap = ctx.process_manager_reply_recv_cap;
    if let Some(cap) = pm_reply_recv_cap {
        yarm_user_rt::user_log!("BLKCACHE_BACKEND_REPLY_RECV_CAP cap={}", cap);
    }
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        yarm_user_rt::user_log!("BLKCACHE_NO_RECV_CAP");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    };
    yarm_user_rt::user_log!("BLKCACHE_SRV_RECV_CAP cap={}", recv_cap);
    let mut backends = [BackendRecord::empty(); MAX_BACKENDS];
    let mut write_cache = WriteThroughCache::new();
    yarm_user_rt::user_log!("BLKCACHE_SRV_READY");
    yarm_user_rt::user_log!("BLKCACHE_BLOCKING_RECV_LOOP");

    loop {
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some(received)) => {
                let msg = received.message;
                let reply_cap = received.reply_cap;
                yarm_user_rt::user_log!(
                    "BLKCACHE_RECV_MSG opcode={} len={}",
                    msg.opcode,
                    msg.as_slice().len()
                );
                yarm_user_rt::user_log!(
                    "BLKCACHE_RECV_CAPS reply_cap={:?} transferred_cap={:?}",
                    reply_cap,
                    received.transferred_cap
                );
                if msg.opcode == BLKCACHE_OP_WRITE_BLOCK {
                    yarm_user_rt::user_log!("BLKCACHE_OP_WRITE_BLOCK");
                    let reply = match BlkWriteRequest::decode(msg.as_slice()) {
                        Ok(request) => {
                            let backend = find_backend(&backends, request.device_id);
                            match (backend, pm_reply_recv_cap) {
                                (Some(backend), Some(reply_recv_cap)) => {
                                    write_cache.write(backend, &request, |chunk| {
                                        call_backend_write(
                                            backend.backend_send_cap,
                                            reply_recv_cap,
                                            chunk,
                                        )
                                        .unwrap_or_else(
                                            |_| block_reply(chunk, BlkStatus::IOError, 0, false),
                                        )
                                    })
                                }
                                (None, _) => {
                                    block_reply(&request, BlkStatus::DeviceUnavailable, 0, false)
                                }
                                (_, None) => block_reply(&request, BlkStatus::NotReady, 0, false),
                            }
                        }
                        Err(status) => BlkWriteReply {
                            request_id: 0,
                            status,
                            bytes_accepted: 0,
                            sector_committed: 0,
                            lba: 0,
                        },
                    };
                    if let Some(reply_cap) = reply_cap {
                        if let Ok(message) = Message::with_header(
                            0,
                            BLKCACHE_OP_WRITE_BLOCK,
                            0,
                            None,
                            &reply.encode(),
                        ) {
                            let _ =
                                unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &message) };
                        }
                    }
                    continue;
                }
                let (request_id, status) = match msg.opcode {
                    BLK_OP_GET_INFO => {
                        yarm_user_rt::user_log!("BLKCACHE_GET_INFO_CALL_BEGIN");
                        let Some(reply_recv_cap) = pm_reply_recv_cap else {
                            yarm_user_rt::user_log!(
                                "BLKCACHE_GET_INFO_CALL_RETURN ok=0 status={}",
                                BlkStatus::DeviceUnavailable as u32
                            );
                            if let Some(reply_cap) = reply_cap {
                                let resp = BlkGetInfoReply {
                                    status: BlkStatus::DeviceUnavailable,
                                    _reserved0: 0,
                                    logical_block_size: 512,
                                    _reserved1: 0,
                                    total_blocks: 0,
                                    feature_flags: 0,
                                };
                                if let Ok(reply) = Message::with_header(
                                    0,
                                    BLK_OP_GET_INFO,
                                    0,
                                    None,
                                    &resp.encode(),
                                ) {
                                    let _ = unsafe {
                                        yarm_user_rt::syscall::ipc_reply(reply_cap, &reply)
                                    };
                                }
                            }
                            continue;
                        };
                        let backend = find_backend(&backends, 1)
                            .or_else(|| first_registered_backend(&backends));
                        let response = if let Some(rec) = backend {
                            yarm_user_rt::user_log!(
                                "BLKCACHE_FORWARD_GET_INFO backend_id={} send_cap={}",
                                rec.backend_id,
                                rec.backend_send_cap
                            );
                            match call_backend_get_info(rec.backend_send_cap, reply_recv_cap) {
                                Ok(reply) => {
                                    yarm_user_rt::user_log!(
                                        "BLKCACHE_FORWARD_GET_INFO_REPLY ok=1 status={}",
                                        reply.status as u32
                                    );
                                    Some(reply)
                                }
                                Err(()) => {
                                    yarm_user_rt::user_log!(
                                        "BLKCACHE_FORWARD_GET_INFO_REPLY ok=0 status={}",
                                        BlkStatus::DeviceUnavailable as u32
                                    );
                                    None
                                }
                            }
                        } else {
                            yarm_user_rt::user_log!("BLKCACHE_NO_BACKEND_REGISTERED");
                            None
                        };
                        let resp = response.unwrap_or(BlkGetInfoReply {
                            status: BlkStatus::NotReady,
                            _reserved0: 0,
                            logical_block_size: 512,
                            _reserved1: 0,
                            total_blocks: 0,
                            feature_flags: 0,
                        });
                        yarm_user_rt::user_log!(
                            "BLKCACHE_GET_INFO_CALL_RETURN ok=1 status={}",
                            resp.status as u32
                        );
                        if let Some(reply_cap) = reply_cap
                            && let Ok(reply) =
                                Message::with_header(0, BLK_OP_GET_INFO, 0, None, &resp.encode())
                        {
                            let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
                        }
                        continue;
                    }
                    BLKCACHE_OP_REGISTER_BACKEND => {
                        match RegisterBackendArgs::decode(msg.as_slice()) {
                            Some(args) => {
                                yarm_user_rt::user_log!(
                                    "BLKCACHE_OP_REGISTER_BACKEND backend_id={} payload_len={}",
                                    args.backend_id,
                                    msg.as_slice().len()
                                );
                                let tx_cap = received.transferred_cap.map(|c| c as u64);
                                yarm_user_rt::user_log!(
                                    "BLKCACHE_BACKEND_REGISTER_RECV transferred_cap={:?}",
                                    tx_cap
                                );
                                let Some(tx_cap) = tx_cap else {
                                    yarm_user_rt::user_log!(
                                        "BLKCACHE_BACKEND_REGISTER_REJECT backend_id={} reason=MissingTransferredCap",
                                        args.backend_id
                                    );
                                    continue;
                                };
                                {
                                    let status = register_backend(&mut backends, args, tx_cap);
                                    if status == BLKCACHE_STATUS_OK {
                                        yarm_user_rt::user_log!(
                                            "BLKCACHE_BACKEND_PROBE_AFTER_REGISTER backend_id={}",
                                            args.backend_id
                                        );
                                        if let Some(reply_cap) = pm_reply_recv_cap {
                                            if let Some(rec) =
                                                find_backend(&backends, args.backend_id)
                                            {
                                                probe_backend_query_state(
                                                    rec.backend_id,
                                                    rec.backend_send_cap,
                                                    reply_cap,
                                                );
                                            } else {
                                                yarm_user_rt::user_log!(
                                                    "BLKCACHE_BACKEND_QUERY_STATE_SKIP backend_id={} reason=BackendNotFoundAfterRegister",
                                                    args.backend_id
                                                );
                                            }
                                        } else {
                                            yarm_user_rt::user_log!(
                                                "BLKCACHE_BACKEND_QUERY_STATE_SKIP backend_id={} reason=NoReplyRecvCap",
                                                args.backend_id
                                            );
                                        }
                                    }
                                    yarm_user_rt::user_log!(
                                        "BLKCACHE_REGISTER_NO_REPLY_PATH backend_id={} status={}",
                                        args.backend_id,
                                        status
                                    );
                                    continue;
                                }
                            }
                            None => {
                                yarm_user_rt::user_log!(
                                    "BLKCACHE_BACKEND_REGISTER_REJECT backend_id=0 reason=DecodeFailed payload_len={}",
                                    msg.as_slice().len()
                                );
                                continue;
                            }
                        }
                    }
                    BLKCACHE_OP_REGISTER_BUFFER => {
                        yarm_user_rt::user_log!("BLKCACHE_OP_REGISTER_BUFFER");
                        RegisterBufferArgs::decode(msg.as_slice())
                            .map(|a| (a.buffer_id, BLKCACHE_STATUS_ERR_UNSUPPORTED))
                            .unwrap_or((0, BLKCACHE_STATUS_ERR_BAD_REQUEST))
                    }
                    BLKCACHE_OP_UNREGISTER_BUFFER => {
                        yarm_user_rt::user_log!("BLKCACHE_OP_UNREGISTER_BUFFER");
                        UnregisterBufferArgs::decode(msg.as_slice())
                            .map(|a| (a.buffer_id, BLKCACHE_STATUS_ERR_UNSUPPORTED))
                            .unwrap_or((0, BLKCACHE_STATUS_ERR_BAD_REQUEST))
                    }
                    BLKCACHE_OP_READ_BLOCK => {
                        yarm_user_rt::user_log!("BLKCACHE_OP_READ_BLOCK");
                        BlockIoRequest::decode(msg.as_slice())
                            .map(|a| (a.request_id, BLKCACHE_STATUS_ERR_UNSUPPORTED))
                            .unwrap_or((0, BLKCACHE_STATUS_ERR_BAD_REQUEST))
                    }
                    BLKCACHE_OP_FLUSH => {
                        yarm_user_rt::user_log!("BLKCACHE_OP_FLUSH");
                        RangeRequest::decode(msg.as_slice())
                            .map(|a| (a.request_id, BLKCACHE_STATUS_ERR_UNSUPPORTED))
                            .unwrap_or((0, BLKCACHE_STATUS_ERR_BAD_REQUEST))
                    }
                    BLKCACHE_OP_INVALIDATE => {
                        yarm_user_rt::user_log!("BLKCACHE_OP_INVALIDATE");
                        RangeRequest::decode(msg.as_slice())
                            .map(|a| (a.request_id, BLKCACHE_STATUS_ERR_UNSUPPORTED))
                            .unwrap_or((0, BLKCACHE_STATUS_ERR_BAD_REQUEST))
                    }
                    BLKCACHE_OP_GET_STATS => {
                        yarm_user_rt::user_log!("BLKCACHE_OP_GET_STATS");
                        GetStatsRequest::decode(msg.as_slice())
                            .map(|a| (a.request_id, BLKCACHE_STATUS_ERR_UNSUPPORTED))
                            .unwrap_or((0, BLKCACHE_STATUS_ERR_BAD_REQUEST))
                    }
                    BLKCACHE_OP_CANCEL => {
                        yarm_user_rt::user_log!("BLKCACHE_OP_CANCEL");
                        CancelRequest::decode(msg.as_slice())
                            .map(|a| (a.request_id, BLKCACHE_STATUS_ERR_UNSUPPORTED))
                            .unwrap_or((0, BLKCACHE_STATUS_ERR_BAD_REQUEST))
                    }
                    _ => (0, BLKCACHE_STATUS_ERR_BAD_REQUEST),
                };
                if let Some(reply_cap) = reply_cap {
                    return_reply(reply_cap, msg.opcode, request_id, status);
                }
            }
            Ok(None) => {}
            Err(_e) => {
                let _ = yarm_user_rt::syscall::yield_now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_backend_happy_and_duplicate() {
        let mut t = [BackendRecord::empty(); MAX_BACKENDS];
        let a = RegisterBackendArgs {
            backend_id: 1,
            backend_send_cap: 9,
            block_size: 512,
            flags: 0,
            block_count: 1,
        };
        assert_eq!(register_backend(&mut t, a, 9), BLKCACHE_STATUS_OK);
        assert_eq!(register_backend(&mut t, a, 9), BLKCACHE_STATUS_ERR_BUSY);
    }

    #[test]
    fn register_backend_invalid_and_capacity() {
        let mut t = [BackendRecord::empty(); MAX_BACKENDS];
        let bad = RegisterBackendArgs {
            backend_id: 0,
            backend_send_cap: 1,
            block_size: 512,
            flags: 0,
            block_count: 1,
        };
        assert_eq!(
            register_backend(&mut t, bad, 1),
            BLKCACHE_STATUS_ERR_BAD_REQUEST
        );
        for i in 0..MAX_BACKENDS {
            let ok = RegisterBackendArgs {
                backend_id: (i + 1) as u64,
                backend_send_cap: (i + 2) as u64,
                block_size: 512,
                flags: 0,
                block_count: 1,
            };
            assert_eq!(
                register_backend(&mut t, ok, (i + 2) as u64),
                BLKCACHE_STATUS_OK
            );
        }
        let extra = RegisterBackendArgs {
            backend_id: 99,
            backend_send_cap: 77,
            block_size: 512,
            flags: 0,
            block_count: 1,
        };
        assert_eq!(
            register_backend(&mut t, extra, 77),
            BLKCACHE_STATUS_ERR_NO_MEMORY
        );
    }

    fn test_backend(block_count: u64) -> BackendRecord {
        BackendRecord {
            backend_id: 1,
            backend_send_cap: 9,
            block_size: 512,
            _flags: 0,
            block_count,
            registered: true,
        }
    }

    fn write_sector(
        cache: &mut WriteThroughCache,
        backend: BackendRecord,
        downstream: &mut crate::drivers::virtio_blk::service::VirtioBlkWriteService<4>,
        request_id: u32,
        lba: u64,
        data: &[u8; 512],
    ) -> BlkWriteReply {
        use yarm_ipc_abi::block_abi::{
            BLK_SECTOR_SIZE, BLK_WRITE_F_FIRST, BLK_WRITE_F_LAST, BLK_WRITE_MAX_CHUNK_BYTES,
        };
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
                device_id: backend.backend_id,
                lba,
                sector_offset: offset as u32,
                data_len: len as u32,
                data: chunk,
            };
            final_reply =
                Some(cache.write(backend, &request, |chunk| downstream.handle_write(chunk)));
            offset += len;
        }
        final_reply.expect("write reply")
    }

    #[test]
    fn blkcache_write_through_then_read_and_overwrite_are_exact() {
        let backend = test_backend(4);
        let mut cache = WriteThroughCache::new();
        let mut downstream = crate::drivers::virtio_blk::service::VirtioBlkWriteService::<4>::new();
        let first = [0x6d; 512];
        let second = core::array::from_fn(|index| (index * 5) as u8);

        assert_eq!(
            write_sector(&mut cache, backend, &mut downstream, 1, 2, &first).status,
            BlkStatus::Success
        );
        assert_eq!(cache.read_sector(1, 2), Some(first));
        assert_eq!(downstream.read_sector(2), Ok(first));

        assert_eq!(
            write_sector(&mut cache, backend, &mut downstream, 2, 2, &second).status,
            BlkStatus::Success
        );
        assert_eq!(cache.read_sector(1, 2), Some(second));
        assert_eq!(downstream.read_sector(2), Ok(second));
    }

    #[test]
    fn blkcache_rejects_out_of_range_and_does_not_cache_failed_write() {
        use yarm_ipc_abi::block_abi::{BLK_WRITE_F_FIRST, BLK_WRITE_MAX_CHUNK_BYTES};
        let backend = test_backend(1);
        let mut cache = WriteThroughCache::new();
        let request = BlkWriteRequest {
            request_id: 1,
            flags: BLK_WRITE_F_FIRST,
            device_id: 1,
            lba: 1,
            sector_offset: 0,
            data_len: 1,
            data: [0xaa; BLK_WRITE_MAX_CHUNK_BYTES],
        };
        let reply = cache.write(backend, &request, |_| {
            block_reply(&request, BlkStatus::Success, 1, false)
        });
        assert_eq!(reply.status, BlkStatus::InvalidRequest);
        assert_eq!(cache.read_sector(1, 1), None);

        let request = BlkWriteRequest { lba: 0, ..request };
        let reply = cache.write(backend, &request, |_| {
            block_reply(&request, BlkStatus::IOError, 0, false)
        });
        assert_eq!(reply.status, BlkStatus::IOError);
        assert_eq!(cache.read_sector(1, 0), None);
    }
}
