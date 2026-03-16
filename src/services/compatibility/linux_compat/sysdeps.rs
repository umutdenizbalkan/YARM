use super::{LINUX_NR_BRK, LINUX_NR_MMAP, LINUX_NR_MPROTECT, LINUX_NR_MUNMAP, LinuxErrno};
use crate::kernel::bootstrap::KernelState;
use crate::kernel::capabilities::CapId;
use crate::kernel::vm::PAGE_SIZE;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Minimal sysdeps status used while porting musl to x86_64-unknown-none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SysdepsBootstrapStatus {
    pub startup_hook_ready: bool,
    pub memory_hooks_ready: bool,
    pub clock_hooks_ready: bool,
    pub thread_hooks_ready: bool,
    pub futex_hooks_ready: bool,
}

impl SysdepsBootstrapStatus {
    pub const fn in_progress() -> Self {
        Self {
            startup_hook_ready: true,
            memory_hooks_ready: true,
            clock_hooks_ready: true,
            thread_hooks_ready: true,
            futex_hooks_ready: true,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupBootstrapInfo {
    pub stack_top: usize,
    pub argv_ptr: usize,
    pub envp_ptr: usize,
    pub auxv_ptr: usize,
}

static NEXT_TID: AtomicU64 = AtomicU64::new(1000);
static CLOCK_TICKS_NS: AtomicU64 = AtomicU64::new(0);
static LAST_TLS_TID: AtomicU64 = AtomicU64::new(0);
static LAST_TLS_BASE: AtomicUsize = AtomicUsize::new(0);
static FUTEX_ADDR: AtomicUsize = AtomicUsize::new(0);
static FUTEX_WAITERS: AtomicUsize = AtomicUsize::new(0);

/// Placeholder startup hook for crt0/`__libc_start_main` integration wiring.
pub fn startup_hook(info: StartupBootstrapInfo) -> Result<StartupBootstrapInfo, LinuxErrno> {
    if info.stack_top == 0 {
        return Err(LinuxErrno::Inval);
    }
    Ok(info)
}

pub fn mmap_hook(
    kernel: &mut KernelState,
    aspace_cap: CapId,
    addr: usize,
    len: usize,
    prot: usize,
) -> Result<usize, LinuxErrno> {
    kernel
        .linux_mmap_region(aspace_cap, addr, len, prot)
        .map_err(Into::into)
}

pub fn munmap_hook(
    kernel: &mut KernelState,
    aspace_cap: CapId,
    addr: usize,
    len: usize,
) -> Result<(), LinuxErrno> {
    kernel
        .linux_munmap_region(aspace_cap, addr, len)
        .map_err(Into::into)
}

pub fn mprotect_hook(
    kernel: &mut KernelState,
    aspace_cap: CapId,
    addr: usize,
    len: usize,
    prot: usize,
) -> Result<(), LinuxErrno> {
    kernel
        .linux_mprotect_region(aspace_cap, addr, len, prot)
        .map_err(Into::into)
}

pub fn brk_hook(
    kernel: &mut KernelState,
    tid: u64,
    aspace_cap: CapId,
    requested: usize,
    prot: usize,
) -> Result<usize, LinuxErrno> {
    kernel
        .linux_brk(tid, aspace_cap, requested, prot)
        .map_err(Into::into)
}

/// Monotonic bootstrap clock used before timer-service plumbing lands.
pub fn clock_gettime_hook() -> Result<u64, LinuxErrno> {
    Ok(CLOCK_TICKS_NS.fetch_add(1_000_000, Ordering::Relaxed) + 1_000_000)
}

pub fn nanosleep_hook(nanos: u64) -> Result<(), LinuxErrno> {
    if nanos == 0 {
        return Ok(());
    }
    CLOCK_TICKS_NS.fetch_add(nanos, Ordering::Relaxed);
    Ok(())
}

pub fn clone_thread_hook(parent_tid: u64) -> Result<u64, LinuxErrno> {
    if parent_tid == 0 {
        return Err(LinuxErrno::Inval);
    }
    Ok(NEXT_TID.fetch_add(1, Ordering::Relaxed))
}

pub fn set_tls_hook(tid: u64, tls_base: usize) -> Result<(), LinuxErrno> {
    if tid == 0 || tls_base == 0 {
        return Err(LinuxErrno::Inval);
    }
    LAST_TLS_TID.store(tid, Ordering::Relaxed);
    LAST_TLS_BASE.store(tls_base, Ordering::Relaxed);
    Ok(())
}

pub fn get_tls_hook(tid: u64) -> Result<Option<usize>, LinuxErrno> {
    if tid == LAST_TLS_TID.load(Ordering::Relaxed) {
        Ok(Some(LAST_TLS_BASE.load(Ordering::Relaxed)))
    } else {
        Ok(None)
    }
}

pub fn futex_wait_hook(addr: usize, expected: u32, observed: u32) -> Result<bool, LinuxErrno> {
    if addr == 0 {
        return Err(LinuxErrno::Inval);
    }
    if observed != expected {
        return Ok(false);
    }
    FUTEX_ADDR.store(addr, Ordering::Relaxed);
    FUTEX_WAITERS.fetch_add(1, Ordering::Relaxed);
    Ok(true)
}

pub fn futex_wake_hook(addr: usize, max_wake: u32) -> Result<u32, LinuxErrno> {
    if addr == 0 {
        return Err(LinuxErrno::Inval);
    }
    if max_wake == 0 || FUTEX_ADDR.load(Ordering::Relaxed) != addr {
        return Ok(0);
    }
    let cur = FUTEX_WAITERS.load(Ordering::Relaxed) as u32;
    let woken = cur.min(max_wake);
    FUTEX_WAITERS.fetch_sub(woken as usize, Ordering::Relaxed);
    if FUTEX_WAITERS.load(Ordering::Relaxed) == 0 {
        FUTEX_ADDR.store(0, Ordering::Relaxed);
    }
    Ok(woken)
}

pub const fn default_mmap_len() -> usize {
    PAGE_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::bootstrap::Bootstrap;

    #[test]
    fn memory_syscall_numbers_match_linux_compat_contract() {
        let nums = memory_syscall_numbers();
        assert_eq!(nums.brk, LINUX_NR_BRK);
        assert_eq!(nums.mmap, LINUX_NR_MMAP);
        assert_eq!(nums.munmap, LINUX_NR_MUNMAP);
        assert_eq!(nums.mprotect, LINUX_NR_MPROTECT);
    }

    #[test]
    fn startup_hook_validates_nonzero_stack_top() {
        let ok = startup_hook(StartupBootstrapInfo {
            stack_top: 0x1000,
            argv_ptr: 1,
            envp_ptr: 2,
            auxv_ptr: 3,
        })
        .expect("startup");
        assert_eq!(ok.stack_top, 0x1000);
        assert_eq!(
            startup_hook(StartupBootstrapInfo {
                stack_top: 0,
                argv_ptr: 1,
                envp_ptr: 2,
                auxv_ptr: 3,
            }),
            Err(LinuxErrno::Inval)
        );
    }

    #[test]
    fn clock_and_sleep_hooks_are_callable() {
        let before = clock_gettime_hook().expect("clock before");
        nanosleep_hook(1_000_000).expect("sleep");
        let after = clock_gettime_hook().expect("clock after");
        assert!(after >= before);
    }

    #[test]
    fn thread_tls_and_futex_hooks_have_stable_semantics() {
        let tid = clone_thread_hook(7).expect("clone");
        assert!(tid >= 1000);
        set_tls_hook(tid, 0xDEAD_BEEF).expect("set tls");
        assert_eq!(get_tls_hook(tid).expect("get tls"), Some(0xDEAD_BEEF));

        assert_eq!(futex_wait_hook(0x1000, 3, 4).expect("mismatch"), false);
        assert_eq!(futex_wait_hook(0x1000, 3, 3).expect("wait"), true);
        assert_eq!(futex_wake_hook(0x1000, 1).expect("wake"), 1);
        assert_eq!(futex_wake_hook(0x1000, 1).expect("wake empty"), 0);
    }

    #[test]
    fn memory_hooks_route_into_kernel_vm_helpers() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_cap) = state.create_user_address_space().expect("aspace");

        let addr = mmap_hook(
            &mut state,
            aspace_cap,
            0x8000,
            default_mmap_len(),
            super::super::PROT_READ | super::super::PROT_WRITE,
        )
        .expect("mmap");
        assert_eq!(addr, 0x8000);

        mprotect_hook(
            &mut state,
            aspace_cap,
            0x8000,
            default_mmap_len(),
            super::super::PROT_READ,
        )
        .expect("mprotect");

        munmap_hook(&mut state, aspace_cap, 0x8000, default_mmap_len()).expect("munmap");
    }

    #[test]
    fn brk_hook_routes_into_kernel_brk_helper() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_cap) = state.create_user_address_space().expect("aspace");

        let grown = brk_hook(
            &mut state,
            0,
            aspace_cap,
            0x4000_0000 + PAGE_SIZE,
            super::super::PROT_READ | super::super::PROT_WRITE,
        )
        .expect("grow");
        assert_eq!(grown, 0x4000_0000 + PAGE_SIZE);
    }

    #[test]
    fn status_tracks_bootstrap_progress() {
        let status = SysdepsBootstrapStatus::in_progress();
        assert!(status.startup_hook_ready);
        assert!(status.memory_hooks_ready);
        assert!(status.clock_hooks_ready);
        assert!(status.thread_hooks_ready);
        assert!(status.futex_hooks_ready);
    }
}
