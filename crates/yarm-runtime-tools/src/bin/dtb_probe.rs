// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Hosted-only DTB inspection command for comparing QEMU virt and Raspberry Pi
//! 5 device trees before bare-metal platform integration. The kernel DTB parser
//! remains separate; this tool does not imply Raspberry Pi 5 boot support.

use std::path::PathBuf;
use yarm_runtime_tools::dtb_probe::{parse_fdt, render_report};

fn main() {
    if let Err(error) = run() {
        eprintln!("dtb_probe: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os();
    let program = args.next().unwrap_or_else(|| "dtb_probe".into());
    let Some(path) = args.next() else {
        return Err(format!("usage: {} <file.dtb>", PathBuf::from(program).display()).into());
    };
    if args.next().is_some() {
        return Err(format!("usage: {} <file.dtb>", PathBuf::from(program).display()).into());
    }
    let path = PathBuf::from(path);
    let bytes =
        std::fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let parsed = parse_fdt(&bytes)?;
    print!("{}", render_report(&parsed));
    Ok(())
}
