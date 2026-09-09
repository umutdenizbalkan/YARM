// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! NR 30 `RecvSharedV3` syscall handler and helpers.
//!
//! Mechanically split from the parent `syscall.rs` module with zero behavior
//! change. The dispatch arm in `syscall.rs` (`Syscall::RecvSharedV3 =>
//! handle_recv_shared_v3`) is unchanged; this module only hosts the moved
//! body. See `doc/KERNEL_UNLOCKING.md` for the D4 step 1 tracking entry.

use super::{
    SYSCALL_ABI_VERSION, SYSCALL_NO_TRANSFER_CAP, SyscallError, current_tid,
    materialize_received_message_cap, record_user_fault, validate_endpoint_right,
};
use crate::kernel::boot::KernelState;
use crate::kernel::capabilities::{CapId, CapRights};
use crate::kernel::ipc::Message;
use crate::kernel::trap::FaultAccess;
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::{CachePolicy, Mapping, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

// ── Stage 42+43: recv_shared_v3 helpers ──────────────────────────────────────

/// Parse a `RecvSharedV3Request` from a raw byte buffer at the wire-format offsets.
///
/// Bytes below 64 are required; bytes [64..80] (the `reserved` fields) default
/// to zero when absent so validation still passes for a minimal 64-byte record.
pub(crate) fn parse_v3_request_bytes(
    buf: &[u8],
) -> crate::kernel::recv_core::recv_shared_v3::RecvSharedV3Request {
    use crate::kernel::recv_core::recv_shared_v3::RecvSharedV3Request;
    macro_rules! u32le {
        ($off:expr) => {
            u32::from_le_bytes([buf[$off], buf[$off + 1], buf[$off + 2], buf[$off + 3]])
        };
    }
    macro_rules! u64le {
        ($off:expr) => {
            if buf.len() >= $off + 8 {
                u64::from_le_bytes([
                    buf[$off],
                    buf[$off + 1],
                    buf[$off + 2],
                    buf[$off + 3],
                    buf[$off + 4],
                    buf[$off + 5],
                    buf[$off + 6],
                    buf[$off + 7],
                ])
            } else {
                0u64
            }
        };
    }
    RecvSharedV3Request {
        version: u32le!(0),
        record_len: u32le!(4),
        endpoint_cap: u64le!(8),
        payload_ptr: u64le!(16),
        payload_len: u64le!(24),
        metadata_ptr: u64le!(32),
        metadata_len: u64le!(40),
        map_intent: u32le!(48),
        flags: u32le!(52),
        timeout_ticks: u64le!(56),
        reserved: [u64le!(64), u64le!(72)],
    }
}

/// Write a v3 output record to user memory at `out_ptr` if the buffer is valid.
///
/// `out_ptr == 0` or `out_len < 80` — silently skip (caller may call with
/// metadata_ptr/metadata_len from the request without a null check).
///
/// Writes `min(out_len, 120)` bytes so callers with larger buffers receive
/// new fields without breaking existing 80-byte or 88-byte callers.
///
/// Byte layout (must match `#[repr(C)] RecvSharedV3Output` field offsets):
///   [0..40]   authoritative fields (version … transferred_cap)
///   [40..44]  object_kind (u32)
///   [44..48]  0 (C-layout padding before u64)
///   [48..56]  object_generation (u64)
///   [56..60]  effective_rights (u32)
///   [60..64]  0 (C-layout padding before u64)
///   [64..72]  exact_object_size (u64) — authoritative for MemoryObject (Stage 49); 0 otherwise
///   [72..80]  region_offset — always 0 (FUTURE)
///   [80..88]  exact_region_len (u64) — authoritative for DmaRegion (Stage 50); 0 otherwise
///   [88..96]  mapped_base (u64) — VA of live mapping; 0 if no mapping (Stage 58+59)
///   [96..104] page_rounded_mapped_len (u64) — 0 if no mapping (Stage 58+59)
///   [104..108] actual_mapping_perm (u32) — 1=RO, 3=RW, 0=none (Stage 58+59)
///   [108..112] C-layout padding
///   [112..120] cleanup_token (u64) — nonzero when mapping live (Stage 58+59)
/// U9-XFER2 §3 — the OUTPUT ENCODER, factored out of the acquisition.
///
/// Building the record and writing it to user memory used to be one function that took
/// `&mut KernelState`, which is what tied the whole of NR 30's output to the broad route. The
/// bytes are the same bytes; what changed is that producing them no longer requires an
/// acquisition, so both routes encode identically and differ only in which owner performs the
/// copy.
#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_v3_output(
    out_len: u64,
    result_status: u32,
    sender_tid: u64,
    message_len: u32,
    message_flags: u32,
    transferred_cap: u64,
    object_kind: u32,
    object_generation: u64,
    effective_rights: u32,
    exact_object_size: u64,
    exact_region_len: u64,
    mapped_base: u64,
    page_rounded_mapped_len: u64,
    actual_mapping_perm: u32,
    cleanup_token: u64,
) -> Option<([u8; 120], usize)> {
    use crate::kernel::recv_core::recv_shared_v3::{V3_MIN_OUTPUT_LEN, V3_VERSION};
    if out_len < V3_MIN_OUTPUT_LEN as u64 {
        return None;
    }
    let mut out = [0u8; 120];
    out[0..4].copy_from_slice(&V3_VERSION.to_le_bytes());
    out[4..8].copy_from_slice(&(V3_MIN_OUTPUT_LEN as u32).to_le_bytes());
    out[8..12].copy_from_slice(&(SYSCALL_ABI_VERSION as u32).to_le_bytes());
    out[12..16].copy_from_slice(&result_status.to_le_bytes());
    out[16..24].copy_from_slice(&sender_tid.to_le_bytes());
    out[24..28].copy_from_slice(&message_len.to_le_bytes());
    out[28..32].copy_from_slice(&message_flags.to_le_bytes());
    out[32..40].copy_from_slice(&transferred_cap.to_le_bytes());
    out[40..44].copy_from_slice(&object_kind.to_le_bytes());
    out[48..56].copy_from_slice(&object_generation.to_le_bytes());
    out[56..60].copy_from_slice(&effective_rights.to_le_bytes());
    out[64..72].copy_from_slice(&exact_object_size.to_le_bytes());
    out[80..88].copy_from_slice(&exact_region_len.to_le_bytes());
    out[88..96].copy_from_slice(&mapped_base.to_le_bytes());
    out[96..104].copy_from_slice(&page_rounded_mapped_len.to_le_bytes());
    out[104..108].copy_from_slice(&actual_mapping_perm.to_le_bytes());
    out[112..120].copy_from_slice(&cleanup_token.to_le_bytes());
    Some((out, (out_len as usize).min(120)))
}

