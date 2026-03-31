use crate::kernel::scheduler::CpuId;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn register_cpu_mapping(cpu: CpuId) {
    super::x86_64::descriptor_tables::register_apic_cpu_mapping(cpu.0, cpu);
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub fn register_cpu_mapping(_cpu: CpuId) {}
