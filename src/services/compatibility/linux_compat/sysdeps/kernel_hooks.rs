use crate::kernel::boot::KernelState;
use crate::kernel::capabilities::CapId;
use crate::kernel::vm::PAGE_SIZE;
use crate::services::compatibility::linux_compat::LinuxErrno;

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

pub fn clone_thread_hook(
    kernel: &mut KernelState,
    parent_tid: u64,
    tls_base: usize,
    user_stack_top: usize,
    user_entry: usize,
) -> Result<u64, LinuxErrno> {
    kernel
        .spawn_user_thread(parent_tid, tls_base, user_stack_top, user_entry)
        .map_err(Into::into)
}

pub fn set_tls_hook(kernel: &mut KernelState, tid: u64, tls_base: usize) -> Result<(), LinuxErrno> {
    kernel
        .set_thread_tls_base(tid, tls_base)
        .map_err(Into::into)
}

pub fn get_tls_hook(kernel: &KernelState, tid: u64) -> Result<Option<usize>, LinuxErrno> {
    if tid == 0 {
        return Err(LinuxErrno::Inval);
    }
    Ok(kernel.thread_tls_base(tid))
}

pub fn futex_wait_hook(
    kernel: &mut KernelState,
    addr: usize,
    expected: u32,
    observed: u32,
) -> Result<bool, LinuxErrno> {
    kernel
        .futex_wait_current(addr, expected, observed)
        .map_err(LinuxErrno::from)
}

pub fn futex_wake_hook(
    kernel: &mut KernelState,
    addr: usize,
    max_wake: u32,
) -> Result<u32, LinuxErrno> {
    kernel.futex_wake(addr, max_wake).map_err(LinuxErrno::from)
}

pub const fn default_mmap_len() -> usize {
    PAGE_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::task::TaskClass;

    #[test]
    fn thread_tls_and_futex_hooks_have_stable_semantics() {
        let mut kernel = crate::kernel::boot::Bootstrap::init().expect("init");
        let (asid, _aspace_cap) = kernel.create_user_address_space().expect("asid");
        kernel
            .spawn_user_task_from_image(crate::kernel::boot::UserImageSpec {
                tid: 7,
                entry: 0x4000,
                asid: Some(asid),
                class: TaskClass::App,
            })
            .expect("parent");
        let tid =
            clone_thread_hook(&mut kernel, 7, 0xDEAD_BEEF, 0x8000_0000, 0x4010).expect("clone");
        assert!(tid >= 10_000);
        set_tls_hook(&mut kernel, tid, 0xFEED_CAFE).expect("set tls");
        assert_eq!(
            get_tls_hook(&kernel, tid).expect("get tls"),
            Some(0xFEED_CAFE)
        );
        assert!(!futex_wait_hook(&mut kernel, 0x1000, 3, 4).expect("mismatch"));
        assert!(futex_wait_hook(&mut kernel, 0x1000, 3, 3).expect("wait"));
        assert_eq!(futex_wake_hook(&mut kernel, 0x1000, 1).expect("wake"), 1);
        assert_eq!(
            futex_wake_hook(&mut kernel, 0x1000, 1).expect("wake empty"),
            0
        );
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
            super::super::super::PROT_READ | super::super::super::PROT_WRITE,
        )
        .expect("mmap");
        assert_eq!(addr, 0x8000);
        mprotect_hook(
            &mut state,
            aspace_cap,
            0x8000,
            default_mmap_len(),
            super::super::super::PROT_READ,
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
            super::super::super::PROT_READ | super::super::super::PROT_WRITE,
        )
        .expect("grow");
        assert_eq!(grown, 0x4000_0000 + PAGE_SIZE);
    }
}