#[allow(clippy::too_many_arguments)]
fn write_v3_output_to_user(
    kernel: &mut KernelState,
    out_ptr: u64,
    out_len: u64,
    result_status: u32,
    sender_tid: u64,
    message_len: u32,
    message_flags: u32,
    transferred_cap: u64,
    object_kind: u32,
    object_generation: u64,
    effective_rights: u32,
    exact_object_size: u64,
    exact_region_len: u64,
    mapped_base: u64,
    page_rounded_mapped_len: u64,
    actual_mapping_perm: u32,
    cleanup_token: u64,
) -> bool {
    if out_ptr == 0 {
        return false;
    }
    // U9-XFER2 §3: ONE encoder. This is the broad route's acquisition around it.
    let Some((out, write_len)) = encode_v3_output(
        out_len,
        result_status,
        sender_tid,
        message_len,
        message_flags,
        transferred_cap,
        object_kind,
        object_generation,
        effective_rights,
        exact_object_size,
        exact_region_len,
        mapped_base,
        page_rounded_mapped_len,
        actual_mapping_perm,
        cleanup_token,
    ) else {
        return false;
    };
    kernel
        .copy_to_current_user(out_ptr as usize, &out[..write_len])
        .is_ok()
}

/// Map a [`CapObject`] variant to its `RecvSharedV3ObjectKind` discriminant.
pub(crate) fn recv_v3_object_kind(obj: crate::kernel::capabilities::CapObject) -> u32 {
    use crate::kernel::capabilities::CapObject;
    match obj {
        CapObject::MemoryObject { .. } => 1,
        CapObject::Endpoint { .. } => 2,
        CapObject::Reply { .. } => 3,
        CapObject::Notification { .. } => 4,
        // Stage 52+53: DmaRegion is now a first-class object kind (discriminant 5).
        CapObject::DmaRegion { .. } => 5,
        _ => 0xFF,
    }
}

