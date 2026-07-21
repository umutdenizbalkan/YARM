<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D2C2B3 — AP Saved-Resume User-Memory Consumption Proof

Goal: root-cause and fix the CPU-1 ring-3 fault that occurred when the remotely-awakened AP recv-v2
server read its delivered payload/metadata, then rerun the cross-CPU NR6 oracle and prove the resumed
CPU-1 userspace server ITSELF validates the request payload, length, recv-v2 metadata, and fresh
receiver-local Reply cap — via direct ring-3 loads. No NR7.

## Outcome — GENUINE LIVE user-consumption seal

One fresh `QEMU_SMP=2` boot (`scripts/qemu-x86_64-ap-cross-cpu-user-consume-smoke.sh`) now produces,
after the sealed cross-CPU delivery + saved-frame resume, with **zero ring-3 fault after resume**:

```
USER_LOG tid=20205 msg=X86_AP_RECV_V2_CONTINUED cpu=1 request_ok=1 metadata_ok=1 reply_cap=1 continuations=1 result=ok
IPCCALL_DIRECT_SMP_REQUEST_OK sender_cpu=0 receiver_cpu=1 cross_cpu=1 request_copies=1 server_wakes=1 server_continuations=1 result=ok
USER_LOG tid=20205 msg=X86_AP_RECV_V2_USER_VALIDATED cpu=1 payload_ok=1 length_ok=1 meta_ok=1 reply_cap_nonzero=1 continuations=1 result=ok
STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_USER_SEAL arch=x86_64 smp=2 sender_cpu=0 receiver_cpu=1 cross_cpu=1 saved_resume=1 ring3_payload_read=1 ring3_metadata_read=1 ring3_reply_cap_read=1 duplicate_deliveries=0 duplicate_wakes=0 result=ok
```

The resumed server executes NORMAL ring-3 loads: it reads the delivered payload at `0x2003_0000` and
compares it byte-exact to `NR6-REQ!`, reads the recv-v2 metadata `payload_len` field (== 8) and the
receiver-local Reply cap field (offset 16, != 0), and emits `X86_AP_RECV_V2_USER_VALIDATED` only when
all three direct-load checks pass (else `X86_AP_RECV_V2_VALIDATE_FAIL`). This is no longer a kernel
substitute — the userspace load itself is the proof.

## Exact ring-3 fault captured

Instrumenting a real AP #PF handler (the AP IDT previously routed vector 14 to the catch-all park
stub, so the fault hung CPU 1 silently) captured, on the payload read:

```
X86_AP_RECV_V2_USER_READ_FAULT cpu=1 cr2=0x20030000 error=0xc present=0 write=0 user=1 rsvd=1 ifetch=0 region=payload
X86_AP_RECV_V2_USER_READ_FAULT_WALK cpu=1 rip=0x2000003b cr3=0x1000b000 pml4e=0x1000c027 pdpte=0x1000d027 pde=0x1000e027 pte=0x8000000010292007 pte_present=1 pte_user=1 pte_write=1
```

## Root cause

`error=0xc` = **reserved-bit** (bit 3) + **user** (bit 2), a read. The re-walk shows the PTE
`0x8000000010292007` is **present, user, writable — with bit 63 set (NX)**. The fault is a
RESERVED-BIT violation, not a mapping problem: `configure_syscall_msrs_for_self` set only `EFER.SCE`
on the AP, never `EFER.NXE`. With NXE disabled, PTE bit 63 (NX) is a RESERVED bit, so **every AP
ring-3 access to a non-executable data page (payload/meta/stack — all NX=1) took a reserved-bit
#PF**, while the executable code page (NX=0) worked. The BSP enables NXE in early boot (`boot.rs`
`or edi, 0x800`); the AP path was missing it.

## CR3 / PCID / ASID findings
CR3 `0x1000b000` is the server's own asid-5 page table (its code page translates + executes there).
No PCID/no-flush issue and no wrong-CR3: the resume loads `cr3_for_asid(server asid)` and the walk
confirms all four levels present+user. The kernel copy (`copy_slice_to_user_asid_split_write`) writes
to the same physical frame (`0x10292000`) the ring-3 PTE resolves — no aliasing.

## Mapping / TLB findings
The payload/meta/stack mappings are created in `build_ap_workload`'s aspace-creation block BEFORE the
server ever runs; the resume's `mov cr3` flushes the non-global TLB. No stale translation — the fault
was purely the NXE-vs-NX reserved-bit interaction, reproduced identically every boot.

## Minimal fix
`configure_syscall_msrs_for_self` now sets `EFER.SCE | EFER.NXE` (bit 11). The kernel page tables
already use NX (proving NX support), so the AP must honor it too. Limited to the AP EFER — no page
tables, address spaces, or the accepted cross-CPU machinery changed. A real AP #PF diagnostic handler
(`yarm_ap_page_fault_stub` → `yarm_x86_ap_page_fault_diag`) is wired into the AP IDT so any future AP
fault is visible (decoded error bits + PTE walk) instead of a silent park.

## Preserved
C2A (`STAGE_199_X86_AP_SAVED_RETURN_SEAL`), B1 (`STAGE_199_X86_AP_RECV_V2_BLOCK_SEAL`), and B2
(`STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_SEAL`) all re-run green with NXE on. SYSCALL_COUNT=32,
VARIANT_COUNT=22, NR27 absent, DebugLog=192, REPLY_CAP_QUEUEING_SUPPORTED=false, Stage 198F cells=30,
Stage 199 functional cells=6, single-pair oracle ack store, queued IpcCall unsupported, timeouts /
notifications / server-death caller-wake unretired. 12 hosted guards + the accepted machinery
untouched. NR7 remains disabled.

## Cross-CPU NR7 plan (199A2D2C2C)
Now that the resumed CPU-1 server holds a genuine, userspace-readable receiver-local Reply cap, NR7
mirrors the request on the reply endpoint: the CPU-0 client blocks in recv-v2 on its reply endpoint
(a committed saved continuation on CPU 0); the resumed server issues a real NR7 IpcReply on the Reply
cap through the accepted off-lock reply transaction (reserve → caller-copy → exact-waiter claim →
record Consumed → single enqueue on CPU 0); CPU 1 sends a reschedule IPI to CPU 0; CPU 0's managed
idle dispatcher restores the client's recv-v2 continuation via the SAME sealed saved-frame resume
(now applied to the BSP-bound caller) — and, because EFER.NXE is now correct on both CPUs, the
resumed client can validate the reply bytes with direct ring-3 loads too. The only new integration is
the reply direction of the wake + the BSP-side resume; the AP saved-frame return, the accepted reply
transaction, and now the AP user-memory consumption are all done.
