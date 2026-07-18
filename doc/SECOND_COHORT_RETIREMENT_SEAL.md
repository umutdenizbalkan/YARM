<!-- SPDX-License-Identifier: Apache-2.0 -->
# Second-Cohort Retirement Seal — `IpcSendSharedRegionDirect` 3/3 + Policy Freeze

Stage 198E3D-S. Records the completed direct shared-region retirement matrix, the
authoritative production ABI/contract it rests on, and the explicit freeze of the
shared-region **enqueue** class as unsupported.

## 1. Commit tested

`d4b8fd6` (Stage 198E3C2C head). All three artifacts are built FRESH from this commit
by the per-architecture smoke scripts the matrix runner invokes; no stale artifact or
log from the individual-cell stages is reused.

## 2. Three direct live cells

`IpcSendSharedRegionDirect` is live-retired on all three architectures via one fresh
`QEMU_SMP=1` oracle boot each, run serially by
`scripts/qemu-shared-region-direct-matrix-seal.sh`:

| arch | feature | selector | slot-5 | per-arch seal |
| --- | --- | --- | --- | --- |
| x86_64  | `x86-shared-region-direct-oracle`     | `yarm.x86_64_shared_region_direct_oracle=1`  | 2 | `SECOND_COHORT_SHARED_REGION_DIRECT_LIVE_SEAL arch=x86_64 classes=1 live_cells=1 fuse_trips=0 result=ok` |
| aarch64 | `aarch64-shared-region-direct-oracle` | `yarm.aarch64_shared_region_direct_oracle=1` | 6 | `… arch=aarch64 … result=ok` |
| riscv64 | `riscv-shared-region-direct-oracle`   | `yarm.riscv_shared_region_direct_oracle=1`   | 7 | `… arch=riscv64 … result=ok` |

Aggregate (emitted only after all three fresh per-arch seals pass in the same run):

```
SECOND_COHORT_SHARED_REGION_DIRECT_MATRIX_SEAL arches=3 classes=1 live_cells=3 fuse_trips=0 duplicate_wakes=0 result=ok
```

Each cell requires, exactly once: `IPCSEND_SHARED_REGION_OBJECT_OK`,
`IPCSEND_SHARED_REGION_MAP_OK`, `IPCSEND_SHARED_REGION_LIFECYCLE_OK`,
`GLOBAL_LOCK_RETIRE_CLASS_BEGIN`/`DONE class=IpcSendSharedRegionDirect result=ok`, the
arch-specific `…_SHARED_REGION_DIRECT_LIVE_ORACLE_DONE … result=ok`, and
`DISPATCH_POST_WORK_DONE kind=blocked_waiter_shared_region result=ok`; plus
`mapped_pages=2 fresh_cap=1 readonly=1 first_release=ok second_release=rejected wakes=1
continuations=1` and one successful send.

## 3. Large-length `IpcSend` ABI (production, arch-neutral)

The shared-region transfer is selected purely by the **large-transfer form** of
`IpcSend` (NR 1): `arg(LEN) > Message::MAX_PAYLOAD` (128). It is NOT selected by an
inline `OPCODE_SHARED_MEM` message (that value is produced by the kernel on this path).
Args: `arg0 = ep_cap`, `arg1 = offset`, `arg2 = region_len (> 128)`, `arg5 = source
shared-region cap`. The kernel builds a 16-byte `SharedMemoryRegion { offset:u64@0,
len:u64@8 }` descriptor; the source cap is delegated/duplicated (not moved). See
`doc/STAGE_198E3C1_SHARED_REGION_USERSPACE_CONTRACT.md`.

## 4. Target-specific transfer VAs (must NOT be unified)

The two-page oracle receive window is per-`target_arch`, chosen from each arch's user
address-space boundaries. Reusing one value across arches is a bug:

| arch | oracle VA | why this value |
| --- | --- | --- |
| x86_64  | `0x4000_0000` (1 GiB)   | above image/initrd, far below the multi-TB user stack |
| aarch64 | `0x2000_0000` (512 MiB) | user (TTBR0) half ends at `KERNEL_SPACE_BASE = 0x4000_0000`, so 1 GiB is not a user VA (would fault `PrivilegeViolation`) |
| riscv64 | `0x2000_0000` (512 MiB) | `USER_BRK_DEFAULT_BASE = 0x4000_0000` makes 1 GiB the **heap base**; the window sits in the image↔heap gap, below `KERNEL_SPACE_BASE = 0x8000_0000` |

