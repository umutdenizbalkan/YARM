#[cfg(feature = "hosted-dev")]
pub fn write_line(_msg: &str) {}

#[cfg(not(feature = "hosted-dev"))]
pub fn write_line(_msg: &str) {}
