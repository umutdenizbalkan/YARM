// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub fn write_line(msg: &str) {
    crate::arch::selected_isa::console::write_line(msg);
}
