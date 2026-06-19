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
fn parse_v3_request_bytes(
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
    use crate::kernel::recv_core::recv_shared_v3::{V3_MIN_OUTPUT_LEN, V3_VERSION};
    if out_ptr == 0 || out_len < V3_MIN_OUTPUT_LEN as u64 {
        return false;
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
    // Stage 47+48 object introspection fields.
    out[40..44].copy_from_slice(&object_kind.to_le_bytes());
    // out[44..48]: C-layout padding (already 0).
    out[48..56].copy_from_slice(&object_generation.to_le_bytes());
    out[56..60].copy_from_slice(&effective_rights.to_le_bytes());
    // out[60..64]: C-layout padding (already 0).
    // Stage 49: exact_object_size for MemoryObject; 0 for all other kinds.
    out[64..72].copy_from_slice(&exact_object_size.to_le_bytes());
    // out[72..80]: region_offset — FUTURE, always 0.
    // Stage 50: exact_region_len for DmaRegion; 0 for all other kinds.
    out[80..88].copy_from_slice(&exact_region_len.to_le_bytes());
    // Stage 58+59: live mapping output fields (0 when no mapping).
    out[88..96].copy_from_slice(&mapped_base.to_le_bytes());
    out[96..104].copy_from_slice(&page_rounded_mapped_len.to_le_bytes());
    out[104..108].copy_from_slice(&actual_mapping_perm.to_le_bytes());
    // out[108..112]: C-layout padding (already 0).
    out[112..120].copy_from_slice(&cleanup_token.to_le_bytes());
    let write_len = (out_len as usize).min(120);
    kernel
        .copy_to_current_user(out_ptr as usize, &out[..write_len])
        .is_ok()
}

/// Map a [`CapObject`] variant to its `RecvSharedV3ObjectKind` discriminant.
fn recv_v3_object_kind(obj: crate::kernel::capabilities::CapObject) -> u32 {
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
fn recv_v3_object_generation(obj: crate::kernel::capabilities::CapObject) -> u64 {
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
fn recv_v3_exact_region_len(obj: crate::kernel::capabilities::CapObject) -> u64 {
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
    use crate::kernel::recv_core::{
        RecvBlockingPolicy, RecvMapIntent, RecvMetaTarget, RecvOutcome, RecvPayloadTarget,
        RecvRequest, RecvRequestKind, RecvSchedulerWakePlan, RecvTransferPolicy,
        RecvUserWritebackOutcome, execute_user_asid_plain_writeback, try_recv_core_user_plain,
    };

    const V3_STATUS_OK: u32 = 0;
    const V3_STATUS_WOULD_BLOCK: u32 = 1;

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

    // Stage 42+43: blocking not implemented — full blocking requires
    // RecvAbiVariant::RecvSharedV3 in task.rs and wake-path changes.
    if req.timeout_ticks != 0 {
        return Err(SyscallError::WouldBlock);
    }

    // Stage 58+59: map_intent is now live for DmaRegion read-only.
    // When map_intent != 0 the caller must supply at least V3_LIVE_OUTPUT_LEN bytes
    // so mapped_base, page_rounded_mapped_len, actual_mapping_perm, and cleanup_token
    // can all be written.  Smaller buffers are rejected to prevent silent token loss.
    if req.map_intent != 0
        && req.metadata_len < crate::kernel::recv_core::recv_shared_v3::V3_LIVE_OUTPUT_LEN as u64
    {
        return Err(SyscallError::InvalidArgs);
    }

    // Stage 72: MAP_READ|MAP_WRITE (0x3) is permitted for the READ_SHARED_REPLY profile.
    // Rights enforcement: compute_recv_v3_mapping_plan checks CAP_RIGHT_MAP + CAP_RIGHT_WRITE;
    // InsufficientRights → rollback + InvalidArgs below.
    // NX: hardcoded (execute: false) in all recv_shared_v3 page mappings.
    // Cleanup: ActiveTransferMapping carries owner_tid+cap+base+len regardless of perm;
    // purge_active_transfer_mappings_for_pid cleans both read-only and read-write mappings.
    // WRITE-only (0x2) is already rejected: validate_v3_request above requires READ bit.

    let caller_tid = current_tid(kernel)?;
    let recv_cap = CapId(req.endpoint_cap);

    validate_endpoint_right(kernel, recv_cap, CapRights::RECEIVE)?;
    let endpoint_cap = kernel
        .current_task_cnode()
        .and_then(|cnode| kernel.capability_for_cnode_local(cnode, recv_cap))
        .and_then(|cap| kernel.capability_object_live(cap.object).map(|_| cap));
    let Some(ep_cap) = endpoint_cap else {
        return Err(SyscallError::InvalidCapability);
    };
    let endpoint = ep_cap.object;

    let request = RecvRequest {
        kind: RecvRequestKind::NonblockingProbe,
        requester_tid: caller_tid,
        recv_cap,
        payload_target: RecvPayloadTarget::UserMemory {
            ptr: req.payload_ptr as usize,
            len: req.payload_len as usize,
        },
        meta_target: RecvMetaTarget::None,
        blocking: RecvBlockingPolicy::NoWait,
        transfer: RecvTransferPolicy::LegacyFull,
        map_intent: RecvMapIntent::None,
    };

    crate::yarm_log!("RECV_V3_ENTER tid={} cap={}", caller_tid, recv_cap.0);
    let outcome = try_recv_core_user_plain(kernel, &request, endpoint);

    match outcome {
        RecvOutcome::WouldBlock | RecvOutcome::FallbackRequired(_) => {
            let _ = write_v3_output_to_user(
                kernel,
                req.metadata_ptr,
                req.metadata_len,
                V3_STATUS_WOULD_BLOCK,
                0,
                0,
                0,
                SYSCALL_NO_TRANSFER_CAP,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            );
            crate::yarm_log!("RECV_V3_WOULD_BLOCK tid={}", caller_tid);
            return Err(SyscallError::WouldBlock);
        }
        RecvOutcome::TimedOut => return Err(SyscallError::TimedOut),
        RecvOutcome::Error(e) => return Err(SyscallError::from(e)),
        RecvOutcome::Delivered(delivery) => {
            // Cap materialization BEFORE writeback — matches full-path §58 ordering.
            let is_reply_cap = (delivery.msg.flags & Message::FLAG_REPLY_CAP) != 0;
            let materialized_cap: Option<u64> = if let Some(_plan) = delivery.cap_transfer {
                match materialize_received_message_cap(
                    kernel,
                    endpoint,
                    caller_tid,
                    delivery.msg.sender_tid.0,
                    &delivery.msg,
                ) {
                    Ok(cap) => cap,
                    Err(e) => return Err(e),
                }
            } else {
                None
            };

            // Deferred sender wake BEFORE writeback — matches §58 ordering.
            if let RecvSchedulerWakePlan::WakeSender(wake_tid) = delivery.scheduler {
                let _ = kernel.apply_split_sender_wake_plan(wake_tid);
            }

            let payload_len = delivery.msg.as_slice().len();
            let sender_tid_raw = delivery.msg.sender_tid.0;
            let message_flags_raw = delivery.msg.flags as u32;
            let xfer_cap_out = materialized_cap.unwrap_or(SYSCALL_NO_TRANSFER_CAP);

            // Stage 47+48 + Stage 49 + Stage 50: resolve object metadata from the materialized cap.
            // Resolve capability first (borrows kernel briefly), then query size separately.
            let (obj_kind, obj_gen, eff_rights, exact_obj_size, exact_reg_len) =
                match materialized_cap {
                    Some(cap_id_raw) => {
                        let resolved = kernel
                            .capability_service()
                            .resolve_current_task_capability(CapId(cap_id_raw));
                        if let Some(cap) = resolved {
                            (
                                recv_v3_object_kind(cap.object),
                                recv_v3_object_generation(cap.object),
                                u32::from(cap.rights_bits()),
                                recv_v3_exact_object_size(kernel, cap.object),
                                recv_v3_exact_region_len(cap.object),
                            )
                        } else {
                            (0, 0, 0, 0, 0)
                        }
                    }
                    None => (0, 0, 0, 0, 0),
                };

            // Stage 58+59: live DmaRegion/MemoryObject read-only (or RW) mapping.
            // Order: materialize cap → metadata → map pages → register token → output.
            // On any failure: rollback mapped pages + cleanup slot + rollback cap.
            // Stage 60: 6th element (map_rollback) carries (Asid, CapId) for
            // post-writeback rollback if copy_to_current_user fails.
            let (
                mapped_base,
                mapped_len_out,
                actual_perm,
                cleanup_token,
                skip_payload,
                map_rollback,
            ) = if req.map_intent != 0 {
                use crate::kernel::capabilities::CapObject;
                use crate::kernel::recv_core::recv_shared_v3::{
                    MAP_PERM_READ_ONLY, MAP_PERM_READ_WRITE, RecvV3MappingPlan,
                    compute_recv_v3_mapping_plan,
                };

                let Some(cap_id_raw) = materialized_cap else {
                    // map_intent requires a cap-transfer message
                    return Err(SyscallError::InvalidArgs);
                };
                let cap_id = CapId(cap_id_raw);

                // Use eff_rights (already resolved above) for plan computation.
                let plan = compute_recv_v3_mapping_plan(
                    delivery.msg.opcode,
                    req.map_intent,
                    req.payload_ptr,
                    req.payload_len,
                    eff_rights as u8,
                    exact_reg_len,
                    PAGE_SIZE as u64,
                );

                match plan {
                    RecvV3MappingPlan::Map {
                        map_va,
                        mapped_len,
                        read_only,
                    } => {
                        // Resolve physical start: mo.phys + dma.offset.
                        // Separate cap lookup (Copy) and memory lookup (immutable borrow).
                        let dma_fields = kernel
                            .capability_service()
                            .resolve_current_task_capability(cap_id)
                            .and_then(|cap| match cap.object {
                                CapObject::DmaRegion { id, offset, .. } => Some((id, offset)),
                                CapObject::MemoryObject { id } => Some((id, 0u64)),
                                _ => None,
                            });
                        let phys_start = dma_fields.and_then(|(mo_id, dma_offset)| {
                            kernel.with_memory_state(|m| {
                                m.memory_objects
                                    .iter()
                                    .flatten()
                                    .find(|e| e.id == mo_id)
                                    .map(|e| PhysAddr(e.phys.0 + dma_offset))
                            })
                        });
                        let phys_start = match phys_start {
                            Some(p) => p,
                            None => {
                                kernel.rollback_materialized_recv_cap(
                                    caller_tid,
                                    cap_id,
                                    is_reply_cap,
                                );
                                return Err(SyscallError::InvalidArgs);
                            }
                        };

                        let receiver_asid = match kernel.task_asid(caller_tid) {
                            Some(a) => a,
                            None => {
                                kernel.rollback_materialized_recv_cap(
                                    caller_tid,
                                    cap_id,
                                    is_reply_cap,
                                );
                                return Err(SyscallError::InvalidArgs);
                            }
                        };

                        let map_flags = PageFlags {
                            read: true,
                            write: !read_only,
                            execute: false,
                            user: true,
                            cache_policy: CachePolicy::WriteBack,
                        };
                        let num_pages = (mapped_len / PAGE_SIZE as u64) as usize;
                        for page_idx in 0..num_pages {
                            let virt = VirtAddr(map_va + page_idx as u64 * PAGE_SIZE as u64);
                            let phys = PhysAddr(phys_start.0 + page_idx as u64 * PAGE_SIZE as u64);
                            if kernel
                                .map_user_page_in_asid_raw(
                                    receiver_asid,
                                    virt,
                                    Mapping {
                                        phys,
                                        flags: map_flags,
                                    },
                                )
                                .is_err()
                            {
                                let rollback_len = page_idx * PAGE_SIZE;
                                if rollback_len > 0 {
                                    kernel.unmap_range_two_phase(
                                        receiver_asid,
                                        map_va as usize,
                                        rollback_len,
                                    );
                                }
                                kernel.rollback_materialized_recv_cap(
                                    caller_tid,
                                    cap_id,
                                    is_reply_cap,
                                );
                                return Err(SyscallError::InvalidArgs);
                            }
                        }

                        if kernel
                            .register_active_transfer_mapping(
                                crate::kernel::ipc::ThreadId(caller_tid),
                                cap_id,
                                VirtAddr(map_va),
                                mapped_len as usize,
                            )
                            .is_err()
                        {
                            kernel.unmap_range_two_phase(
                                receiver_asid,
                                map_va as usize,
                                mapped_len as usize,
                            );
                            kernel.rollback_materialized_recv_cap(caller_tid, cap_id, is_reply_cap);
                            return Err(SyscallError::InvalidArgs);
                        }

                        crate::yarm_log!(
                            "RECV_V3_MAPPED tid={} va=0x{:x} len={} ro={}",
                            caller_tid,
                            map_va,
                            mapped_len,
                            read_only
                        );
                        let perm = if read_only {
                            MAP_PERM_READ_ONLY
                        } else {
                            MAP_PERM_READ_WRITE
                        };
                        // cleanup_token = xfer_cap_out (full CapId.0, encodes slot+generation).
                        // Stage 60: stale tokens are generation-safe because CapId encodes
                        // generation in bits[63:16]; a revoked-then-reused slot has a
                        // different CapId and will not match the stored active mapping entry.
                        (
                            map_va,
                            mapped_len,
                            perm,
                            xfer_cap_out,
                            true,
                            Some((receiver_asid, cap_id)),
                        )
                    }
                    RecvV3MappingPlan::Skip => {
                        // map_intent != 0 but received message is not OPCODE_SHARED_MEM.
                        kernel.rollback_materialized_recv_cap(caller_tid, cap_id, is_reply_cap);
                        return Err(SyscallError::InvalidArgs);
                    }
                    RecvV3MappingPlan::InvalidRegion | RecvV3MappingPlan::InsufficientRights => {
                        kernel.rollback_materialized_recv_cap(caller_tid, cap_id, is_reply_cap);
                        return Err(SyscallError::InvalidArgs);
                    }
                }
            } else {
                (0u64, 0u64, 0u32, 0u64, false, None)
            };

            if skip_payload {
                // Mapping done: payload_ptr is the mapping target VA, not an inline
                // payload buffer. Skip copy. All info is in v3 metadata output.
                // Stage 60: if metadata writeback fails the caller never receives the
                // cleanup_token, so it cannot call TransferRelease. Roll back the mapping,
                // remove the registry entry, and revoke the materialized cap so no resources
                // leak.
                let wrote_ok = write_v3_output_to_user(
                    kernel,
                    req.metadata_ptr,
                    req.metadata_len,
                    V3_STATUS_OK,
                    sender_tid_raw,
                    0,
                    message_flags_raw,
                    xfer_cap_out,
                    obj_kind,
                    obj_gen,
                    eff_rights,
                    exact_obj_size,
                    exact_reg_len,
                    mapped_base,
                    mapped_len_out,
                    actual_perm,
                    cleanup_token,
                );
                if !wrote_ok {
                    if let Some((rb_asid, rb_cap)) = map_rollback {
                        kernel.unmap_range_two_phase(
                            rb_asid,
                            mapped_base as usize,
                            mapped_len_out as usize,
                        );
                        kernel.remove_active_transfer_mapping(
                            crate::kernel::ipc::ThreadId(caller_tid),
                            rb_cap,
                        );
                        kernel.rollback_materialized_recv_cap(caller_tid, rb_cap, is_reply_cap);
                    }
                    crate::yarm_log!(
                        "RECV_V3_WRITEBACK_FAIL_ROLLBACK tid={} cap={}",
                        caller_tid,
                        xfer_cap_out
                    );
                    return Err(SyscallError::InvalidArgs);
                }
                frame.set_ok(
                    usize::try_from(sender_tid_raw).unwrap_or(0),
                    0,
                    usize::try_from(xfer_cap_out).unwrap_or(usize::MAX),
                );
                crate::yarm_log!(
                    "RECV_V3_LIVE_MAPPED tid={} sender={}",
                    caller_tid,
                    sender_tid_raw
                );
                return Ok(());
            }

            match execute_user_asid_plain_writeback(kernel, &delivery) {
                RecvUserWritebackOutcome::Ok => {
                    let _ = write_v3_output_to_user(
                        kernel,
                        req.metadata_ptr,
                        req.metadata_len,
                        V3_STATUS_OK,
                        sender_tid_raw,
                        payload_len as u32,
                        message_flags_raw,
                        xfer_cap_out,
                        obj_kind,
                        obj_gen,
                        eff_rights,
                        exact_obj_size,
                        exact_reg_len,
                        0,
                        0,
                        0,
                        0,
                    );
                    frame.set_ok(
                        usize::try_from(sender_tid_raw).unwrap_or(0),
                        payload_len,
                        usize::try_from(xfer_cap_out).unwrap_or(usize::MAX),
                    );
                    crate::yarm_log!("RECV_V3_LIVE tid={} sender={}", caller_tid, sender_tid_raw);
                }
                RecvUserWritebackOutcome::UndersizedBuffer => {
                    // Rollback cap — buffer too small, message consumed, §58.
                    if let Some(cap_id) = materialized_cap {
                        kernel.rollback_materialized_recv_cap(
                            caller_tid,
                            CapId(cap_id),
                            is_reply_cap,
                        );
                    }
                    return Err(SyscallError::InvalidArgs);
                }
                RecvUserWritebackOutcome::CopyFault { user_ptr } => {
                    // No rollback on payload copy fault — message consumed, §58.
                    record_user_fault(kernel, frame, user_ptr, FaultAccess::Write);
                    return Ok(());
                }
            }
            Ok(())
        }
    }
}
