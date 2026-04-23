<!-- SPDX-License-Identifier: Apache-2.0 -->

# Shared IPC Migration Guide (Current)

This guide reflects the current workspace boundary.

## Ownership model

- ABI opcode/payload ownership: `crates/yarm-ipc-abi`
- Shared service-side helper/runtime glue: `crates/yarm-srv-common`
- Service implementation ownership: extracted server crates (`yarm-*-servers`)

## Migration rule

When migrating an IPC surface:

1. Define/freeze request+reply codec in `yarm-ipc-abi`.
2. Use shared decode/reply helpers from `yarm-srv-common` where applicable.
3. Keep policy/orchestration in service crates, not kernel.
4. Add deterministic tests in the owning service crate.

## Shared-memory flow expectations

For transfer-cap/shared-memory flows:

1. receive/map through the current IPC contract,
2. consume in bounded region,
3. release transfer mapping (`TransferRelease`) to avoid leaks/drift.

## Gate expectations

- run `scripts/phase7-shared-ipc-gates.sh` for shared-IPC migration checks
- keep map/release parity checks green in canary tests
