# TID allocation contract

## Allocation domains
- **Static/bootstrap TIDs**: `0..=static_tid_upper_bound`.
- **Dynamic TIDs**: `dynamic_tid_floor..=u64::MAX`.

The current kernel policy exports these boundaries through:
- `KernelState::dynamic_tid_floor()`
- `KernelState::static_tid_upper_bound()`
- `KernelState::is_dynamic_tid(tid)`

## Caller expectations
- Dynamic TIDs are **unique while live**, but are **not globally monotonic forever** because the cursor can wrap from `u64::MAX` back to `dynamic_tid_floor`.
- Callers must not infer ordering semantics from numeric TID magnitude across long runtimes.
- If logic needs to classify dynamic vs static IDs, use policy helpers rather than hard-coded literals.

## Telemetry and diagnostics
- `dynamic_tid_allocations`: successful dynamic allocations.
- `dynamic_tid_wraps`: wrap events when cursor rolls to floor.
- `gap_floor_repairs`: times a stale cursor below floor was normalized.

Boot logs emit a policy marker (`YARM_TID_POLICY ...`) and wrap events emit `YARM_TID_ALLOC_WRAP ...`.
