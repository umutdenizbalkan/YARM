#!/usr/bin/env python3
"""IRQ-AUTH1 baseline-reproducer tree: <wt at 41403e1> + a test-only hook between the
notification resolve and the route write in bind_irq_notification + the baseline tests.
Usage: ia1_baseline_tree.py <wt> <tests.rs fragment>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); frag = pathlib.Path(sys.argv[2]).read_text()
p = wt / "src/kernel/boot/ipc_state.rs"; s = p.read_text()
old = """        let notif_idx = self.resolve_notification_index(capability.object)?;
        let irq_idx = irq_line as usize;"""
assert s.count(old) == 1
s = s.replace(old, """        let notif_idx = self.resolve_notification_index(capability.object)?;
        #[cfg(test)]
        irq_bind_phase_hook::run_after_resolve(self);
        let irq_idx = irq_line as usize;""")
s = s.rstrip("\n") + """

#[cfg(test)]
pub(crate) mod irq_bind_phase_hook {
    extern crate std;
    use core::cell::RefCell;
    type Hook = alloc::boxed::Box<dyn FnMut(&mut crate::kernel::boot::KernelState)>;
    std::thread_local! { static AFTER_RESOLVE: RefCell<Option<Hook>> = const { RefCell::new(None) }; }
    pub(crate) fn arm(hook: Hook) { AFTER_RESOLVE.with(|h| *h.borrow_mut() = Some(hook)); }
    pub(crate) fn run_after_resolve(state: &mut crate::kernel::boot::KernelState) {
        let hook = AFTER_RESOLVE.with(|h| h.borrow_mut().take());
        if let Some(mut hook) = hook { hook(state); }
    }
}
"""
p.write_text(s)
t = wt / "src/kernel/boot/tests.rs"
t.write_text(t.read_text().rstrip("\n") + "\n" + frag)
print("baseline tree built")
