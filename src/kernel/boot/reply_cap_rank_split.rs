// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 188D — reply-cap rank-inversion seam (IPC rank-3 record bridges).
//!
//! The reply-cap materialization has, since Stage 105 (D5), been the persistent
//! `reply_cap_ipc_rank_inversion` blocker for the dispatch-return channel: after
//! the rank-4 cnode mint, the receiver-local reply CapId must be recorded back
//! into the reply-cap registry, which lives under IPC `ipc_state_lock` (rank 3) —
//! *below* the capability rank. Under the global lock the whole sequence sits in
//! one critical section; a seam split makes the mint→record window real.
//!
//! Stage 188D closes this with **phase separation**, not a nested acquisition:
//!
//! - **Phase B (rank 4 + rank 6):** mint the receiver-local reply cap through the
//!   existing Stage 186D-proper seam
//!   ([`crate::runtime::SharedKernel::mint_capability_with_memory_ref_split`]). A
//!   `Reply` object carries no memory refcount, so this is rank-4-only. NO IPC
//!   lock is held.
//! - **Phase C (rank 3):** record the receiver-local CapId into the reply-cap
//!   registry through [`SharedKernel::try_record_reply_waiter_cap_split`] below,
//!   which acquires ONLY `ipc_state_lock` via `with_ipc_split_mut`. If the record
//!   is stale (the reply object was revoked/reused in the mint→record window),
//!   the caller rolls the mint back
//!   ([`crate::runtime::SharedKernel::rollback_minted_cap_split`]).
//!
//! The two critical sections are **disjoint** — the rank-4 mint fully releases
//! its lock before the rank-3 record acquires `ipc_state_lock`. This helper
//! therefore never holds the capability lock while taking the IPC lock (no rank
//! inversion), never mints a cap under `ipc_state_lock`, and never touches a
//! broad `&mut KernelState`. It is the exact rank-3 half of the D5 split
//! (`try_set_reply_cap_waiter_cap` / `clear_reply_cap_waiter_cap`), re-expressed
//! on the `SharedKernel` IPC seam so the dispatch-return executor can run it
//! after the broad borrow drops.
//!
//! # Scope / status
//!
//! Solves the rank inversion at the **seam** level. The one live
//! reply-cap→blocked-waiter delivery path in a real boot is `ipc_call` to a
//! blocked server; wiring a live producer there is **out of Stage 188D scope**
//! (the broader `ipc_send`/`ipc_call` conversion is deliberately untouched).
//! The producer is wired into the sanctioned dispatch-return site (`ipc_reply`);
//! reply-with-reply-cap does not occur in the current boot, so the seam is
//! exercised end-to-end by unit tests, not by boot traffic. See
//! `doc/KERNEL_UNLOCKING.md` (Stage 188D).

use super::*;

impl crate::runtime::SharedKernel {
    /// Stage 188D — Phase C (rank 3, IPC only): record the receiver-local reply
    /// CapId into the reply-cap registry, with the same generation/liveness
    /// stale-detection as [`super::KernelState::try_set_reply_cap_waiter_cap`],
    /// but through the `with_ipc_split_mut` seam so no broad `&mut KernelState`
    /// borrow is live. Acquires ONLY `ipc_state_lock`.
    ///
    /// A non-[`ReplyRecordSetOutcome::Set`] outcome means the reply object was
    /// revoked/reused between the rank-4 mint and this record — the caller MUST
    /// roll the mint back (see module docs). This function performs NO mint and
    /// NO rollback itself; it is a pure rank-3 record.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn try_record_reply_waiter_cap_split(
        &self,
        reply_index: usize,
        reply_generation: u64,
        cap: CapId,
    ) -> ReplyRecordSetOutcome {
        self.with_ipc_split_mut(|ipc| {
            if reply_index >= super::MAX_REPLY_CAPS {
                return ReplyRecordSetOutcome::IndexOutOfRange;
            }
            if ipc.reply_cap_generations[reply_index] != reply_generation {
                return ReplyRecordSetOutcome::GenerationMismatch;
            }
            if let Some(record) = &mut ipc.reply_caps[reply_index] {
                record.waiter_cap_id = Some(cap);
                crate::yarm_log!(
                    "IPC_RECV_REPLY_CAP_WAITER_CAP_SET reply_index={} reply_gen={} cap={}",
                    reply_index,
                    reply_generation,
                    cap.0
                );
                ReplyRecordSetOutcome::Set
            } else {
                ReplyRecordSetOutcome::SlotEmpty
            }
        })
    }

    /// Stage 188D — rank-3 clear of a previously-recorded waiter CapId, the seam
    /// sibling of [`super::KernelState::clear_reply_cap_waiter_cap`]. Called on a
    /// post-record rollback (a user-copy fault after the record was set): the
    /// receiver-cnode slot is being revoked, so the registry must no longer
    /// reference it. Generation-guarded; the `ReplyCapRecord` itself stays live
    /// and re-deliverable. Acquires ONLY `ipc_state_lock`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn clear_reply_waiter_cap_split(&self, reply_index: usize, reply_generation: u64) {
        self.with_ipc_split_mut(|ipc| {
            if reply_index >= super::MAX_REPLY_CAPS {
                return;
            }
            if ipc.reply_cap_generations[reply_index] != reply_generation {
                return;
            }
            if let Some(record) = &mut ipc.reply_caps[reply_index] {
                record.waiter_cap_id = None;
            }
        });
    }
}
