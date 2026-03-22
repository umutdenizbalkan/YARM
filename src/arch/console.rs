pub fn write_line(msg: &str) {
    crate::arch::selected_isa::console::write_line(msg);
}
