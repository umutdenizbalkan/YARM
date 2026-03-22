use super::{KernelError, KernelState, TaskMemByte};
use crate::kernel::ipc::{Message, ThreadId};
use crate::kernel::vm::{VirtAddr, VmError};

impl KernelState {
    pub fn write_user_memory(
        &mut self,
        tid: u64,
        ptr: usize,
        data: &[u8],
    ) -> Result<(), KernelError> {
        let _ = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;

        let mut i = 0;
        while i < data.len() {
            let va = ptr + i;
            self.validate_user_access_for_tid(tid, va, true)?;

            let mut found = false;
            for slot in &mut self.memory.task_mem {
                if slot
                    .as_ref()
                    .is_some_and(|entry| entry.tid == ThreadId(tid) && entry.addr == va)
                {
                    slot.as_mut().expect("checked").value = data[i];
                    found = true;
                    break;
                }
            }

            if !found {
                let slot = self
                    .memory
                    .task_mem
                    .iter_mut()
                    .find(|slot| slot.is_none())
                    .ok_or(KernelError::TaskTableFull)?;
                *slot = Some(TaskMemByte {
                    tid: ThreadId(tid),
                    addr: va,
                    value: data[i],
                });
            }
            i += 1;
        }

        Ok(())
    }

    pub fn read_user_memory(
        &self,
        tid: u64,
        ptr: usize,
        len: usize,
    ) -> Result<[u8; Message::MAX_PAYLOAD], KernelError> {
        if len > Message::MAX_PAYLOAD {
            return Err(KernelError::UserMemoryFault);
        }

        let mut out = [0u8; Message::MAX_PAYLOAD];
        let mut i = 0;
        while i < len {
            let va = ptr + i;
            self.validate_user_access_for_tid(tid, va, false)?;
            let value = self
                .memory
                .task_mem
                .iter()
                .flatten()
                .find(|entry| entry.tid == ThreadId(tid) && entry.addr == va)
                .map(|entry| entry.value)
                .ok_or(KernelError::UserMemoryFault)?;
            out[i] = value;
            i += 1;
        }

        Ok(out)
    }

    fn validate_user_access_for_tid(
        &self,
        tid: u64,
        va: usize,
        need_write: bool,
    ) -> Result<(), KernelError> {
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
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
        self.write_user_memory(tid, user_ptr, bytes)
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
        self.read_user_memory(tid, user_ptr, len)
    }
}