/// Return the object generation stored in a [`CapObject`], or 0 if unavailable.
pub(crate) fn recv_v3_object_generation(obj: crate::kernel::capabilities::CapObject) -> u64 {
    use crate::kernel::capabilities::CapObject;
    match obj {
        CapObject::Endpoint { generation, .. } => generation,
        CapObject::Notification { generation, .. } => generation,
        CapObject::Reply { generation, .. } => generation,
        _ => 0,
    }
}

/// Return the exact byte size of a [`CapObject::MemoryObject`] from the kernel registry.
///
/// Returns the page-aligned byte length stored in `MemorySubsystem.memory_objects`.
/// Returns 0 for all other cap kinds (not fabricated — genuinely unavailable).
fn recv_v3_exact_object_size(
    kernel: &KernelState,
    obj: crate::kernel::capabilities::CapObject,
) -> u64 {
    use crate::kernel::capabilities::CapObject;
    let CapObject::MemoryObject { id } = obj else {
        return 0;
    };
    kernel.with_memory_state(|memory| {
        memory
            .memory_objects
            .iter()
            .flatten()
            .find(|entry| entry.id == id)
            .map(|entry| entry.len as u64)
            .unwrap_or(0)
    })
}

/// Return the exact byte length of a [`CapObject::DmaRegion`] sub-region.
///
/// The length is embedded directly in the cap — no registry lookup needed.
/// Returns 0 for all other cap kinds (not fabricated — genuinely unavailable).
pub(crate) fn recv_v3_exact_region_len(obj: crate::kernel::capabilities::CapObject) -> u64 {
    use crate::kernel::capabilities::CapObject;
    match obj {
        CapObject::DmaRegion { len, .. } => len,
        _ => 0,
    }
}

