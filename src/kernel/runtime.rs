use super::bootstrap::KernelState;
use super::lock::{SpinLock, SpinLockGuard};

#[derive(Debug)]
pub struct SharedKernel {
    state: SpinLock<KernelState>,
}

impl SharedKernel {
    pub const fn new(state: KernelState) -> Self {
        Self {
            state: SpinLock::new(state),
        }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, KernelState> {
        self.state.lock()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut KernelState) -> R) -> R {
        let mut guard = self.state.lock();
        f(&mut guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::bootstrap::Bootstrap;
    use crate::kernel::scheduler::CpuId;
    use crate::kernel::smp::WorkItem;

    #[test]
    fn shared_kernel_serializes_access() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));

        kernel.with(|state| {
            state
                .submit_cross_cpu_work(WorkItem::Reschedule {
                    target_cpu: CpuId(0),
                })
                .expect("submit");
        });

        let processed = kernel.with(|state| {
            state
                .process_cross_cpu_work_for_cpu(CpuId(0))
                .expect("process")
        });

        assert_eq!(processed, 1);
    }
}
