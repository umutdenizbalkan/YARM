# x86_64 boot path notes (initrd / first-user ABI)

- PVH module parsing interprets module entries as **(start, size)**; end is computed as `start + size`.
- PVH `modlist_paddr` and module payload addresses are physical and are accessed through the bootstrap higher-half alias (`KERNEL_BOOTSTRAP_VIRT_BASE + phys`).
- First-user image selection prefers `/init` from initramfs CPIO; synthetic ELF is fallback-only.
- x86_64 ring3 startup ABI lanes:
  - `rdi/rsi/rdx` => arg0/arg1/arg2
  - `rcx` => mapped startup args block VA
  - `r8` => startup args count
  - `r9` => reserved
- The startup args block is copied into user-mapped memory before ring3 entry.
