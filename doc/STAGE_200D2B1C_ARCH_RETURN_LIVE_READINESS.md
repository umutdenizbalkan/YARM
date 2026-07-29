<!-- SPDX-License-Identifier: Apache-2.0 -->

# Stage 200D-2B1C — Architecture Return Contract and Live Readiness

The three-architecture return contract for the non-returning `CurrentTaskExited { tid, asid }`
disposition, pinned as a contract, plus the wiring that makes a ServerDies live run possible:
selector forwarding, disposable oracle tasks, exact-commit runners and feature-off binary
guards.

This stage runs no QEMU and claims **no live cell**. It emits a *readiness* seal — the
statement that a later stage can execute the runners, not that they have been executed.

## 1. What was already there, and what was missing

The consumers themselves landed in Stages 200D-0B3 (x86_64), 0C1 (AArch64) and 0D1 (RISC-V)
and were live-proven per architecture for the `ExitCurrentTask` oracle. Auditing them against
this stage's five requirements found the return contract **already satisfied on all three
ports** — full `{tid, asid}` validation, exited-task-never-restored, replacement-or-idle, and
per-port epilogue ownership with no duplicate cleanup.

One thing was genuinely missing, and it was load-bearing:

> `IPC_REPLY_TIMEOUT_MODE_SERVER_DIES = 3` existed, and the entire ServerDies mechanism was
> wired behind it — but **no boot-command-line value mapped to it**. All three per-arch
> selectors accepted only `timeout-wins|1` and `reply-wins|2`. The scenario was unreachable
> from a real boot and could never have run live, on any port.

`server-dies|3` is now accepted by all three selectors. That is the only production behaviour
change in this stage.

## 2. The return contract, per port

| Requirement | x86_64 | AArch64 | RISC-V |
|---|---|---|---|
| exactly one consumer | ✓ | ✓ | ✓ |
| full `{tid, asid}` validated | ✓ | ✓ | ✓ |
| reaped TCB unimpersonatable (`None => true`) | ✓ | ✓ | ✓ |
| exited task never current / never reselected | ✓ | ✓ | ✓ |
| replacement path | `RESTORE_OWNER_PREPARED owner=replacement` | `RESTORE_OWNER owner=replacement` | `RESTORE_OWNER owner=replacement` |
| idle path | `owner=idle`, ordinary epilogue | `owner=idle` → `idle_no_eret_loop()` | `owner=idle` → `RiscvIdleReason::ExitCurrentTaskNoRunnable` |
| epilogue / depth ownership | vector epilogue owns the single depth clear; consumer writes none | hardware ERET owns cleanup | trap bridge owns the single `sret`; `software_depth_clears=0` |
| consumed after lock release + drains | **no — in-lock by design** | ✓ `broad_lock=0` | ✓ `broad_lock=0` |

### The one deliberate divergence

AArch64 and RISC-V consume the disposition **after** broad-lock release and after every
post-lock drain, and attest `broad_lock=0`. x86_64 does **not**: it consumes in-lock and
attests `broad_lock=1`.

This is not drift. Stage 200D-0B3 established it deliberately, correcting the false
`broad_lock=0` claims that Stage 200D-0B1/0B2 had sealed. The x86_64 consumer runs where the
exiting identity is still coherent and the outgoing owner is selected, and performs **no**
side effect there — no teardown, enqueue, terminal claim, user copy, frame write or depth
write. The hardware frame is not committed until `flush_trap_context_to_iret_frame` in the
vector epilogue, which runs after `with_cpu` returns *and* after every drain. So the effect
ordering this stage asks for holds on x86_64; only the position of the `take` differs, and it
says so rather than claiming otherwise.

Guard `a07` asserts the two post-lock ports attest release and drain *before* consuming, and
separately asserts the x86_64 port declares `broad_lock=1` and defers its frame commit. Moving
the x86_64 consumer post-lock would break a live-sealed design; it was not done.

## 3. Live-readiness wiring

- **Feature forwarding** — `server-dies|3` on all three per-arch selectors; an unrecognized
  value still leaves the oracle inert.
- **Disposable oracle task** — the ServerDies server exits through the ordinary
  `exit_current_task()`; any return is `IPC_SERVER_DEATH_EXIT_RETURNED ... result=fail`, a hard
  failure rather than a fallback. Scenario selection goes through the shared ABI decoder, not a
  per-port table.
- **Exact-commit runners** — `scripts/qemu-{x86_64,aarch64,riscv64}-server-dies-smoke.sh` over
  `scripts/lib/serverdies-runner-common.sh`. Each freezes SHA + tree, refuses a dirty tree,
  re-checks after every phase, uses fresh logs, boots once with `-no-reboot -no-shutdown`,
  grades an **ordered** marker chain, rejects an 18-literal forbidden set, and requires exactly
  one caller wake and one PeerDeath winner.
- **Feature-off binary guards** — RUN_A is two-sided: oracle literals must be absent *and* the
  production server-death literals must survive, because server death is not an oracle feature.

## 4. Validation

| Check | Result |
|---|---|
| `stage200d2b1c_arch_return` (hosted) | 10 passed |
| `server_dies_runner_scope` (integration) | 9 passed |
| hosted default (lib) | 3698 passed, 0 failed, 2 ignored |
| hosted `ipc-reply-timeout-oracle-core` (lib) | 3855 passed, 0 failed, 2 ignored |
| every other `cargo test` target, both feature sets | 0 failed |
| `cargo fmt --check`, `git diff --check` | clean |
| workspace, 3 feature-off kernels, 3 oracle-on kernels, 3 `init_server` | build |
| feature-off binary audit, x86_64 / AArch64 / RISC-V | 0/14 oracle literals, 14/14 production literals |
| discriminating counter-check (oracle-ON, each arch) | 17 oracle-literal occurrences |

Mutation-tested, 9/9 caught: selector dropped on one port; RISC-V idle reason changed; AArch64
idle restore-owner removed; RISC-V claiming a consumer-side depth clear; post-boot commit
re-check dropped; marker chain unordered; feature-off audit made one-sided; `-no-reboot
-no-shutdown` removed; `EXIT_RETURNED` dropped from the forbidden set.

## 5. Live-readiness seal

```text
STAGE_200D2B1C_ARCH_RETURN_LIVE_READINESS_SEAL
arches=3
consumers=3
consumers_per_arch=1
identity_fields=tid_asid
replacement_paths=3
idle_paths=3
duplicate_cleanups=0
consumer_side_effects=0
selector_ports_forwarded=3
disposable_oracle_tasks=3
exact_commit_runners=3
runner_scope_tests=9
arch_contract_tests=10
feature_off_oracle_literals=0
feature_off_production_literals=14
qemu_boots=0
live_cells=0
result=ok
```

`qemu_boots=0` and `live_cells=0` are part of the seal, not omissions. This is a *readiness*
seal: it states that the contract is pinned and the runners are prepared and guarded. Earning
the three live cells is a later stage's act, and each runner emits its own
`STAGE_200D2B1C_<ARCH>_SERVER_DIES_SEAL` when it is actually executed — a distinct seal, so a
prepared-but-unrun runner can never be mistaken for a live proof.
