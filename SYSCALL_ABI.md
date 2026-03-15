# YARM Syscall ABI v2 (Frozen Contract)

- ABI Version: `2`
- Syscall count: `3`

## Syscall numbers

- `0`: `Yield`
- `1`: `IpcSend`
- `2`: `IpcRecv`

## Argument register layout (`args[0..]`)

- `args[0]`: endpoint capability id
- `args[1]`: user pointer (small copy path) or shared-region offset (grant path)
- `args[2]`: length
- `args[3]`: inline payload lane 0 (kernel/no-ASID path) and recv metadata lane 0
- `args[4]`: inline payload lane 1 (kernel/no-ASID path) and recv metadata lane 1
- `args[5]`: optional transfer capability id (`0` or `u64::MAX` => none)

## IPC model details

- **Synchronous rendezvous-friendly path**: small payloads (up to register lanes) can be passed through register lanes without kernel-side user-buffer copying on kernel/no-ASID paths.
- **Capability transfer opportunity on each IPC**: send can optionally attach a capability; recv returns transferred cap id in `ret2`.
- **Large payload zero-copy descriptor path**: if send length exceeds `Message::MAX_PAYLOAD`, sender provides a transferable memory capability and the kernel sends a shared-memory descriptor (`offset`,`len`) as payload metadata instead of copying bytes.

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
