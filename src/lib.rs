// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

extern crate alloc;
extern crate self as yarm;

#[cfg(all(feature = "hosted-dev", target_os = "none"))]
compile_error!(
    "feature `hosted-dev` cannot be enabled for bare-metal targets (target_os=\"none\"); build with --no-default-features for x86_64-yarm-none"
);

#[cfg(all(feature = "hosted-dev", not(target_os = "none")))]
pub extern crate std;

pub mod arch;
pub mod kernel;
pub mod runtime;

#[macro_export]
macro_rules! yarm_log {
    ($($arg:tt)*) => {{
        #[cfg(all(feature = "hosted-dev", not(target_os = "none")))]
        {
            $crate::std::println!($($arg)*);
        }
        #[cfg(not(all(feature = "hosted-dev", not(target_os = "none"))))]
        {
            $crate::pr_info!($($arg)*);
            let _ = $crate::kernel::printk::printk_flush();
        }
    }};
}
