// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// Selected ISA syscall ABI-shape re-exports used by kernel mechanism code.

/// Cross-architecture minimum inline IPC register payload lane count.
///
/// The baseline is currently set by the most restrictive supported syscall
/// calling convention (x86_64) so generic kernel IPC code can rely on a
/// portable floor without per-ISA conditionals.
pub const IPC_REGISTER_WORDS_MIN: usize = 2;

pub use super::selected_isa::syscall_abi::*;

const _: () = assert!(IPC_REGISTER_WORDS >= IPC_REGISTER_WORDS_MIN);
