use crate::kernel::task::ArchSwitchContext;

#[inline]
pub fn switch_frames(_prev: &mut ArchSwitchContext, _next: &ArchSwitchContext) {}
