# Recommended Next Steps 1-5 Status

1. **Driver manager protocol hooks**: implemented kernel-facing registration and grant APIs (`register_driver`, `grant_driver_irq`, `grant_driver_dma`) and Linux server routing primitives.
2. **DMA window constraints**: implemented bounded DMA region minting with offset/length alignment and page-window checks (`mint_dma_region_cap`).
3. **Exited/dead lifecycle + restart token**: implemented `TaskStatus::Exited` / `TaskStatus::Dead`, plus `exit_task`, `restart_task`, and `mark_task_dead` token flow.
4. **VFS FD lifecycle expansion**: Linux-compat dispatcher now routes `dup`, `fcntl`, and `poll` over VFS IPC.
5. **Deterministic stress test**: added a deterministic mixed sequence test combining SMP cross-CPU work and IRQ notification IPC routing.