The kernel `SHARED_REGION_ORACLE_USER_VA` and userspace `SHARED_REGION_ORACLE_VA` MUST
be equal per arch (the ack gate rejects a wrong-VA recv). Hosted guards assert the
agreement and that riscv/aarch64 do NOT reuse the x86 1 GiB value.

## 5. Target-specific release decoders (must NOT be unified)

`TransferRelease` (NR 4) success returns the released length; the decode is per-arch:

| arch | convention |
| --- | --- |
| x86_64  | separate error register (`RCX`); released length in `RAX`/`ret0` |
| aarch64 | X0 = released length (≥ PAGE, page-aligned) on success, X0 = a sub-PAGE `SyscallError` code on failure — page-alignment is the discriminator (`decode_release`) |
| riscv64 | identical to AArch64 (`a0` value-or-error), so the same `#[cfg(any(aarch64, riscv64))]` branch applies |

Collapsing the x86 separate-error-register path into the a0/x0 value-or-error path (or
vice-versa) mis-decodes a successful release as an error. Hosted guards assert both
branches remain distinct.

## 6. Authoritative blocked-recv acknowledgement

The direct producer fires only against an authoritatively-committed recv-v2 waiter. The
receiver's recv path publishes `SHARED_REGION_BLOCKED_RECV_ACK` (module
`shared_region_blocked_recv`) AFTER the full commit point (`BlockedRecvState` stored),
carrying `{receiver tid+asid, endpoint index+generation, RecvV2 state, oracle VA, two-page
length, non-null metadata ptr}`. A pre-ack send returns the canonical retryable
`WouldBlock` before ANY mutation (source cap + envelope preserved); the ack is consumed
only AFTER the post-work publication (consume-after-publish, exactly once).

## 7. Off-lock mapping / finalization

The accepted transaction (`shared_region_txn`) runs FULLY off the broad lock through
`SharedRegionOffLockCtx`: claim → map the two pages (NX, `WriteBack`) → user recv-v2 meta
copy → finalize (clear blocked-return + the exact endpoint waiter slot, enqueue the
receiver exactly once). Fresh maps need no TLB shootdown; only rollback unmaps do. A
failed step performs a single idempotent rollback (no wake, prefix unmapped, provisional
cap revoked, pin released) and emits NO attestation/retirement marker.

## 8. Exact waiter identity `{tid, asid}`

The endpoint waiter slot carries `ReceiverWaiterIdentity { tid, asid }`. The direct
producer requires the committed generation-bearing waiter for THIS receiver; finalization
clears that exact slot. A stale-waiter or ASID mismatch fails closed (no delivery).

## 9. First / duplicate cleanup

