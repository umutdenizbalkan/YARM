# U9-COW1 — STOPPED AT THE LIVE GATE. Nothing delivered to main.

**Boundary: the x86_64 COW witness does not exist at the directive's own base.**

The mission is premised on `VM_COW=1` supplying an existing live witness — the two
`path=private_copy` recoveries the U9-PF derivation measured at `adcf229`. That premise is
false at `11d6ba4`.

Measured with fresh matched artifacts, `VM_COW=1`, in an isolated worktree at base `11d6ba4`:

| Marker | Base 11d6ba4 | Head (route wired) |
|---|---|---|
| `VM_COW_ENABLED` | 1 | 1 |
| `VM_COW_FORK_BEGIN` | **0** | **0** |
| `VM_COW_FAULT_BEGIN` | **0** | **0** |
| `VM_COW_PHASE_METADATA` | **0** | **0** |
| `VM_COW_DONE` | **0** | **0** |
| `PAGE_FAULT_HANDLED_COW` | **0** | **0** |
| `PAGE_FAULT_ENTRY` | **0** | **0** |

The profile engages — `VM_COW_ENABLED` is present, the knob is applied, the sender-wake proof
workload runs — and the smoke exits 0. It exits 0 **vacuously**: every COW assertion in the
Stage-172 block is conditional on `if log_has_pattern "VM_COW_FAULT_BEGIN"`, so a boot with zero
COW faults passes the same gate as a boot with two correct ones. That is the same shape of
false comfort U9-FT3 already cost this programme once.

**Zero page faults of ANY class occur.** COW recovery needs a `fork` (NR 12): only
`fork_user_process_cow` calls `clone_user_address_space_cow`, which is what marks pages COW.
No shipped userspace image reaches that syscall in any profile. `FORK_PROOF_*` markers do
appear, but they are spawn/resume markers, not a COW clone — `VM_COW_FORK_BEGIN` is 0.

**No other already-shipped profile qualifies.** `vm-cow` is the only COW profile; the U9-PF
Finding-3 table already measured plain x86_64 core, RISC-V core, `FAULT_DELIVERY` and
`SPAWN_LIFECYCLE` as producing zero page faults of any class, and there is no fork or COW
script in `scripts/`.

**Why this stops the increment rather than being worked around.** §4 says wire x86_64 *because*
`VM_COW=1` supplies the witness, and "Do not manufacture evidence". §6 requires, per run, that
"the existing two COW witnesses still occur" — they do not occur once, at head or at base — and
says the target COW behavior may not be deferred. Producing the witness would mean writing a
fork workload, which §Scope-exclusions forbids ("no new script/workload") and which would be
fabricated evidence for a route that has never run.

**What is on this branch, and what it is not.** `72ae5ec` contains the full derivation result:
the shared rank-6 object-slot owner, the `CowRecovery` outcome, the
`cow_recover_private_copy_split` transaction and the pre-lock route, compiling on all four
targets. It is NOT delivered, because delivering it would put an unwitnessed production route on
the live PageFault path. It is retained here as the record of the attempt, not as a placeholder
in `main` — `main` is untouched at `11d6ba4`.

**Known state of this branch head (not green, deliberately not fixed):**
`kernel::boot::tests::u9tm_proof_gate::both_bridges_skip_the_broad_arm_on_post_work` fails.
It enumerates the dispositions that may skip the broad arm and the route adds a third
(`cow_recovered`). That guard is re-derivable — the claim it protects is still true — but
re-deriving a guard to accommodate a route that cannot be live-proven would be the wrong order
of work.

**What the next increment needs, in order.** A real COW witness, from an existing shipped
surface rather than a new one: something in the boot path must actually call `fork` (NR 12) and
then write to an inherited page. Until one exists, `try_handle_cow_fault`'s `private_copy` arm
has no live coverage at all — which is worth recording independently of this route, because the
U9-PF routing matrix names `SplitCow` as x86_64's route on the strength of a witness that has
since disappeared.
