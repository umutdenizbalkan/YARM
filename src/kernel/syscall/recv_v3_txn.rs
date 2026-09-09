// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-XFER2 §3 — THE `RecvSharedV3` transaction (NR 30), stated once for both routes.
//!
//! # What this is not
//!
//! It is not a reordering of NR 30. U9-XFER1 already moved the dequeue and the sender wake behind
//! everything that can fail, and that order is preserved verbatim here. What this adds is the
//! *acquisition boundary*: every owner the handler needs is named, so the same body runs under one
//! broad borrow or under per-domain seams, and neither route can drift from the other.
//!
//! # Ownership across the phase boundaries
//!
//! A successful peek reserves NOTHING. `Endpoint::peek` returns a copy of the head message and
//! releases rank 3; the queue is free to change immediately afterwards. So the transaction owns
//! exactly what it has itself created, and nothing else:
//!
//! | phase | what the transaction owns afterwards | undo |
//! |---|---|---|
//! | K peek | nothing — the message is still the sender's | none needed |
//! | M materialize | the minted capability, and the transfer envelope it consumed | `rollback_cap` |
//! | P map | the pages it installed, and only those | `unmap_range` over its own base/len |
//! | G register | the active-transfer registry entry it added | `remove_registration` |
//! | C commit | the message — but only if the head was still the one it peeked | none: this is the linearization point |
//! | O writeback | user memory, which cannot be un-written | none |
//!
//! **Exactly-once sender settlement.** The wake target is produced by the COMMIT, never by the
//! peek: a peek cannot yield one (the policy asserts this by construction — its peek result has no
//! wake channel), so a sender can only be settled by the acquisition that actually consumed its
//! message. Compensation paths never wake, because they never consumed.
//!
//! # The one branch that consumes and can then fail
//!
//! Step O. User memory cannot be un-written, so a writeback failure after the commit cannot put
//! the message back. It is the LAST step for exactly that reason, and it compensates everything
//! the transaction still owns — mapping, registration, capability — before returning.

use crate::kernel::capabilities::{CapId, CapObject};
use crate::kernel::ipc::{Message, SenderWakeTarget};
use crate::kernel::syscall::SyscallError;
use crate::kernel::vm::{Asid, PhysAddr, VirtAddr};

/// What the endpoint's head looked like when this transaction planned around it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PeekedHead {
    pub(crate) endpoint_idx: usize,
    pub(crate) msg: Message,
}

/// The outcome of the identity-checked consume.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum V3CommitOutcome {
    /// The peeked message was consumed. `wake` is the sender to settle, if the dequeue refilled
    /// from a blocked sender — produced HERE and nowhere else, so settlement is exactly once.
    Consumed { wake: Option<SenderWakeTarget> },
    /// The head was no longer the peeked message: another receiver took it. Nothing was consumed.
    LostRace,
}

/// Object metadata the output record carries. Pure data, resolved from reads.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct V3ObjectMeta {
    pub(crate) kind: u32,
    pub(crate) generation: u64,
    pub(crate) effective_rights: u32,
    pub(crate) exact_object_size: u64,
    pub(crate) exact_region_len: u64,
}

/// The owners NR 30 needs. One method per acquisition or pure read.
///
/// The caller's ASID is passed EXPLICITLY wherever user memory is touched, rather than resolved
/// from an ambient "current task" lookup: NR 30 wakes a sender partway through, so "current" is
/// not a safe question to ask after the commit, and the split route has no ambient borrow to ask
/// it of in the first place.
pub(crate) trait RecvV3Owners {
    fn caller_tid(&mut self) -> u64;
    fn caller_asid(&mut self) -> Option<Asid>;

    /// rank 5 — read the request record out of the caller's address space.
    fn read_user(&mut self, asid: Asid, ptr: usize, len: usize) -> Option<[u8; 80]>;

    /// rank 5 — write the encoded output record into the caller's address space.
    fn write_user(&mut self, asid: Asid, ptr: usize, bytes: &[u8]) -> bool;

    /// rank 4 (+3) — resolve the receive endpoint and prove the RECEIVE right, exactly as the
    /// broad `validate_endpoint_right` + liveness pair does.
    fn resolve_recv_endpoint(&mut self, cap: CapId) -> Result<CapObject, SyscallError>;

    /// rank 3 — the endpoint's index, or the error the broad resolver produces.
    fn endpoint_index(&mut self, endpoint: CapObject) -> Result<usize, SyscallError>;

    /// rank 3 — NON-CONSUMING head read. `Ok(None)` is "nothing available", which the ABI already
    /// reports as `WouldBlock`.
    fn peek_head(&mut self, endpoint_idx: usize) -> Result<Option<Message>, SyscallError>;