The receiver-local cleanup cap (fresh, distinct from the sender's source cap) authorizes
release. The FIRST `release_shared_region_mapping` returns `Ok(map_len)` (two-phase
unmap + cap revoke + mapping removal); a DUPLICATE returns `InvalidArgs` (the cap +
mapping are gone) — surfaced verbatim per the arch decoder. Observed live:
`first_release=ok second_release=rejected`.

## 10. Shared-region ENQUEUE — UNSUPPORTED (policy freeze)

| class | status |
| --- | --- |
| `IpcSendSharedRegionDirect` | **supported**, live-retired on x86_64 + AArch64 + RISC-V |
| `IpcSendSharedRegionEnqueue` | **NOT supported** for production retirement |

`IpcSendSharedRegionEnqueue`:
- has **no** production feature/build marker — the artifact guard
  (`scripts/lib/build-qemu-artifacts-common.sh`) FORBIDS `class=IpcSendSharedRegionEnqueue`
  in **every** build (armed or not, every arch);
- has **no** live oracle and **no** runtime producer;
- is **not** counted in the Stage 198F supported matrix.

The hosted enqueue transaction tests remain as mechanism/reference coverage only; they do
NOT constitute production support. The hosted code is NOT deleted — production gates and
artifact guards stay fail-closed instead. No queued shared-region producer is enabled and
no new capability-transfer variant is created in this stage.

## 11. Stage 198F supported `IpcSend` matrix

Supported second-cohort `IpcSend` retirement cells = **6 classes × 3 arches = 18**:

| # | class | x86_64 | aarch64 | riscv64 |
| --- | --- | --- | --- | --- |
| 1 | `IpcSendPlain`               | ✓ | ✓ | ✓ |
| 2 | `IpcSendPlainEnqueue`        | ✓ | ✓ | ✓ |
| 3 | `IpcSendOrdinaryCap`         | ✓ | ✓ | ✓ |
| 4 | `IpcSendOrdinaryCapEnqueue`  | ✓ | ✓ | ✓ |
| 5 | `IpcSendReplyCap`            | ✓ | ✓ | ✓ |
| 6 | `IpcSendSharedRegionDirect` | ✓ | ✓ | ✓ |

Excluded (would wrongly make 21): `IpcSendSharedRegionEnqueue` (3 cells) — unsupported.
`IpcSendReplyCapEnqueue` is likewise forbidden and never counted.

The supported-cell count is therefore **18, not 21**. Stage 198F's combined seal aggregates
these 18 second-cohort cells (alongside the 12 first-cohort cells) and must never count the
shared-region enqueue class.

## 12. Stage 198F — complete 30-cell cross-architecture retirement seal

1. **Exact commit tested:** the Stage 198F seal commit (this document's head). The combined runner
   (`scripts/qemu-combined-retirement-seal.sh`) refuses to seal on a dirty tree, records
   `git rev-parse HEAD`, builds every artifact fresh from that HEAD, and rejects any artifact that
   predates the run or any subordinate seal not present in the current-run log dir.

2. **First cohort — 12 cells** (4 classes × 3 arches): `DebugLog`, `FutexWake`, `FutexWait`, `Yield`
   on x86_64 + aarch64 + riscv64 (`FIRST_COHORT_CROSS_ARCH_SEAL arches=3 classes=4 result=ok`).

3. **Supported IpcSend cohort — 18 cells** (6 classes × 3 arches): the §11 table
   (`IpcSendPlain`, `IpcSendPlainEnqueue`, `IpcSendOrdinaryCap`, `IpcSendOrdinaryCapEnqueue`,
   `IpcSendReplyCap`, `IpcSendSharedRegionDirect`).

4. **30-cell total:** `first_cohort_cells=12` + `ipc_send_cells=18` = `total_live_cells=30`
   (`total_classes=10`). The runner computes this from an EXPLICIT per-cohort class manifest, never a
   broad substring count, and emits the seal only when every current-run cohort passed, the total is
   exactly 30, `fuse_trips=0`, `duplicate_wakes=0`, and the tree stayed clean at the seal commit.

5. **Exact supported class list (10):** `DebugLog`, `FutexWake`, `FutexWait`, `Yield`,
   `IpcSendPlain`, `IpcSendPlainEnqueue`, `IpcSendOrdinaryCap`, `IpcSendOrdinaryCapEnqueue`,
   `IpcSendReplyCap`, `IpcSendSharedRegionDirect`.

6. **`IpcSendReplyCapEnqueue` — UNSUPPORTED:** the queued reply-cap path is not a production
   retirement class. It has no live oracle/producer and the artifact guard forbids
   `class=IpcSendReplyCapEnqueue` in every build. It is a policy exclusion (not a missing cell) and is
   never counted; counting it would wrongly make the total 33.

7. **`IpcSendSharedRegionEnqueue` — UNSUPPORTED:** as in §10 — no feature/build marker, no live
   oracle, no runtime producer; forbidden in every build; never counted (would also make 33).

8. **Direct shared-region 3/3:** the shared-region DIRECT matrix cohort
   (`SECOND_COHORT_SHARED_REGION_DIRECT_MATRIX_SEAL arches=3 classes=1 live_cells=3 fuse_trips=0
   duplicate_wakes=0 result=ok`) contributes the final 3 IpcSend cells; x86_64/aarch64/riscv64 all pass.

9. **Artifact/run manifest:** `"$LOGROOT/stage198f-manifest.txt"` under the per-run
   `LOGROOT=/tmp/combined-retirement-seal/<RUN_ID>/` — one row per (arch, cohort) with artifact path,
   SHA-256, size, mtime, log path, and individual seal result, plus the seal commit + run id + totals.

10. **Top-level seal** (host-side only — NEVER a kernel DebugLog marker literal):

```
STAGE_198F_COMPLETE_RETIREMENT_SEAL_HEADER commit=<sha> run_id=<id> manifest=<path> result=ok
STAGE_198F_COMPLETE_RETIREMENT_SEAL arches=3 first_cohort_classes=4 first_cohort_cells=12 ipc_send_classes=6 ipc_send_cells=18 total_classes=10 total_live_cells=30 reply_cap_enqueue=unsupported shared_region_enqueue=unsupported fuse_trips=0 duplicate_wakes=0 result=ok
```

11. **Next stage — Stage 199 (blocking IPC / D2):** the blocking-IPC / reply / timeout / notification
    seal (`IpcCall` retirement, reply-record lifecycle, send/recv timeouts, notifications). Stage 198F
    does NOT begin any of it, nor shared-region/reply-cap enqueue enablement, D3, or D6. The hosted
    enqueue transaction tests remain mechanism/reference coverage only and are never described as
    production-retired.
