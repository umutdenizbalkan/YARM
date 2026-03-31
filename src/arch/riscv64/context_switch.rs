use crate::kernel::task::KernelSwitchFrame;

#[inline]
pub fn switch_frames(_prev: &mut KernelSwitchFrame, _next: &KernelSwitchFrame) {}