    /// rank 4 (+6) — mint the transferred capability from the PEEKED descriptor.
    fn materialize_cap(
        &mut self,
        endpoint: CapObject,
        sender_tid: u64,
        msg: &Message,
    ) -> Result<Option<u64>, SyscallError>;

    /// rank 4 / rank 6 reads — the output record's object fields.
    fn object_meta(&mut self, cap: CapId) -> V3ObjectMeta;

    /// rank 4 / rank 6 reads — the region's physical base (`mo.phys + dma.offset`).
    fn region_phys_start(&mut self, cap: CapId) -> Option<PhysAddr>;

    /// rank 5 — install one page. `false` is a refusal the caller compensates.
    fn map_page(&mut self, asid: Asid, virt: VirtAddr, phys: PhysAddr, writable: bool) -> bool;

    /// rank 5 → TLB → rank 6 — remove a range this transaction installed.
    fn unmap_range(&mut self, asid: Asid, base: usize, len: usize);

    /// rank 3 — register the active-transfer entry.
    fn register_transfer(&mut self, cap: CapId, base: VirtAddr, len: usize) -> bool;

    /// rank 3 — remove the entry this transaction registered.
    fn remove_transfer(&mut self, cap: CapId) -> bool;

    /// ONE rank-3 acquisition — consume the peeked message if the head is still it, and yield the
    /// sender to settle. The ONLY producer of a wake target.
    fn commit_peeked(&mut self, head: &PeekedHead) -> V3CommitOutcome;

    /// rank 2 → rank 1 — settle the sender the commit named.
    fn settle_sender(&mut self, wake: SenderWakeTarget);

    /// rank 4 (+3, +6) — undo a materialized capability this transaction minted.
    fn rollback_cap(&mut self, cap: CapId, is_reply: bool);

    /// Observability.
    fn note(&mut self, event: V3TxnEvent);
}

/// What the transaction did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum V3TxnEvent {
    Refused,
    WouldBlock,
    /// Delivered with a live mapping; `cleanup_token` is what NR 4 will be handed.
    DeliveredMapped {
        sender_tid: u64,
        cleanup_token: u64,
    },
    /// Delivered as a plain payload copy.
    DeliveredPlain {
        sender_tid: u64,
        payload_len: usize,
    },
    /// The head changed between the peek and the commit. Nothing consumed, everything this
    /// transaction built compensated.
    LostRace,
    /// The commit consumed the message and the metadata writeback then failed. Compensated as far
    /// as it is possible to compensate; the message cannot be put back.
    WritebackFailedAfterCommit,
}

/// What the transaction delivered, for the adapter to encode into the caller's frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum V3Delivery {
    /// A live mapping was installed; the payload copy is skipped because `payload_ptr` was the
    /// mapping target. `ret1` is 0, exactly as before.
    Mapped { sender_tid: u64, xfer_cap: u64 },
    /// An ordinary payload copy.
    Plain {
        sender_tid: u64,
        payload_len: usize,
        xfer_cap: u64,
    },
    /// The payload copy faulted. The message IS consumed, matching §58 semantics: the adapter
    /// records the fault against `user_ptr` and returns `Ok(())` without setting a result.
    PayloadFault { user_ptr: usize },
}

const V3_STATUS_OK: u32 = 0;
const V3_STATUS_WOULD_BLOCK: u32 = 1;

