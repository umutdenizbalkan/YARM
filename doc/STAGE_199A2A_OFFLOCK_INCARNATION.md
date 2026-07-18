# Stage 199A2A — Off-Lock IpcCall/IpcReply Boundaries and Incarnation-Safe Reply Records

Hosted-only increment. Covers the two retirement candidates **IpcCallDirectRequest**
(NR 6) and **IpcReplyDirect** (NR 7). No QEMU, no userspace oracle, no boot-cap /
artifact-marker changes, no new live selectors — the live markers and oracle
selectors stay disabled (Part 8).

This document is deliberately explicit about **what was genuinely achieved and
verified in hosted** versus **what remains deferred**, and grounds every deferral
in the source. It does **not** claim a green off-lock seal.

---

## 1. What this increment lands (achieved, hosted-verified)

### Part 1 — Incarnation-safe `ReplyCapRecord` (`numeric_tid_only_authority=0`)

`ReplyCapRecord` (`src/kernel/boot/defs.rs`) now carries the **complete
generation-bearing identity** of both parties, not a bare numeric `ThreadId`:

```
pub(crate) caller_tid:   ThreadId,
pub(crate) caller_asid:  Asid,          // caller incarnation discriminator
pub(crate) responder_tid: Option<ThreadId>,
pub(crate) replier_asid: Option<Asid>,  // bound-responder incarnation discriminator
```

* `create_reply_cap_for_caller_in_cnode` captures `caller_asid = task_asid(caller)`
  and `replier_asid = responder_tid.and_then(task_asid)` **before** the mutable
  ipc-state borrow, and stores them in the record.
* `ipc_reply` authorizes a reply only when the **current replier's `{tid, asid}`**
  matches the bound identity. A numeric replier TID reused by a replacement task
  (different ASID) is rejected with `MissingRight` — **numeric TID alone never
  authorizes a reply delivery/wake.** The rejection is logged
  `IPC_REPLY_INCARNATION_REJECT …` and consumes nothing (the record survives).
* `revoke_reply_caps_for_caller` / `revoke_reply_caps_for_replier` resolve the
  current incarnation's ASID before the mutable borrow and clear a record only when
  the complete `{tid, asid}` identity matches. A numeric-TID sweep can never clear a
  **replacement** task's record. When the current ASID is unresolvable (kernel task /
  partial teardown) they fall back to the numeric-TID match — the safe, never-leak
  direction for a one-shot record.

