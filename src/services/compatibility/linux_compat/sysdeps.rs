use super::{LINUX_NR_BRK, LINUX_NR_MMAP, LINUX_NR_MPROTECT, LINUX_NR_MUNMAP, LinuxErrno};

/// Minimal sysdeps status used while porting musl to x86_64-unknown-none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SysdepsBootstrapStatus {
    pub startup_hook_ready: bool,
    pub memory_hooks_ready: bool,
    pub clock_hooks_ready: bool,
}

impl SysdepsBootstrapStatus {
    pub const fn in_progress() -> Self {
        Self {
            startup_hook_ready: true,
            memory_hooks_ready: true,
            clock_hooks_ready: false,
        }
    }
}

/// Stable syscall numbers expected by the shim for memory bootstrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySyscallNumbers {
    pub brk: usize,
    pub mmap: usize,
    pub munmap: usize,
    pub mprotect: usize,
}

pub const fn memory_syscall_numbers() -> MemorySyscallNumbers {
    MemorySyscallNumbers {
        brk: LINUX_NR_BRK,
        mmap: LINUX_NR_MMAP,
        munmap: LINUX_NR_MUNMAP,
        mprotect: LINUX_NR_MPROTECT,
    }
}

/// Placeholder startup hook for crt0/`__libc_start_main` integration wiring.
pub fn startup_hook() -> Result<(), LinuxErrno> {
    Ok(())
}

/// Placeholder clock hook for early musl bring-up.
/// Returns `ENOSYS` until timer service integration is wired.
pub fn clock_gettime_hook() -> Result<u64, LinuxErrno> {
    Err(LinuxErrno::NoSys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_syscall_numbers_match_linux_compat_contract() {
        let nums = memory_syscall_numbers();
        assert_eq!(nums.brk, LINUX_NR_BRK);
        assert_eq!(nums.mmap, LINUX_NR_MMAP);
        assert_eq!(nums.munmap, LINUX_NR_MUNMAP);
        assert_eq!(nums.mprotect, LINUX_NR_MPROTECT);
    }

    #[test]
    fn startup_hook_is_ready_for_wiring() {
        assert_eq!(startup_hook(), Ok(()));
    }

    #[test]
    fn clock_hook_reports_enosys_until_timer_service_is_plumbed() {
        assert_eq!(clock_gettime_hook(), Err(LinuxErrno::NoSys));
    }

    #[test]
    fn status_tracks_minimal_sysdeps_progress() {
        let status = SysdepsBootstrapStatus::in_progress();
        assert!(status.startup_hook_ready);
        assert!(status.memory_hooks_ready);
        assert!(!status.clock_hooks_ready);
    }
}
