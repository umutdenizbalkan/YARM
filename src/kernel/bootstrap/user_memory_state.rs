use super::{KernelError, KernelState};
use crate::kernel::ipc::Message;
use crate::kernel::vm::{Asid, VirtAddr, VmError};

impl KernelState {
    #[cfg(feature = "hosted-dev")]
    fn write_user_byte(&mut self, asid: Asid, va: VirtAddr, value: u8) {
        self.memory.user_memory.insert((asid.0, va.0), value);
    }

    #[cfg(not(feature = "hosted-dev"))]
    fn write_user_byte(&mut self, _asid: Asid, _va: VirtAddr, _value: u8) {}

    #[cfg(feature = "hosted-dev")]
    fn read_user_byte(&self, asid: Asid, va: VirtAddr) -> Option<u8> {
        self.memory.user_memory.get(&(asid.0, va.0)).copied()
    }

    #[cfg(not(feature = "hosted-dev"))]
    fn read_user_byte(&self, _asid: Asid, _va: VirtAddr) -> Option<u8> {
        None
    }

    pub fn copy_to_user(
        &mut self,
        asid: Asid,
        va: VirtAddr,
        bytes: &[u8],
    ) -> Result<(), KernelError> {
        for (i, &byte) in bytes.iter().enumerate() {
            let addr = va.0 as usize + i;
            self.validate_user_access_for_asid(asid, addr, true)?;
            self.write_user_byte(asid, VirtAddr(addr as u64), byte);
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
            self.validate_user_access_for_asid(asid, addr, false)?;
            *slot = self
                .read_user_byte(asid, VirtAddr(addr as u64))
                .ok_or(KernelError::UserMemoryFault)?;
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

    fn validate_user_access_for_asid(
        &self,
        asid: Asid,
        va: usize,
        need_write: bool,
    ) -> Result<(), KernelError> {
        let aspace = self
            .user_spaces
            .get(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let page_base = va & !(crate::kernel::vm::PAGE_SIZE - 1);
        let mapping = aspace
            .resolve(VirtAddr(page_base as u64))
            .ok_or(KernelError::UserMemoryFault)?;
        if !mapping.flags.user || !mapping.flags.read || (need_write && !mapping.flags.write) {
            return Err(KernelError::UserMemoryFault);
        }
        Ok(())
    }

    pub fn copy_to_current_user(
        &mut self,
        user_ptr: usize,
        bytes: &[u8],
    ) -> Result<(), KernelError> {
        let tid = self
            .scheduler
            .current_tid()
            .ok_or(KernelError::TaskMissing)?;
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        self.copy_to_user(asid, VirtAddr(user_ptr as u64), bytes)
    }

    pub fn copy_from_current_user(
        &self,
        user_ptr: usize,
        len: usize,
    ) -> Result<[u8; Message::MAX_PAYLOAD], KernelError> {
        let tid = self
            .scheduler
            .current_tid()
            .ok_or(KernelError::TaskMissing)?;
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        self.copy_from_user(asid, VirtAddr(user_ptr as u64), len)
    }
}
