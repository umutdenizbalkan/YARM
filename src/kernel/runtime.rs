use super::bootstrap::{KernelError, KernelState};
use super::lock::SpinLock;
#[cfg(test)]
use super::lock::SpinLockGuard;
use super::scheduler::CpuId;

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

    #[cfg(test)]
    pub fn lock(&self) -> SpinLockGuard<'_, KernelState> {
        self.state.lock()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut KernelState) -> R) -> R {
        let mut guard = self.state.lock();
        f(&mut guard)
    }

    pub fn with_cpu<R>(
        &self,
        cpu: CpuId,
        f: impl FnOnce(&mut KernelState) -> R,
    ) -> Result<R, KernelError> {
        let mut guard = self.state.lock();
        guard.set_current_cpu(cpu)?;
        Ok(f(&mut guard))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::kernel::bootstrap::Bootstrap;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::scheduler::CpuId;
    use crate::kernel::smp::WorkItem;
    use std::sync::Arc;
    use std::thread;

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

    #[test]
    fn with_cpu_applies_targeted_cross_cpu_work_before_closure() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state.bring_up_cpu(CpuId(1)).expect("cpu1");
            state.register_task(2).expect("task2");
            state
                .submit_cross_cpu_work(WorkItem::WakeTask {
                    target_cpu: CpuId(1),
                    tid: ThreadId(2),
                })
                .expect("submit");
        });

        let processed = kernel
            .with_cpu(CpuId(1), |state| {
                assert_eq!(state.current_cpu(), CpuId(1));
                state
                    .process_cross_cpu_work_for_cpu(CpuId(1))
                    .expect("drain")
            })
            .expect("with_cpu");
        assert_eq!(processed, 1);
    }

    #[test]
    fn with_cpu_propagates_invalid_cpu_errors() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let result = kernel.with_cpu(CpuId(1), |_| 0);
        assert!(result.is_err());
    }

    #[test]
    fn shared_kernel_allows_concurrent_serialized_access() {
        let kernel: Arc<SharedKernel> =
            Arc::new(SharedKernel::new(Bootstrap::init().expect("init")));
        let k1 = Arc::clone(&kernel);
        let k2 = Arc::clone(&kernel);

        let t1 = thread::spawn(move || {
            for _ in 0..32 {
                k1.with(|state| {
                    state
                        .submit_cross_cpu_work(WorkItem::Reschedule {
                            target_cpu: CpuId(0),
                        })
                        .expect("submit t1");
                });
            }
        });

        let t2 = thread::spawn(move || {
            for _ in 0..32 {
                k2.with(|state| {
                    state
                        .submit_cross_cpu_work(WorkItem::Reschedule {
                            target_cpu: CpuId(0),
                        })
                        .expect("submit t2");
                });
            }
        });

        t1.join().expect("join t1");
        t2.join().expect("join t2");

        let drained =
            kernel.with(|state| state.process_cross_cpu_work_for_cpu(CpuId(1)).unwrap_or(0));
        assert_eq!(drained, 0);

        let drained_cpu0 = kernel.with(|state| {
            state
                .process_cross_cpu_work_for_cpu(CpuId(0))
                .expect("drain cpu0")
        });
        assert_eq!(drained_cpu0, 64);
    }
}
