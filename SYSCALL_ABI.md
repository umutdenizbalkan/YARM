# YARM Syscall ABI v4 (Frozen Contract)

- ABI Version: `4`
- Syscall count: `4`

## Syscall numbers

- `0`: `Yield`
- `1`: `IpcSend`
- `2`: `IpcRecv`
- `3`: `VmMap` (YARM-native VM map syscall, capability-targeted)

## Argument register layout (`args[0..]`)

- `args[0]`: endpoint capability id
- `args[1]`: user pointer (small copy path) or shared-region offset (grant path)
- `args[2]`: length
- `args[3]`: inline payload lane 0 (kernel/no-ASID path) and recv metadata lane 0
- `args[4]`: inline payload lane 1 (kernel/no-ASID path) and recv metadata lane 1
- `args[5]`: optional transfer capability id (`0` or `u64::MAX` => none)

### `VmMap` argument layout

- `args[0]`: address-space mapping capability id (`CapId`)
- `args[1]`: virtual address (page-aligned)
- `args[2]`: mapping length in bytes (rounded up to page size)
- `args[3]`: protection flags bitmask (`READ=0x1`, `WRITE=0x2`, `EXEC=0x4`)
- `args[4]`: reserved (must be `0`)
- `args[5]`: reserved (must be `0`)

This syscall is intentionally YARM-native so Linux-compat `mmap` can keep Linux
argument order instead of repurposing `arg0` for capabilities.

## IPC model details

- **Synchronous rendezvous-friendly path**: small payloads (up to register lanes) can be passed through register lanes without kernel-side user-buffer copying on kernel/no-ASID paths.
- **Capability transfer opportunity on each IPC**: send can optionally attach a capability; recv returns transferred cap id in `ret2`.
- **Large payload zero-copy descriptor path**: if send length exceeds `Message::MAX_PAYLOAD`, sender provides a transferable memory capability and the kernel sends a shared-memory descriptor (`offset`,`len`) as payload metadata instead of copying bytes.

### Shared-memory transfer status (current)

- `Message::MAX_PAYLOAD` is fixed at 64 bytes for inline IPC envelopes.
- `OPCODE_SHARED_MEM` plus `SharedMemoryRegion { offset, len }` is implemented for large-send metadata.
- Transfer-envelope handling and capability materialization on `IpcRecv` are implemented, so the receiver can obtain a delegated memory capability.
- **Not yet implemented as an end-to-end fast path**: automatic/map-on-receive plumbing that wires the shared region into both communicating address spaces with lifecycle + revocation semantics suitable for sustained filesystem/network/display data-plane throughput.

In short: descriptor + cap handoff is present, but production-grade shared-memory
data-plane mapping policy/mechanics are still a critical remaining milestone.

## Return layout

- `ret0`: status/value (sender tid for recv)
- `ret1`: auxiliary value (message length)
- `ret2`: transferred capability id (`u64::MAX` sentinel when none)
- `error`: syscall error code (`0` means success)

## Error codes

- `1`: `InvalidNumber`
- `2`: `InvalidArgs`
- `3`: `InvalidCapability`
- `4`: `MissingRight`
- `5`: `WrongObject`
- `6`: `QueueFull`
- `7`: `WouldBlock`
- `8`: `PageFault`
- `255`: `Internal`


## Per-ISA shape source of truth

- Trap/syscall argument lane count and IPC inline register-lane width are sourced from `crate::arch::syscall_abi` (`TRAPFRAME_ARG_REGS`, `IPC_REGISTER_WORDS`).
- `src/kernel/syscall.rs` and `src/kernel/trapframe.rs` include compile-time assertions to keep ABI lane mapping synchronized with the selected ISA profile.