/// VALIDATION: SPLIT_FAST_PATH_ONLY
/// Stage 101 (audit): NR 30 RecvSharedV3 reuses the `try_recv_core_user_plain`
/// split-recv adapter for the dequeue+writeback. The trap-entry seam itself
/// still routes NR 30 through the global-lock dispatch (`dispatch()`), but the
/// IPC dequeue inside this handler runs against the same split adapter as
/// Stage 36. See doc/KERNEL_UNLOCKING.md
///
/// Stage 42+43: handle the `recv_shared_v3` syscall (NR 30).
///
/// # Constraints (Stage 42+43)
///
/// - **Non-blocking only**: `timeout_ticks` must be 0.  Blocking paths require
///   `RecvAbiVariant::RecvSharedV3` in task.rs — deferred to a future stage.
/// - **No mapped receive**: `map_intent` must be 0.  VM mapping on the split
///   path is not yet proven equivalent.
/// - **Cap-transfer**: fully supported via the canonical receive core
///   (`ipc_try_recv_queued_with_cap_transfer`); rollback on writeback failure.
///
/// # ABI
///
/// - `arg0` = `req_ptr` — pointer to a `RecvSharedV3Request` record in user space.
/// - `arg1` = `req_len` — byte length of the record (≥ 64 required).
/// - Output written to `request.metadata_ptr` (if non-null, len ≥ 80).
/// - Frame registers on success: `ret0` = sender_tid, `ret1` = message_len,
///   `ret2` = transferred_cap (or `SYSCALL_NO_TRANSFER_CAP`).
pub(super) fn handle_recv_shared_v3(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    use crate::kernel::recv_core::recv_shared_v3::{V3_MIN_REQUEST_LEN, validate_v3_request};
    use crate::kernel::syscall::recv_v3_txn::{V3Delivery, run_recv_v3_transaction};

    let req_ptr = frame.arg(0);
    let req_len = frame.arg(1);
    if req_len < V3_MIN_REQUEST_LEN as usize {
        return Err(SyscallError::InvalidArgs);
    }
    let read_len = req_len.min(80);
    let mut req_bytes = [0u8; 80];
    kernel
        .copy_from_current_user_into_slice(req_ptr, read_len, &mut req_bytes[..read_len])
        .map_err(|_| SyscallError::PageFault)?;
    let req = parse_v3_request_bytes(&req_bytes);
    if validate_v3_request(&req).is_err() {
        return Err(SyscallError::InvalidArgs);
    }
    // Blocking is unimplemented (Stage 42+43) and stays that way: adding a blocking mode is out
    // of scope, and this is the answer the ABI already gives.
    if req.timeout_ticks != 0 {
        return Err(SyscallError::WouldBlock);
    }
    if req.map_intent != 0
        && req.metadata_len < crate::kernel::recv_core::recv_shared_v3::V3_LIVE_OUTPUT_LEN as u64
    {
        return Err(SyscallError::InvalidArgs);
    }

    let tid = current_tid(kernel)?;
    crate::yarm_log!("RECV_V3_ENTER tid={} cap={}", tid, req.endpoint_cap);
    let mut owners = BroadRecvV3Owners { kernel, tid };
    match run_recv_v3_transaction(&mut owners, &req) {
        Ok(V3Delivery::Mapped {
            sender_tid,
            xfer_cap,
        }) => {
            frame.set_ok(
                usize::try_from(sender_tid).unwrap_or(0),
                0,
                usize::try_from(xfer_cap).unwrap_or(usize::MAX),
            );
            Ok(())
        }
        Ok(V3Delivery::Plain {
            sender_tid,
            payload_len,
            xfer_cap,
        }) => {
            frame.set_ok(
                usize::try_from(sender_tid).unwrap_or(0),
                payload_len,
                usize::try_from(xfer_cap).unwrap_or(usize::MAX),
            );
            Ok(())
        }
        Ok(V3Delivery::PayloadFault { user_ptr }) => {
            // §58 semantics, unchanged: the message IS consumed, the fault is recorded against
            // the faulting pointer, and the syscall returns Ok without a result lane.
            record_user_fault(kernel, frame, user_ptr, FaultAccess::Write);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// U9-XFER2 §3 — the BROAD adapter for NR 30.
///
/// Holds no policy: it resolves the owners out of `&mut KernelState` and runs the SAME
/// [`crate::kernel::syscall::recv_v3_txn::run_recv_v3_transaction`] the split adapter runs. The
/// caller's ASID is resolved once, up front, and passed explicitly — the same discipline the
/// split route needs, applied here so the two cannot disagree about which address space they are
/// touching after the sender wake.
pub(crate) struct BroadRecvV3Owners<'a> {
    pub(crate) kernel: &'a mut KernelState,
    pub(crate) tid: u64,
}

impl crate::kernel::syscall::recv_v3_txn::RecvV3Owners for BroadRecvV3Owners<'_> {
    fn caller_tid(&mut self) -> u64 {
        self.tid
    }

    fn caller_asid(&mut self) -> Option<crate::kernel::vm::Asid> {
        self.kernel.task_asid(self.tid)
    }

    fn read_user(
        &mut self,
        asid: crate::kernel::vm::Asid,
        ptr: usize,
        len: usize,
    ) -> Option<[u8; 80]> {
        let wide = self
            .kernel
            .copy_from_user(asid, crate::kernel::vm::VirtAddr(ptr as u64), len)
            .ok()?;
        let mut out = [0u8; 80];
        let take = len.min(80);
        out[..take].copy_from_slice(&wide[..take]);
        Some(out)
    }

    fn write_user(&mut self, asid: crate::kernel::vm::Asid, ptr: usize, bytes: &[u8]) -> bool {
        self.kernel
            .copy_to_user(asid, crate::kernel::vm::VirtAddr(ptr as u64), bytes)
            .is_ok()
    }

    fn resolve_recv_endpoint(
        &mut self,
        cap: CapId,
    ) -> Result<crate::kernel::capabilities::CapObject, SyscallError> {
        use crate::kernel::capabilities::CapRights;
        validate_endpoint_right(self.kernel, cap, CapRights::RECEIVE)?;
        let resolved = self
            .kernel
            .current_task_cnode()
            .and_then(|cnode| self.kernel.capability_for_cnode_local(cnode, cap))
            .and_then(|c| self.kernel.capability_object_live(c.object).map(|_| c));
        resolved
            .map(|c| c.object)
            .ok_or(SyscallError::InvalidCapability)
    }

    fn endpoint_index(
        &mut self,
        endpoint: crate::kernel::capabilities::CapObject,
    ) -> Result<usize, SyscallError> {
        self.kernel
            .resolve_endpoint_index(endpoint)
            .map_err(SyscallError::from)
    }

    fn peek_head(
        &mut self,
        endpoint_idx: usize,
    ) -> Result<Option<crate::kernel::ipc::Message>, SyscallError> {
        use crate::kernel::boot::IpcEndpointPeekResult;
        Ok(
            match self.kernel.peek_queued_with_cap_transfer(endpoint_idx) {
                IpcEndpointPeekResult::Peeked(msg) => Some(msg),
                IpcEndpointPeekResult::Ineligible(_) => None,
            },
        )
    }

    fn materialize_cap(
        &mut self,
        endpoint: crate::kernel::capabilities::CapObject,
        sender_tid: u64,
        msg: &crate::kernel::ipc::Message,
    ) -> Result<Option<u64>, SyscallError> {
        materialize_received_message_cap(self.kernel, endpoint, self.tid, sender_tid, msg)
    }

    fn object_meta(&mut self, cap: CapId) -> crate::kernel::syscall::recv_v3_txn::V3ObjectMeta {
        use crate::kernel::syscall::recv_v3_txn::V3ObjectMeta;
        let Some(capability) = self
            .kernel
            .capability_service()
            .resolve_current_task_capability(cap)
        else {
            return V3ObjectMeta::default();
        };
        V3ObjectMeta {
            kind: recv_v3_object_kind(capability.object),
            generation: recv_v3_object_generation(capability.object),
            effective_rights: u32::from(capability.rights_bits()),
            exact_object_size: recv_v3_exact_object_size(self.kernel, capability.object),
            exact_region_len: recv_v3_exact_region_len(capability.object),
        }
    }

    fn region_phys_start(&mut self, cap: CapId) -> Option<crate::kernel::vm::PhysAddr> {
        use crate::kernel::capabilities::CapObject;
        let (mo_id, offset) = self
            .kernel
            .capability_service()
            .resolve_current_task_capability(cap)
            .and_then(|c| match c.object {
                CapObject::DmaRegion { id, offset, .. } => Some((id, offset)),
                CapObject::MemoryObject { id } => Some((id, 0u64)),
                _ => None,
            })?;
        self.kernel.with_memory_state(|m| {
            m.memory_objects
                .iter()
                .flatten()
                .find(|e| e.id == mo_id)
                .map(|e| crate::kernel::vm::PhysAddr(e.phys.0 + offset))
        })
    }

    fn map_page(
        &mut self,
        asid: crate::kernel::vm::Asid,
        virt: crate::kernel::vm::VirtAddr,
        phys: crate::kernel::vm::PhysAddr,
        writable: bool,
    ) -> bool {
        use crate::kernel::vm::{CachePolicy, Mapping, PageFlags};
        self.kernel
            .map_user_page_in_asid_raw(
                asid,
                virt,
                Mapping {
                    phys,
                    flags: PageFlags {
                        read: true,
                        write: writable,
                        execute: false,
                        user: true,
                        cache_policy: CachePolicy::WriteBack,
                    },
                },
            )
            .is_ok()
    }

    fn unmap_range(&mut self, asid: crate::kernel::vm::Asid, base: usize, len: usize) {
        self.kernel.unmap_range_two_phase(asid, base, len);
    }

    fn register_transfer(
        &mut self,
        cap: CapId,
        base: crate::kernel::vm::VirtAddr,
        len: usize,
    ) -> bool {
        self.kernel
            .register_active_transfer_mapping(
                crate::kernel::ipc::ThreadId(self.tid),
                cap,
                base,
                len,
            )
            .is_ok()
    }

    fn remove_transfer(&mut self, cap: CapId) -> bool {
        self.kernel
            .remove_active_transfer_mapping(crate::kernel::ipc::ThreadId(self.tid), cap)
    }

    fn commit_peeked(
        &mut self,
        head: &crate::kernel::syscall::recv_v3_txn::PeekedHead,
    ) -> crate::kernel::syscall::recv_v3_txn::V3CommitOutcome {
        use crate::kernel::boot::IpcEndpointRecvResult;
        use crate::kernel::syscall::recv_v3_txn::V3CommitOutcome;
        match self
            .kernel
            .commit_peeked_recv_with_cap_transfer(head.endpoint_idx, &head.msg)
        {
            IpcEndpointRecvResult::Received(_) => V3CommitOutcome::Consumed { wake: None },
            IpcEndpointRecvResult::ReceivedWithSenderWake(_, wake) => {
                V3CommitOutcome::Consumed { wake: Some(wake) }
            }
            IpcEndpointRecvResult::Ineligible(_) => V3CommitOutcome::LostRace,
        }
    }

    fn settle_sender(&mut self, wake: crate::kernel::ipc::SenderWakeTarget) {
        let _ = self.kernel.apply_split_sender_wake_plan(wake);
        crate::yarm_log!(
            "IPC_RECV_V2_SENDER_WAKE_ORDER_OK wake_tid={} phase=before_writeback",
            wake.tid.0
        );
    }

    fn rollback_cap(&mut self, cap: CapId, is_reply: bool) {
        self.kernel
            .rollback_materialized_recv_cap(self.tid, cap, is_reply);
    }

    fn note(&mut self, event: crate::kernel::syscall::recv_v3_txn::V3TxnEvent) {
        note_v3_event("broad", event);
    }
}

/// U9-XFER2 §3 — the shared NR 30 marker text, so both routes emit the same lines and a reader
/// can still tell them apart on purpose.
pub(crate) fn note_v3_event(route: &str, event: crate::kernel::syscall::recv_v3_txn::V3TxnEvent) {
    use crate::kernel::syscall::recv_v3_txn::V3TxnEvent;
    match event {
        V3TxnEvent::Refused => crate::yarm_log!("RECV_V3_REFUSED route={}", route),
        V3TxnEvent::WouldBlock => crate::yarm_log!("RECV_V3_WOULD_BLOCK route={}", route),
        V3TxnEvent::DeliveredMapped {
            sender_tid,
            cleanup_token,
        } => crate::yarm_log!(
            "RECV_V3_LIVE_MAPPED route={} sender={} token={}",
            route,
            sender_tid,
            cleanup_token
        ),
        V3TxnEvent::DeliveredPlain {
            sender_tid,
            payload_len,
        } => crate::yarm_log!(
            "RECV_V3_LIVE route={} sender={} len={}",
            route,
            sender_tid,
            payload_len
        ),
        V3TxnEvent::LostRace => crate::yarm_log!("RECV_V3_COMMIT_LOST_RACE route={}", route),
        V3TxnEvent::WritebackFailedAfterCommit => {
            crate::yarm_log!("RECV_V3_WRITEBACK_FAIL_ROLLBACK route={}", route)
        }
    }
}
