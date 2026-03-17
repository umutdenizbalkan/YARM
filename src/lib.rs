#![no_std]

#[cfg(feature = "hosted-dev")]
pub extern crate std;

pub mod arch;
pub mod kernel;
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
        }
    }};
}
