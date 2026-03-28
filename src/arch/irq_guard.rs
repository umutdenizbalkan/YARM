#[cfg(target_arch = "riscv64")]
pub type ArchIrqState = super::riscv64::irq::Riscv64IrqState;
#[cfg(target_arch = "riscv64")]
pub fn configure_external_irq_controller_from_platform_layout() {
    super::riscv64::irq::configure_plic_from_platform_layout();
}
#[cfg(target_arch = "riscv64")]
pub fn irq_save() -> ArchIrqState {
    super::riscv64::irq::irq_save()
}
#[cfg(target_arch = "riscv64")]
pub fn irq_restore(state: ArchIrqState) {
    super::riscv64::irq::irq_restore(state)
}
#[cfg(target_arch = "riscv64")]
pub fn external_irq_eoi(irq_line: u16) {
    super::riscv64::irq::external_irq_eoi(irq_line)
}

#[cfg(target_arch = "x86_64")]
pub type ArchIrqState = super::x86_64::irq::X86IrqState;
#[cfg(target_arch = "x86_64")]
pub fn configure_external_irq_controller_from_platform_layout() {
    super::x86_64::irq::configure_lapic_from_platform_layout();
}
#[cfg(target_arch = "x86_64")]
pub fn irq_save() -> ArchIrqState {
    super::x86_64::irq::irq_save()
}
#[cfg(target_arch = "x86_64")]
pub fn irq_restore(state: ArchIrqState) {
    super::x86_64::irq::irq_restore(state)
}
#[cfg(target_arch = "x86_64")]
pub fn external_irq_eoi(irq_line: u16) {
    super::x86_64::irq::external_irq_eoi(irq_line)
}

#[cfg(target_arch = "aarch64")]
pub type ArchIrqState = super::aarch64::irq::Aarch64IrqState;
#[cfg(target_arch = "aarch64")]
pub fn configure_external_irq_controller_from_platform_layout() {
    super::aarch64::irq::configure_gic_from_platform_layout();
}
#[cfg(target_arch = "aarch64")]
pub fn irq_save() -> ArchIrqState {
    super::aarch64::irq::irq_save()
}
#[cfg(target_arch = "aarch64")]
pub fn irq_restore(state: ArchIrqState) {
    super::aarch64::irq::irq_restore(state)
}
#[cfg(target_arch = "aarch64")]
pub fn external_irq_eoi(irq_line: u16) {
    super::aarch64::irq::external_irq_eoi(irq_line)
}

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "x86_64",
    target_arch = "aarch64"
)))]
compile_error!("unsupported target_arch for arch::irq_guard");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_arch_irq_facade_is_callable() {
        let configure: fn() = configure_external_irq_controller_from_platform_layout;
        let save: fn() -> ArchIrqState = irq_save;
        let restore: fn(ArchIrqState) = irq_restore;
        let eoi: fn(u16) = external_irq_eoi;
        let _ = (configure, save, restore, eoi);
    }
}