/// THE `RecvSharedV3` transaction.
///
/// `req` is the already-validated request record; parsing and validating it is the adapter's job
/// because it precedes every acquisition and differs only in which read seam supplies the bytes.
#[allow(clippy::too_many_lines)]
pub(crate) fn run_recv_v3_transaction<O: RecvV3Owners>(
    owners: &mut O,
    req: &crate::kernel::recv_core::recv_shared_v3::RecvSharedV3Request,
) -> Result<V3Delivery, SyscallError> {
    use crate::kernel::recv_core::recv_shared_v3::{
        MAP_PERM_READ_ONLY, MAP_PERM_READ_WRITE, RecvV3MappingPlan, compute_recv_v3_mapping_plan,
    };
    use crate::kernel::vm::PAGE_SIZE;

    let caller_tid = owners.caller_tid();
    let asid = owners.caller_asid().ok_or(SyscallError::InvalidArgs)?;
    let recv_cap = CapId(req.endpoint_cap);

    // V — authority. Unchanged: RECEIVE right, then a live object.
    let endpoint = owners.resolve_recv_endpoint(recv_cap)?;
    let endpoint_idx = owners.endpoint_index(endpoint)?;

    // K — PEEK. Nothing is consumed and no sender is settled, so everything below that can fail
    // does so while the message is still the sender's.
    let Some(msg) = owners.peek_head(endpoint_idx)? else {
        // The ABI's existing empty-queue answer, with the record written best-effort exactly as
        // before. Nothing was consumed, so nothing is owed.
        let _ = write_output(owners, asid, req, V3_STATUS_WOULD_BLOCK, 0, 0, 0, 0, None);
        owners.note(V3TxnEvent::WouldBlock);
        return Err(SyscallError::WouldBlock);
    };
    let head = PeekedHead { endpoint_idx, msg };
    let sender_tid_raw = msg.sender_tid.0;
    let message_flags_raw = msg.flags as u32;
    let is_reply_cap = (msg.flags & Message::FLAG_REPLY_CAP) != 0;

    // M — mint the transferred capability from the PEEKED descriptor. Owned from here.
    let has_transfer = crate::kernel::recv_core::extract_cap_transfer_plan(&msg).is_some();
    let materialized_cap: Option<u64> = if has_transfer {
        // A mint refusal is a terminal exit like any other, so it is NOTED like any other.
        // Nothing is compensated here: the peek consumed nothing, so the message is still the
        // sender's, and the mint owner has already settled whatever it consumed of the envelope.
        match owners.materialize_cap(endpoint, sender_tid_raw, &msg) {
            Ok(v) => v,
            Err(e) => {
                owners.note(V3TxnEvent::Refused);
                return Err(e);
            }
        }
    } else {
        None
    };
    let xfer_cap_out = materialized_cap.unwrap_or(crate::kernel::syscall::SYSCALL_NO_TRANSFER_CAP);

    // Md — the output record's object fields, from reads.
    let meta = match materialized_cap {
        Some(raw) => owners.object_meta(CapId(raw)),
        None => V3ObjectMeta::default(),
    };

    // P/G — the live mapping, when the caller asked for one. Every refusal below undoes the mint.
    let mut mapped: Option<(VirtAddr, u64, u32, CapId)> = None;
    if req.map_intent != 0 {
        let Some(cap_raw) = materialized_cap else {
            rollback_owned(owners, None, None, is_reply_cap);
            owners.note(V3TxnEvent::Refused);
            return Err(SyscallError::InvalidArgs);
        };
        let cap_id = CapId(cap_raw);
        let plan = compute_recv_v3_mapping_plan(
            msg.opcode,
            req.map_intent,
            req.payload_ptr,
            req.payload_len,
            meta.effective_rights as u8,
            meta.exact_region_len,
            PAGE_SIZE as u64,
        );
        let RecvV3MappingPlan::Map {
            map_va,
            mapped_len,
            read_only,
        } = plan
        else {
            rollback_owned(owners, None, Some((cap_id, is_reply_cap)), is_reply_cap);
            owners.note(V3TxnEvent::Refused);
            return Err(SyscallError::InvalidArgs);
        };
        let Some(phys_start) = owners.region_phys_start(cap_id) else {
            rollback_owned(owners, None, Some((cap_id, is_reply_cap)), is_reply_cap);
            owners.note(V3TxnEvent::Refused);
            return Err(SyscallError::InvalidArgs);
        };
        let num_pages = (mapped_len / PAGE_SIZE as u64) as usize;
        for page_idx in 0..num_pages {
            let virt = VirtAddr(map_va + page_idx as u64 * PAGE_SIZE as u64);
            let phys = PhysAddr(phys_start.0 + page_idx as u64 * PAGE_SIZE as u64);
            if !owners.map_page(asid, virt, phys, !read_only) {
                // Undo exactly the prefix this transaction installed — no more.
                let installed = page_idx * PAGE_SIZE;
                if installed > 0 {
                    owners.unmap_range(asid, map_va as usize, installed);
                }
                rollback_owned(owners, None, Some((cap_id, is_reply_cap)), is_reply_cap);
                owners.note(V3TxnEvent::Refused);
                return Err(SyscallError::InvalidArgs);
            }
        }
        if !owners.register_transfer(cap_id, VirtAddr(map_va), mapped_len as usize) {
            owners.unmap_range(asid, map_va as usize, mapped_len as usize);
            rollback_owned(owners, None, Some((cap_id, is_reply_cap)), is_reply_cap);
            owners.note(V3TxnEvent::Refused);
            return Err(SyscallError::InvalidArgs);
        }
        let perm = if read_only {
            MAP_PERM_READ_ONLY
        } else {
            MAP_PERM_READ_WRITE
        };
        mapped = Some((VirtAddr(map_va), mapped_len, perm, cap_id));
    }

    // C — COMMIT. The linearization point: consume the peeked message, or nothing.
    let owned_mapping = mapped.map(|(base, len, _, cap)| (base, len as usize, cap));
    let wake = match owners.commit_peeked(&head) {
        V3CommitOutcome::Consumed { wake } => wake,
        V3CommitOutcome::LostRace => {
            rollback_owned(
                owners,
                owned_mapping.map(|(base, len, cap)| (asid, base, len, cap)),
                materialized_cap.map(|raw| (CapId(raw), is_reply_cap)),
                is_reply_cap,
            );
            owners.note(V3TxnEvent::LostRace);
            return Err(SyscallError::WouldBlock);
        }
    };
    // The message is consumed, so the sender's send succeeded — settled exactly once, by the
    // acquisition that consumed it.
    if let Some(wake) = wake {
        owners.settle_sender(wake);
    }

    // O — the output record, then the payload when there is no mapping.
    if let Some((base, len, perm, _)) = mapped {
        let wrote = write_output(
            owners,
            asid,
            req,
            V3_STATUS_OK,
            sender_tid_raw,
            0,
            message_flags_raw,
            xfer_cap_out,
            Some((meta, base.0, len, perm, xfer_cap_out)),
        );
        if !wrote {
            // The caller never receives the cleanup token, so it can never call NR 4 for this
            // region. Compensate everything still owned; the message cannot be put back.
            rollback_owned(
                owners,
                owned_mapping.map(|(b, l, c)| (asid, b, l, c)),
                materialized_cap.map(|raw| (CapId(raw), is_reply_cap)),
                is_reply_cap,
            );
            owners.note(V3TxnEvent::WritebackFailedAfterCommit);
            return Err(SyscallError::InvalidArgs);
        }
        owners.note(V3TxnEvent::DeliveredMapped {
            sender_tid: sender_tid_raw,
            cleanup_token: xfer_cap_out,
        });
        return Ok(V3Delivery::Mapped {
            sender_tid: sender_tid_raw,
            xfer_cap: xfer_cap_out,
        });
    }

    let payload = msg.as_slice();
    if (req.payload_len as usize) < payload.len() {
        // Undersized buffer: message consumed, cap rolled back — §58 semantics, unchanged.
        rollback_owned(
            owners,
            None,
            materialized_cap.map(|raw| (CapId(raw), is_reply_cap)),
            is_reply_cap,
        );
        owners.note(V3TxnEvent::Refused);
        return Err(SyscallError::InvalidArgs);
    }
    if !owners.write_user(asid, req.payload_ptr as usize, payload) {
        // Copy fault: message consumed, NO rollback — §58 semantics, unchanged.
        owners.note(V3TxnEvent::Refused);
        return Ok(V3Delivery::PayloadFault {
            user_ptr: req.payload_ptr as usize,
        });
    }
    let _ = write_output(
        owners,
        asid,
        req,
        V3_STATUS_OK,
        sender_tid_raw,
        payload.len() as u32,
        message_flags_raw,
        xfer_cap_out,
        Some((meta, 0, 0, 0, 0)),
    );
    owners.note(V3TxnEvent::DeliveredPlain {
        sender_tid: sender_tid_raw,
        payload_len: payload.len(),
    });
    Ok(V3Delivery::Plain {
        sender_tid: sender_tid_raw,
        payload_len: payload.len(),
        xfer_cap: xfer_cap_out,
    })
}

