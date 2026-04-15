// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState};
use crate::kernel::ipc::Message;
use crate::kernel::vm::{Asid, VirtAddr, VmError};

impl KernelState {
    #[cfg(feature = "hosted-dev")]
    fn write_user_byte(&mut self, asid: Asid, va: VirtAddr, value: u8) -> Result<(), KernelError> {
        self.memory.user_memory.insert((asid.0, va.0), value);
        Ok(())
    }

    #[cfg(not(feature = "hosted-dev"))]
    fn write_user_byte(
        &mut self,
        _asid: Asid,
        _va: VirtAddr,
        value: u8,
    ) -> Result<(), KernelError> {
        let ptr = Self::phys_to_direct_map_ptr(_va.0).ok_or(KernelError::UserMemoryFault)?;
        unsafe {
            core::ptr::write_volatile(ptr, value);
        }
        Ok(())
    }

    #[cfg(feature = "hosted-dev")]
    fn read_user_byte(&self, asid: Asid, va: VirtAddr) -> Result<u8, KernelError> {
        self.memory
            .user_memory
            .get(&(asid.0, va.0))
            .copied()
            .ok_or(KernelError::UserMemoryFault)
    }

    #[cfg(not(feature = "hosted-dev"))]
    fn read_user_byte(&self, _asid: Asid, va: VirtAddr) -> Result<u8, KernelError> {
        let ptr = Self::phys_to_direct_map_ptr(va.0).ok_or(KernelError::UserMemoryFault)?;
        Ok(unsafe { core::ptr::read_volatile(ptr) })
    }

    #[cfg(not(feature = "hosted-dev"))]
    fn phys_to_direct_map_ptr(phys: u64) -> Option<*mut u8> {
        if phys >= crate::arch::platform_layout::KERNEL_PHYS_DIRECT_MAP_BYTES {
            return None;
        }
        #[cfg(target_arch = "x86_64")]
        {
            let virt =
                crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE.checked_add(phys)?;
            Some(virt as usize as *mut u8)
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        {
            // Early non-hosted AArch64/RISC-V bootstrap uses identity-mapped
            // lower memory in TTBR0/SATP, so physical addresses are directly
            // accessible as kernel virtual addresses in this phase.
            Some(phys as usize as *mut u8)
        }
        #[cfg(not(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )))]
        {
            let virt =
                crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE.checked_add(phys)?;
            Some(virt as usize as *mut u8)
        }
    }

    pub fn copy_to_user(
        &mut self,
        asid: Asid,
        va: VirtAddr,
        bytes: &[u8],
    ) -> Result<(), KernelError> {
        for (i, &byte) in bytes.iter().enumerate() {
            let addr = va.0 as usize + i;
            let phys = self.validate_user_access_for_asid(asid, addr, true)?;
            self.write_user_byte(asid, VirtAddr(phys), byte)?;
        }
        Ok(())
    }

    pub fn copy_from_user(
        &self,
        asid: Asid,
        va: VirtAddr,
        len: usize,
    ) -> Result<[u8; Message::MAX_PAYLOAD], KernelError> {
        if len > Message::MAX_PAYLOAD {
            return Err(KernelError::UserMemoryFault);
        }

        let mut out = [0u8; Message::MAX_PAYLOAD];
        for (i, slot) in out.iter_mut().take(len).enumerate() {
            let addr = va.0 as usize + i;
            let phys = self.validate_user_access_for_asid(asid, addr, false)?;
            *slot = self.read_user_byte(asid, VirtAddr(phys))?;
        }
        Ok(out)
    }

    pub fn write_user_memory(
        &mut self,
        tid: u64,
        ptr: usize,
        data: &[u8],
    ) -> Result<(), KernelError> {
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        self.copy_to_user(asid, VirtAddr(ptr as u64), data)
    }

    pub fn read_user_memory(
        &self,
        tid: u64,
        ptr: usize,
        len: usize,
    ) -> Result<[u8; Message::MAX_PAYLOAD], KernelError> {
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        self.copy_from_user(asid, VirtAddr(ptr as u64), len)
    }

    #[cfg(any(
        feature = "hosted-dev",
        not(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        ))
    ))]
    fn validate_user_access_for_asid(
        &self,
        asid: Asid,
        va: usize,
        need_write: bool,
    ) -> Result<u64, KernelError> {
        let aspace = self
            .user_spaces
            .get(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let page_base = va & !(crate::kernel::vm::PAGE_SIZE - 1usize);
        let page_off = (va - page_base) as u64;
        let mapping = aspace
            .resolve(VirtAddr(page_base as u64))
            .ok_or(KernelError::UserMemoryFault)?;
        if !mapping.flags.user || !mapping.flags.read || (need_write && !mapping.flags.write) {
            return Err(KernelError::UserMemoryFault);
        }
        mapping
            .phys
            .0
            .checked_add(page_off)
            .ok_or(KernelError::UserMemoryFault)
    }

    #[cfg(all(
        not(feature = "hosted-dev"),
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    ))]
    fn validate_user_access_for_asid(
        &self,
        asid: Asid,
        va: usize,
        need_write: bool,
    ) -> Result<u64, KernelError> {
        let _ = self
            .user_spaces
            .get(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let page_base = va & !(crate::kernel::vm::PAGE_SIZE - 1usize);
        let page_off = (va - page_base) as u64;
        let pte =
            crate::arch::selected_isa::page_table::resolve_page(asid, VirtAddr(page_base as u64))
                .ok_or(KernelError::UserMemoryFault)?;
        if !Self::pte_allows_user_access(pte, need_write) {
            return Err(KernelError::UserMemoryFault);
        }
        pte.addr()
            .checked_add(page_off)
            .ok_or(KernelError::UserMemoryFault)
    }

    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    fn pte_allows_user_access(
        pte: crate::arch::selected_isa::page_table::PageTableEntry,
        need_write: bool,
    ) -> bool {
        let user = (pte.0 & crate::arch::selected_isa::page_table::PageTableEntry::USER) != 0;
        let writable =
            (pte.0 & crate::arch::selected_isa::page_table::PageTableEntry::WRITABLE) != 0;
        user && (!need_write || writable)
    }

    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    fn pte_allows_user_access(
        pte: crate::arch::selected_isa::page_table::PageTableEntry,
        need_write: bool,
    ) -> bool {
        let user = (pte.0 & crate::arch::selected_isa::page_table::PageTableEntry::USER) != 0;
        let read_only =
            (pte.0 & crate::arch::selected_isa::page_table::PageTableEntry::READ_ONLY) != 0;
        user && (!need_write || !read_only)
    }

    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    fn pte_allows_user_access(
        pte: crate::arch::selected_isa::page_table::PageTableEntry,
        need_write: bool,
    ) -> bool {
        let user = (pte.0 & crate::arch::selected_isa::page_table::PageTableEntry::USER) != 0;
        let readable = (pte.0 & crate::arch::selected_isa::page_table::PageTableEntry::READ) != 0;
        let writable = (pte.0 & crate::arch::selected_isa::page_table::PageTableEntry::WRITE) != 0;
        user && readable && (!need_write || writable)
    }

    pub fn copy_to_current_user(
        &mut self,
        user_ptr: usize,
        bytes: &[u8],
    ) -> Result<(), KernelError> {
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        self.copy_to_user(asid, VirtAddr(user_ptr as u64), bytes)
    }

    pub fn copy_from_current_user(
        &self,
        user_ptr: usize,
        len: usize,
    ) -> Result<[u8; Message::MAX_PAYLOAD], KernelError> {
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        self.copy_from_user(asid, VirtAddr(user_ptr as u64), len)
    }
}
