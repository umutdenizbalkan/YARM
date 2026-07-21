<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 199A2D2C2C — x86_64 Complete Bidirectional Cross-CPU Direct IPC (NR6 + NR7)

Goal: complete the reverse (NR7 reply) half of the sealed cross-CPU exchange and earn the FINAL
two-direction SMP seal. The forward (NR6 request) direction was sealed in Stage 199A2D2C2B2/B3 and is
reused UNCHANGED. This increment adds only the reply direction and the two-direction seal.

## Outcome — GENUINE LIVE two-direction seal

One fresh `QEMU_SMP=2` boot (`scripts/qemu-x86_64-ap-cross-cpu-reply-smoke.sh`, reply sub-selector
`yarm.x86_64_ipccall_direct_smp_reply=1`) drives the COMPLETE round trip and emits, after the sealed
forward delivery:

```
IPCREPLY_DIRECT_SMP_CALLER_BLOCKED arch=x86_64 caller_cpu=0 recv_v2_committed=1 saved_frame=1 waiter_exact=1 ack_published=1 absent_from_runqueue=1 ... result=ok
X86_BSP_RESCHEDULE_IPI_SENT sender_cpu=1 receiver_cpu=0 reason=remote_enqueue count=1 result=ok
X86_BSP_RESCHEDULE_IPI_RECEIVED cpu=0 pending=1 dispatch_in_handler=0 result=ok
X86_BSP_SAVED_DISPATCH_OK cpu=0 mode=saved scheduler_selected=1 continuations=1 ... result=ok
USER_LOG tid=... msg=X86_BSP_RECV_V2_CONTINUED cpu=0 result=ok
USER_LOG tid=... msg=X86_BSP_REPLY_USER_VALIDATED cpu=0 payload_ok=1 length_ok=1 meta_ok=1 continuations=1 result=ok
IPCREPLY_DIRECT_SMP_REPLY_OK sender_cpu=1 receiver_cpu=0 cross_cpu=1 reply_copies=1 caller_wakes=1 one_shot=1 result=ok
IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED arch=x86_64 reason=consumed_barrier reply_copies=1 caller_wakes=1 ipis=1 result=ok
STAGE_199_IPCREPLY_DIRECT_SMP_REPLY_USER_SEAL arch=x86_64 smp=2 ... result=ok
STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL arch=x86_64 smp=2 cross_cpu_request=1 cross_cpu_reply=1 ... result=ok
```

## The reverse direction, end to end

1. **CPU-0 caller block.** After its NR6 succeeds (`X86_BSP_NR6_REQUEST_SENT`), the client issues a
   GENUINE recv-v2 on its OWN reply endpoint (the reply RECEIVE cap it already holds + a mapped reply
   payload/meta buffer). It blocks on CPU 0, committing a saved continuation and publishing the
   authoritative blocked-caller ack; the oracle reply endpoint is now armed, so
   `maybe_publish_ipcreply_direct_blocked_caller_ack` fires and, after independently re-verifying every
   blocking-order invariant (saved frame, exact reply-endpoint waiter, absent from every runqueue, home
   CPU 0, live ack seq), emits `IPCREPLY_DIRECT_SMP_CALLER_BLOCKED caller_cpu=0` — the reply-side analog
   of B1's `IPCCALL_DIRECT_SMP_SERVER_BLOCKED`.

2. **CPU-1 genuine NR7.** After `X86_AP_RECV_V2_USER_VALIDATED`, the resumed server loads the
   receiver-local Reply CapId it read in ring 3 (recv-v2 meta offset 16) and issues a real NR7 IpcReply
   on it (NEVER a kernel-injected cap), with a bounded pre-ack WouldBlock retry (≤64, each with a short
   `pause` delay so the CPU-0 caller can block first). The NR7 split gate confines the off-lock reply to
   exactly the oracle reply endpoint and: returns non-mutating WouldBlock while no caller-ack is
   published; falls through to the accepted `ipc_reply_direct_txn` once the ack is claimable; and, after
   the one success, refuses a duplicate NR7 with canonical `WrongObject` (zero further work).

3. **Accepted reply transaction (reused, no fork).** `ipc_reply_direct_txn` runs its sealed ordering:
   resolve reply cap → validate replier → reserve Available→Reserved → copy reply payload+meta to the
   caller OFF-LOCK → claim the exact caller waiter → commit the caller (Runnable + home affinity CPU 0)
   → record Reserved→Consumed (the one-shot barrier, BEFORE the enqueue) → enqueue the caller LAST.

