// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(any(test, feature = "hosted-dev"))]
#[allow(dead_code)]
pub(crate) mod restart_abi_review;
pub mod service;

pub use service::run;
