# Recommended Next Steps 1-5 Status

1. **Driver manager protocol hooks**: implemented kernel-facing registration and grant APIs (`register_driver`, `grant_driver_irq`, `grant_driver_dma`) and a frozen protocol constant set in `driver_proto` (`DRIVER_SERVER_ABI_VERSION`, opcodes), plus request handling in `driver_manager`.
2. **DMA window constraints**: implemented bounded DMA region capabilities with explicit `{id, offset, len}` metadata, alignment/page-window checks in `mint_dma_region_cap`, and per-driver IOVA window configuration/validation.
3. **Exited/dead lifecycle + restart token**: implemented `TaskStatus::Exited` / `TaskStatus::Dead`, restart tokens, restart budget/backoff policy, telemetry, and task restart/dead transitions.
4. **VFS FD lifecycle expansion**: Linux-compat dispatcher now routes `dup`, `fcntl`, and `poll` over VFS IPC in addition to open/close/read/write/ioctl.
5. **Deterministic stress test**: added deterministic mixed-sequence stress coverage combining SMP cross-CPU work and IRQ notification routing.