/// Undo exactly what this transaction owns, in reverse order of construction.
///
/// It never touches anything it did not create: not the sender's message, not a mapping some
/// other transaction installed, not a registry entry it did not add.
fn rollback_owned<O: RecvV3Owners>(
    owners: &mut O,
    mapping: Option<(Asid, VirtAddr, usize, CapId)>,
    cap: Option<(CapId, bool)>,
    is_reply: bool,
) {
    if let Some((asid, base, len, reg_cap)) = mapping {
        owners.unmap_range(asid, base.0 as usize, len);
        owners.remove_transfer(reg_cap);
    }
    if let Some((cap_id, _)) = cap {
        owners.rollback_cap(cap_id, is_reply);
    }
}

/// Encode the output record and write it through the caller's ASID.
#[allow(clippy::too_many_arguments)]
fn write_output<O: RecvV3Owners>(
    owners: &mut O,
    asid: Asid,
    req: &crate::kernel::recv_core::recv_shared_v3::RecvSharedV3Request,
    status: u32,
    sender_tid: u64,
    message_len: u32,
    message_flags: u32,
    transferred_cap: u64,
    live: Option<(V3ObjectMeta, u64, u64, u32, u64)>,
) -> bool {
    if req.metadata_ptr == 0 {
        return false;
    }
    let (meta, base, len, perm, token) = live.unwrap_or((V3ObjectMeta::default(), 0, 0, 0, 0));
    let Some((out, write_len)) = crate::kernel::syscall::recv_shared_v3::encode_v3_output(
        req.metadata_len,
        status,
        sender_tid,
        message_len,
        message_flags,
        transferred_cap,
        meta.kind,
        meta.generation,
        meta.effective_rights,
        meta.exact_object_size,
        meta.exact_region_len,
        base,
        len,
        perm,
        token,
    ) else {
        return false;
    };
    owners.write_user(asid, req.metadata_ptr as usize, &out[..write_len])
}
