# YARM Syscall ABI v1 (Frozen Contract)

- ABI Version: `1`
- Syscall count: `3`

## Syscall numbers

- `0`: `Yield`
- `1`: `IpcSend`
- `2`: `IpcRecv`

## Argument register layout (`args[0..]`)

- `args[0]`: capability id
- `args[1]`: user pointer
- `args[2]`: length
- `args[3]`: inline payload word (kernel/no-ASID path)

## Return layout

- `ret0`: status/value
- `ret1`: auxiliary value
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
