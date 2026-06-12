// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Hosted-only DTB inspection command for comparing QEMU virt and Raspberry Pi
//! 5 device trees before bare-metal platform integration. The kernel DTB parser
//! remains separate; this tool does not imply Raspberry Pi 5 boot support.

use std::path::PathBuf;
use yarm_runtime_tools::dtb_probe::{parse_fdt, render_report, render_yarm_readiness};

fn main() {
    if let Err(error) = run() {
        eprintln!("dtb_probe: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os();
    let program = PathBuf::from(args.next().unwrap_or_else(|| "dtb_probe".into()));
    let usage = || format!("usage: {} [--yarm-readiness] <file.dtb>", program.display());
    let mut readiness = false;
    let mut path = None;
    for argument in args {
        if argument == "--yarm-readiness" {
            if readiness {
                return Err(usage().into());
            }
            readiness = true;
        } else if path.is_none() {
            path = Some(PathBuf::from(argument));
        } else {
            return Err(usage().into());
        }
    }
    let path = path.ok_or_else(usage)?;
    let bytes =
        std::fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let parsed = parse_fdt(&bytes)?;
    if readiness {
        print!("{}", render_yarm_readiness(&parsed));
    } else {
        print!("{}", render_report(&parsed));
    }
    Ok(())
}
