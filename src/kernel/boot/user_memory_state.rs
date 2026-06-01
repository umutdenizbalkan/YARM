// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState};
use crate::kernel::ipc::Message;
use crate::kernel::vm::{Asid, VirtAddr, VmError};

impl KernelState {
    #[cfg(feature = "hosted-dev")]
    fn write_user_byte(&mut self, asid: Asid, va: VirtAddr, value: u8) -> Result<(), KernelError> {
        self.with_memory_state_mut(|memory| {
            memory.user_memory.insert((asid.0, va.0), value);
        });
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
        self.with_memory_state(|memory| {
            memory
                .user_memory
                .get(&(asid.0, va.0))
                .copied()
                .ok_or(KernelError::UserMemoryFault)
        })
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
        if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
            crate::yarm_log!(
                "COPY_TO_USER asid={} va=0x{:x} len={}",
                asid.0,
                va.0,
                bytes.len()
            );
        }
        let mut last_page_base: Option<usize> = None;
        for (i, &byte) in bytes.iter().enumerate() {
            let addr = va.0 as usize + i;
            let page_base = addr & !(crate::kernel::vm::PAGE_SIZE - 1usize);
            if last_page_base != Some(page_base) {
                let pte_present = crate::arch::selected_isa::page_table::resolve_page(
                    asid,
                    VirtAddr(page_base as u64),
                )
                .is_some();
                if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
                    crate::yarm_log!(
                        "COPY_TO_USER_PAGE asid={} page_va=0x{:x} pte_present={} offset={}",
                        asid.0,
                        page_base,
                        pte_present,
                        i
                    );
                }
                last_page_base = Some(page_base);
            }
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

    #[cfg(test)]
    pub fn read_user_memory_for_asid(
        &self,
        asid: Asid,
        ptr: usize,
        len: usize,
    ) -> Result<[u8; Message::MAX_PAYLOAD], KernelError> {
        self.copy_from_user(asid, VirtAddr(ptr as u64), len)
    }