4. **CPU-1 → CPU-0 reschedule IPI.** On the accepted reply success (caller enqueued on CPU 0), the reply
   drain sends the canonical 0xF1 IPI to CPU 0 STRICTLY AFTER the enqueue (`X86_BSP_RESCHEDULE_IPI_SENT
   sender_cpu=1 receiver_cpu=0`). CPU 0's 0xF1 handler runs INLINE without dispatch: LAPIC EOI, records
   its own pending flag, emits `X86_BSP_RESCHEDULE_IPI_RECEIVED cpu=0 pending=1 dispatch_in_handler=0`,
   and iretqs. No self-set pending; no dispatch in the handler; no client migration.

5. **CPU-0 saved-frame resume.** In SMP=2 the BSP's passive scheduler (the single-CPU-only D6 local
   seam) never re-selects a caller enqueued on CPU 0's run queue, so an explicit saved-frame resume runs
   from every CPU-0 trap-return (NOT the 0xF1 handler — dispatch never happens in the interrupt
   handler), gated on one committed reply delivery + a one-shot latch. It makes the client the
   scheduler's `current` on CPU 0 via `on_preempt_prefer_on_cpu` (re-enqueuing the running task, then
   selecting the client — so `current_tid`, and thus the resumed client's DebugLog user-copy asid,
   is the client), reads its committed recv-v2 continuation (CR3/FS/RIP/RSP/GPRs + cleared result regs),
   clears the nested-trap depth guard, and `iretq`s to the instruction after its recv-v2 syscall — never
   a fresh re-entry. `X86_BSP_SAVED_DISPATCH_OK cpu=0 mode=saved`.

6. **CPU-0 ring-3 reply validation.** The resumed client reads its reply buffer + metadata via direct
   ring-3 loads (safe on both CPUs thanks to the Stage 199A2D2C2C EFER.NXE parity guard) and emits
   `X86_BSP_RECV_V2_CONTINUED` + `X86_BSP_REPLY_USER_VALIDATED cpu=0` only when the reply bytes
   ("RPLY-OK!"), the recv-v2 meta payload_len (== 8) and the meta sender_tid (!= 0) all check out.

7. **Duplicate reply proof.** The server issues ONE deliberate second NR7 through the same userspace
   cap; the Consumed record + claimed ack make it a duplicate, refused with `WrongObject` and ZERO
   additional copies / claims / enqueues / IPIs / wakes (`IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED`).

8. **Seals.** `IPCREPLY_DIRECT_SMP_REPLY_OK` (terminal, gated on the client's ring-3 validation + one
   committed reply), the reply user seal `STAGE_199_IPCREPLY_DIRECT_SMP_REPLY_USER_SEAL`, and the
   complete `STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL` (both directions, exact totals) — emitted ONLY
   after a clean genuine two-direction boot.

## Wiring (all default-off; gated on `yarm.x86_64_ipccall_direct_smp_reply=1`)
- `mod.rs`: reply sub-selector flag (implies request) + reply/duplicate counters + terminal reply-OK
  emitter + `maybe_emit_ipcreply_direct_smp_caller_blocked` + `ipcreply_direct_ack::commit_seq` +
  the client-TID record for the BSP dispatch hook.
- `exec_state.rs` (`build_ap_workload`): arms the reply endpoint; provisions the client reply meta
  buffer + reply markers + the server reply source buffer; swaps in the recv-v2-capable client stub and
  the NR7-capable server stub (both hand-asm, verified with `objdump`).
- `ipccall_direct_txn.rs`: the reply drain notes the delivery + sends the reverse IPI on success (mirror
  of the request drain; the accepted `ipc_reply_direct_txn` is UNCHANGED).
- `syscall_split.rs`: NR7 early-WouldBlock + duplicate refusal (split gate only; no txn fork) + the
  terminal reply-OK hook in the off-lock DebugLog path.
- `smp.rs`: `c2c_send_reschedule_ipi_to_cpu0`, the BSP 0xF1 handler `c2c_bsp_handle_reschedule_ipi`, and
  `maybe_emit_bsp_saved_dispatch_ok`.
- `descriptor_tables.rs`: the BSP 0xF1 inline intercept (EOI + handler + no dispatch) and the
  saved-dispatch attestation hook on the normal scheduler switch.
- `debug.rs`: terminal reply-OK hook in the global DebugLog path.
- `boot_command_line.rs`: `yarm.x86_64_ipccall_direct_smp_reply` knob.

## Preserved
C2A / B1 / B2 / B3 / C2C-parity seals re-run green (the request path uses its unchanged stubs and the
reply endpoint stays `usize::MAX` when the reply sub-selector is off). SYSCALL_COUNT=32,
VARIANT_COUNT=22, NR27 absent, DebugLog=192, REPLY_CAP_QUEUEING_SUPPORTED=false, Stage 198F cells=30,
Stage 199 functional cells=6, single-pair acknowledgement stores (both directions), queued IpcCall
unsupported, timeouts / notifications / server-death caller-wake unretired, multi-pair concurrency
unclaimed.
