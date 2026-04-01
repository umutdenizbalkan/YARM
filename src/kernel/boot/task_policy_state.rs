use super::{KernelError, KernelState};
use crate::kernel::capabilities::CNodeId;
use crate::kernel::ipc::ThreadId;
use crate::kernel::task::{TaskClass, ThreadControlBlock};

impl KernelState {
    pub(crate) fn register_task_with_class_in_process(
        &mut self,
        tid: u64,
        class: TaskClass,
        process_pid: u64,
    ) -> Result<(), KernelError> {
        if self.task_status(tid).is_some() {
            return Ok(());
        }
        let limits = self.runtime_capacity_config();
        if self.with_tcbs(|tcbs| tcbs.iter().flatten().count()) >= limits.max_tasks {
            return Err(KernelError::TaskTableFull);
        }
        let cnode = self
            .process_cnode_for_pid(process_pid)
            .unwrap_or(CNodeId(process_pid));
        self.ensure_cnode_space(cnode)?;
        self.set_process_cnode_for_pid(process_pid, cnode)?;
        let inserted = self.with_tcbs_mut(|tcbs| {
            if let Some(idx) = tcbs.iter().position(|slot| slot.is_none()) {
                let tcb = ThreadControlBlock::new(ThreadId(tid), class, None);
                tcbs[idx] = Some(tcb);
                true
            } else {
                false
            }
        });
        if !inserted {
            return Err(KernelError::TaskTableFull);
        }
        self.provision_default_kernel_context(tid)?;
        Ok(())
    }

    pub fn register_task_with_class(
        &mut self,
        tid: u64,
        class: TaskClass,
    ) -> Result<(), KernelError> {
        self.register_task_with_class_in_process(tid, class, tid)
    }

    pub fn register_task(&mut self, tid: u64) -> Result<(), KernelError> {
        self.register_task_with_class(tid, TaskClass::App)
    }

    pub fn allocate_thread_id(&mut self) -> Result<u64, KernelError> {
        let limits = self.runtime_capacity_config();
        let mut candidate = self.next_dynamic_tid;
        for _ in 0..limits.max_tasks.saturating_mul(4) {
            self.next_dynamic_tid = self.next_dynamic_tid.saturating_add(1);
            if self.task_status(candidate).is_none() {
                return Ok(candidate);
            }
            candidate = self.next_dynamic_tid;
        }
        Err(KernelError::TaskTableFull)
    }
}