**Leak-freedom of the cleanup change:** every production revoke call site
(`exit_task`, `restart_task`, `mark_task_dead`, `reap_faulted_task_noalloc_cleanup`
in `src/kernel/boot/restart_state.rs`) runs while the task's TCB is still present
with its **original** ASID, so `asid_now` equals the stored ASID and cleanup remains
leak-free. `restart_task` reuses the same TCB/ASID (it is not a new incarnation from
the ASID's perspective), so restart cleanup also matches.

### Part 2 — NR 6 request copy-before-reserve (leak-free abort)

`handle_ipc_call` (`src/kernel/syscall/ipc.rs`) now copies the request payload into
an **owned** `[u8; Message::MAX_PAYLOAD]` buffer, and validates `len`, **before**
reserving the global `ReplyCapRecord` and minting the caller Reply cap.

Previously the reply cap was created first and a user-copy fault (or an oversized
`len`) returned **without** freeing the reserved record and the minted caller cnode
slot — leaking one global `ReplyCapRecord` and one caller cnode cap per faulting
IpcCall. Copying first means a copy fault / bad length leaves **no** record, cap,
delivery, or wake to unwind. No userspace pointer is retained past the copy; the
owned buffer is the sole source for the delivered message
(`Message::with_header(…, &payload_bytes[..len])`).

### Part 3 — NR 7 reply copy-before-claim (`reply_claim_before_source_copy=0`)

`handle_ipc_reply` already copies the replier payload into an owned buffer
(`payload_bytes: [u8; Message::MAX_PAYLOAD]`) **before** calling `kernel.ipc_reply`,
and the one-shot claim (`ipc.reply_caps[slot] = None`) happens inside `ipc_reply`
**after** the record is resolved. The replier payload copy therefore never happens
after the record is claimed. This ordering is asserted structurally so it cannot
regress.

### Achieved-invariant seal

```
STAGE_199A2A_INCARNATION_SAFE_REPLY_RECORD_SEAL numeric_tid_only_authority=0 request_copy_before_record_reserve=1 reply_claim_before_source_copy=0 leaked_reply_records_on_fault=0 duplicate_replies=0 duplicate_wakes=0 result=ok
```

Verified by the `stage199a2a_offlock_incarnation` hosted module (22 cases:
incarnation capture, authorization gate, cleanup gate, copy-before-reserve /
copy-before-claim ordering, one-shot preservation, ABI/policy preservation).

---

## 2. What is deferred, and why (NOT faked)

The originally requested seal
`STAGE_199_IPCCALL_REPLY_OFFLOCK_SEAL request_copy_under_lock=0
reply_copy_under_lock=0 … result=ok` asserts that the NR 6 / NR 7 **user payload
copies are removed from the broad lock**. That claim is **not honestly earnable in a
hosted-only increment**, for two source-grounded reasons:

1. **The off-broad-lock user-copy mechanism is disabled under hosted.** The only way
   to copy user memory off the broad `&mut KernelState` is the pre-global-lock split
   seam (`src/kernel/syscall_split.rs`), whose copy helper
   `copy_from_user_asid_split_read` is `#[cfg(not(feature = "hosted-dev"))]`-only —
   the hosted stubs (`try_split_debug_log_into_frame`,
   `try_split_futex_wake_into_frame`) return `None`. So an off-lock copy for NR 6 /
   NR 7 **cannot be exercised, let alone proven, in a hosted test**. Both handlers
   receive `&mut KernelState` and are therefore, by construction, under the broad
   lock in every hosted execution.

2. **NR 6 / NR 7 block the caller and drive a queue-advancing dispatch.** Moving a
   blocking IPC syscall off the global lock is exactly the `switch_required` case the
   codebase already **defers** for FutexWait
   (`syscall_split.rs::MARK_FUTEX_WAIT_DEFERRED_REASON =
   "GLOBAL_LOCK_RETIRE_CLASS_DEFERRED class=FutexWait
   reason=block_dispatch_switch_required_needs_global_lock"`). The kernel's own
   out-of-lock dispatch relocation restricts itself to the queue-neutral case and
   falls back to the global lock for `switch_required`. The IpcCall/IpcReply
   block+dispatch cannot be serviced off the global lock without the disclaimed
   multi-stage dispatch rewrite, which additionally requires QEMU hardware
   verification this increment forbids.

Honest state of the requested seal's fields:

```
STAGE_199_IPCCALL_REPLY_OFFLOCK_SEAL request_copy_under_lock=1 reply_copy_under_lock=1 request_copy_before_record_reserve=1 reply_claim_before_source_copy=0 numeric_tid_only_authority=0 duplicate_replies=0 duplicate_wakes=0 leaked_reply_records=0 result=deferred reason=broad_lock_payload_copy_needs_pre_global_lock_split_seam_which_is_hosted_disabled_and_block_dispatch_switch_required
```

This mirrors the **Stage 191D FutexWait deferral discipline**: land and prove the
genuinely-verifiable seam (here, the incarnation-safe record + copy ordering), and
record the one concrete blocker for the literal broad-lock removal with a
source-grounded reason — rather than emit a green seal for something not achieved.

---

## 3. Preserved policy / ABI

* `SYSCALL_COUNT = 32`, `VARIANT_COUNT = 22`, NR 27 absent, DebugLog cap = 192.
* Reply-cap enqueue unsupported; shared-region enqueue unsupported.
* Stage 198F: 10 supported classes / 30 live cells across 3 arches — untouched. The
  NR 6 reorder changes only fault-path resource lifecycle and preserves the
  successful-send marker sequence the sealed matrix greps.
* NR 6 / NR 7 are **not** added to the pre-global-lock split seam this stage.
