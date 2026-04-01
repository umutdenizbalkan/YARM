// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

#[cfg(feature = "hosted-dev")]
pub extern crate std;

pub mod arch;
pub mod kernel;
pub mod runtime;
pub mod services;

#[cfg(feature = "linux-compat")]
pub use services::compatibility::linux_compat;

#[macro_export]
macro_rules! yarm_log {
    ($($arg:tt)*) => {{
        #[cfg(feature = "hosted-dev")]
        {
            $crate::std::println!($($arg)*);
        }
        #[cfg(not(feature = "hosted-dev"))]
        {
            $crate::pr_info!($($arg)*);
            let _ = $crate::kernel::printk::printk_flush();
        }
    }};
}