    #[cfg(test)]
    pub fn write_user_memory_for_asid(
        &mut self,
        asid: Asid,
        ptr: usize,
        data: &[u8],
    ) -> Result<(), KernelError> {
        self.copy_to_user(asid, VirtAddr(ptr as u64), data)
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
        let page_base = va & !(crate::kernel::vm::PAGE_SIZE - 1usize);
        let page_off = (va - page_base) as u64;
        self.with_user_spaces(|spaces| {
            let aspace = spaces
                .get(asid)
                .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
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
        })
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
        if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
            crate::yarm_log!(
                "VALIDATE asid={} va=0x{:x} need_write={}",
                asid.0,
                va,
                need_write
            );
        }
        let page_base = va & !(crate::kernel::vm::PAGE_SIZE - 1usize);
        let page_off = (va - page_base) as u64;
        let (user_space_exists, shadow_mapping_present) = self.with_user_spaces(|spaces| {
            let exists = spaces.get(asid).is_some();
            let shadow = spaces
                .get(asid)
                .and_then(|aspace| aspace.resolve(VirtAddr(page_base as u64)))
                .is_some();
            (exists, shadow)
        });
        if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
            crate::yarm_log!("ASID_EXISTS={}", user_space_exists);
        }
        if !user_space_exists {
            return Err(KernelError::Vm(VmError::InvalidAsid));
        }
        let pte_result =
            crate::arch::selected_isa::page_table::resolve_page(asid, VirtAddr(page_base as u64));
        if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
            crate::yarm_log!(
                "VALIDATE_PAGE asid={} page_va=0x{:x} shadow_present={} resolve_ok={}",
                asid.0,
                page_base,
                shadow_mapping_present,
                pte_result.is_some()
            );
        }
        let pte = pte_result.ok_or(KernelError::UserMemoryFault)?;
        if !Self::pte_allows_user_access(pte, need_write) {
            if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
                crate::yarm_log!(
                    "VALIDATE_PERM_FAIL asid={} page_va=0x{:x} pte=0x{:x}",
                    asid.0,
                    page_base,
                    pte.0
                );
            }
            return Err(KernelError::UserMemoryFault);
        }
        let resolved_phys = pte
            .addr()
            .checked_add(page_off)
            .ok_or(KernelError::UserMemoryFault)?;
        if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
            crate::yarm_log!(
                "VALIDATE_OK asid={} page_va=0x{:x} page_off=0x{:x} phys=0x{:x}",
                asid.0,
                page_base,
                page_off,
                resolved_phys
            );
        }
        Ok(resolved_phys)
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

    /// Copy a kernel slice into an arbitrary task's virtual address space.
    ///
    /// Uses one page-table walk per 4 KiB page boundary crossed, then a bulk
    /// copy for each aligned chunk — avoids per-byte overhead for large buffers.
    ///
    /// Phase 2B temporary bridge: used by initramfs_srv to copy CPIO data into
    /// PM's transfer buffer.  Replace with page-cap grant in Phase 3.
    pub fn copy_slice_to_task(
        &mut self,
        target_tid: u64,
        user_ptr: usize,
        src: &[u8],
    ) -> Result<(), KernelError> {
        let asid = self.task_asid(target_tid).ok_or(KernelError::UserMemoryFault)?;
        let page_size = crate::kernel::vm::PAGE_SIZE;
        let len = src.len();
        let mut done = 0usize;
        while done < len {
            let va_addr = user_ptr + done;
            let offset_in_page = va_addr & (page_size - 1);
            let bytes_in_page = page_size - offset_in_page;
            let chunk = (len - done).min(bytes_in_page);
            let phys = self.validate_user_access_for_asid(asid, va_addr, true)?;
            #[cfg(not(feature = "hosted-dev"))]
            {
                let dst_ptr =
                    Self::phys_to_direct_map_ptr(phys).ok_or(KernelError::UserMemoryFault)?;
                // SAFETY: phys validated above; chunk ≤ remaining bytes in that page.
                unsafe {
                    core::ptr::copy_nonoverlapping(src[done..].as_ptr(), dst_ptr, chunk);
                }
            }
            #[cfg(feature = "hosted-dev")]
            {
                for i in 0..chunk {
                    self.write_user_byte(asid, VirtAddr(phys + i as u64), src[done + i])?;
                }
            }
            done += chunk;
        }
        Ok(())
    }

    /// Copy a kernel slice into the current user task's virtual address space.
    ///
    /// Uses one page-table walk per 4 KiB page boundary crossed, then a bulk
    /// copy for each aligned chunk — avoids per-byte `validate_user_access_for_asid`
    /// overhead for large buffers (e.g. ELF images).
    pub fn copy_to_current_user_from_slice(
        &mut self,
        user_ptr: usize,
        src: &[u8],
    ) -> Result<(), KernelError> {
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        let page_size = crate::kernel::vm::PAGE_SIZE;
        let len = src.len();
        let mut done = 0usize;
        while done < len {
            let va_addr = user_ptr + done;
            let offset_in_page = va_addr & (page_size - 1);
            let bytes_in_page = page_size - offset_in_page;
            let chunk = (len - done).min(bytes_in_page);
            // One page-table walk per page boundary.
            let phys = self.validate_user_access_for_asid(asid, va_addr, true)?;
            #[cfg(not(feature = "hosted-dev"))]
            {
                let dst_ptr =
                    Self::phys_to_direct_map_ptr(phys).ok_or(KernelError::UserMemoryFault)?;
                // SAFETY: phys validated above; chunk ≤ remaining bytes in that page;
                // src slice has at least `len` bytes.
                unsafe {
                    core::ptr::copy_nonoverlapping(src[done..].as_ptr(), dst_ptr, chunk);
                }
            }
            #[cfg(feature = "hosted-dev")]
            {
                for i in 0..chunk {
                    self.write_user_byte(asid, VirtAddr(phys + i as u64), src[done + i])?;
                }
            }
            done += chunk;
        }
        Ok(())
    }

    /// Copy an arbitrary-length slice from the current user task's virtual address space.
    ///
    /// Uses one page-table walk per 4 KiB page boundary crossed, then a bulk
    /// copy for each aligned chunk — avoids per-byte `validate_user_access_for_asid`
    /// overhead for large buffers (e.g. ELF images).
    pub fn copy_from_current_user_into_slice(
        &self,
        user_ptr: usize,
        len: usize,
        out: &mut [u8],
    ) -> Result<(), KernelError> {
        if out.len() < len {
            return Err(KernelError::UserMemoryFault);
        }
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        let page_size = crate::kernel::vm::PAGE_SIZE;
        let mut done = 0usize;
        while done < len {
            let va_addr = user_ptr + done;
            let offset_in_page = va_addr & (page_size - 1);
            let bytes_in_page = page_size - offset_in_page;
            let chunk = (len - done).min(bytes_in_page);
            // One page-table walk per page boundary.
            let phys = self.validate_user_access_for_asid(asid, va_addr, false)?;
            #[cfg(not(feature = "hosted-dev"))]
            {
                let src_ptr =
                    Self::phys_to_direct_map_ptr(phys).ok_or(KernelError::UserMemoryFault)?;
                // SAFETY: phys validated above; chunk ≤ remaining bytes in that page;
                // out slice has at least `len` bytes (checked above).
                unsafe {
                    core::ptr::copy_nonoverlapping(src_ptr, out[done..].as_mut_ptr(), chunk);
                }
            }
            #[cfg(feature = "hosted-dev")]
            {
                for i in 0..chunk {
                    out[done + i] = self.read_user_byte(asid, VirtAddr(phys + i as u64))?;
                }
            }
            done += chunk;
        }
        Ok(())
    }
}
