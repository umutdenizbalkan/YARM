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
#[path = "services/common/mod.rs"]
pub mod service_common;
#[path = "services/compatibility/mod.rs"]
pub mod compatibility;
#[path = "services/init/mod.rs"]
pub mod init;
#[path = "../crates/yarm-control-plane-servers/src/control_plane/mod.rs"]
pub mod yarm_control_plane_servers;
#[path = "../crates/yarm-driver-servers/src/drivers/mod.rs"]
pub mod yarm_driver_servers;
#[path = "../crates/yarm-fs-servers/src/fs/mod.rs"]
pub mod yarm_fs_servers;
#[path = "../crates/yarm-network-servers/src/network/mod.rs"]
pub mod yarm_network_servers;
#[path = "../crates/yarm-ui-servers/src/ui/mod.rs"]
pub mod yarm_ui_servers;

#[cfg(feature = "posix-compat")]
pub use compatibility::posix_compat;

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
