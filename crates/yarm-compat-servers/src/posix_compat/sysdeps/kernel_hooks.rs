// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::yarm_compat_servers::PosixErrno;

#[cfg(test)]
use crate::kernel::boot::KernelState;
#[cfg(test)]
use crate::kernel::capabilities::CapId;
#[cfg(test)]
use crate::kernel::vm::PAGE_SIZE;

#[cfg(test)]
pub fn mmap_hook(
    kernel: &mut KernelState,
    aspace_cap: CapId,
    addr: usize,
    len: usize,
    prot: usize,
) -> Result<usize, PosixErrno> {
    kernel
        .posix_mmap_region(aspace_cap, addr, len, prot)
        .map_err(Into::into)
}

#[cfg(not(test))]
pub fn mmap_hook(
    _aspace_cap: u32,
    _addr: usize,
    _len: usize,
    _prot: usize,
) -> Result<usize, PosixErrno> {
    Err(PosixErrno::NoSys)
}

#[cfg(test)]
pub fn munmap_hook(
    kernel: &mut KernelState,
    aspace_cap: CapId,
    addr: usize,
    len: usize,
) -> Result<(), PosixErrno> {
    kernel
        .posix_munmap_region(aspace_cap, addr, len)
        .map_err(Into::into)
}

#[cfg(not(test))]
pub fn munmap_hook(_aspace_cap: u32, _addr: usize, _len: usize) -> Result<(), PosixErrno> {
    Err(PosixErrno::NoSys)
}

#[cfg(test)]
pub fn mprotect_hook(
    kernel: &mut KernelState,
    aspace_cap: CapId,
    addr: usize,
    len: usize,
    prot: usize,
) -> Result<(), PosixErrno> {
    kernel
        .posix_mprotect_region(aspace_cap, addr, len, prot)
        .map_err(Into::into)
}

#[cfg(not(test))]
pub fn mprotect_hook(
    _aspace_cap: u32,
    _addr: usize,
    _len: usize,
    _prot: usize,
) -> Result<(), PosixErrno> {
    Err(PosixErrno::NoSys)
}

#[cfg(test)]
pub fn brk_hook(
    kernel: &mut KernelState,
    tid: u64,
    aspace_cap: CapId,
    requested: usize,
    prot: usize,
) -> Result<usize, PosixErrno> {
    kernel
        .posix_brk(tid, aspace_cap, requested, prot)
        .map_err(Into::into)
}

#[cfg(not(test))]
pub fn brk_hook(
    _tid: u64,
    _aspace_cap: u32,
    _requested: usize,
    _prot: usize,
) -> Result<usize, PosixErrno> {
    Err(PosixErrno::NoSys)
}

#[cfg(test)]
pub fn clone_thread_hook(
    kernel: &mut KernelState,
    parent_tid: u64,
    tls_base: usize,
    user_stack_top: usize,
    user_entry: usize,
) -> Result<u64, PosixErrno> {
    kernel
        .spawn_user_thread(parent_tid, tls_base, user_stack_top, user_entry)
        .map_err(Into::into)
}

#[cfg(not(test))]
pub fn clone_thread_hook(
    _parent_tid: u64,
    _tls_base: usize,
    _user_stack_top: usize,
    _user_entry: usize,
) -> Result<u64, PosixErrno> {
    Err(PosixErrno::NoSys)
}

#[cfg(test)]
pub fn fork_process_hook(kernel: &mut KernelState, parent_tid: u64) -> Result<u64, PosixErrno> {
    kernel.fork_user_process_cow(parent_tid).map_err(Into::into)
}

#[cfg(not(test))]
pub fn fork_process_hook(_parent_tid: u64) -> Result<u64, PosixErrno> {
    Err(PosixErrno::NoSys)
}

#[cfg(test)]
pub fn set_tls_hook(kernel: &mut KernelState, tid: u64, tls_base: usize) -> Result<(), PosixErrno> {
    kernel
        .set_thread_tls_base(tid, tls_base)
        .map_err(Into::into)
}

#[cfg(not(test))]
pub fn set_tls_hook(_tid: u64, _tls_base: usize) -> Result<(), PosixErrno> {
    Err(PosixErrno::NoSys)
}

#[cfg(test)]
pub fn get_tls_hook(kernel: &KernelState, tid: u64) -> Result<Option<usize>, PosixErrno> {
    if tid == 0 {
        return Err(PosixErrno::Inval);
    }
    Ok(kernel.thread_tls_base(tid))
}

#[cfg(not(test))]
pub fn get_tls_hook(_tid: u64) -> Result<Option<usize>, PosixErrno> {
    Err(PosixErrno::NoSys)
}

#[cfg(test)]
pub fn futex_wait_hook(
    kernel: &mut KernelState,
    addr: usize,
    expected: u32,
    observed: u32,
) -> Result<bool, PosixErrno> {
    kernel
        .futex_wait_current(addr, expected, observed)
        .map_err(PosixErrno::from)
}

#[cfg(not(test))]
pub fn futex_wait_hook(_addr: usize, _expected: u32, _observed: u32) -> Result<bool, PosixErrno> {
    Err(PosixErrno::NoSys)
}

#[cfg(test)]
pub fn futex_wake_hook(
    kernel: &mut KernelState,
    addr: usize,
    max_wake: u32,
) -> Result<u32, PosixErrno> {
    kernel.futex_wake(addr, max_wake).map_err(PosixErrno::from)
}

#[cfg(not(test))]
pub fn futex_wake_hook(_addr: usize, _max_wake: u32) -> Result<u32, PosixErrno> {
    Err(PosixErrno::NoSys)
}

#[cfg(test)]
pub const fn default_mmap_len() -> usize {
    PAGE_SIZE
}

#[cfg(not(test))]
pub const fn default_mmap_len() -> usize {
    4096
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
                startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
            })
            .expect("parent");
        let tid =
            clone_thread_hook(&mut kernel, 7, 0xDEAD_BEEF, 0x8000_0000, 0x4010).expect("clone");
        assert!(tid >= kernel.dynamic_tid_floor());
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
