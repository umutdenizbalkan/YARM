// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use yarm_fs_servers::fs::ext4::fs::{Ext4Image, Ext4ImageError};

#[test]
#[ignore = "development-only: requires mke2fs, debugfs, e2fsck, and tune2fs"]
fn real_indexed_directories_support_uuid_and_stored_checksum_seeds() {
    let tools = ["mke2fs", "debugfs", "e2fsck", "tune2fs"];
    if tools.iter().any(|tool| !tool_available(tool)) {
        eprintln!("skipping ext4 real-image probe; required e2fsprogs tools are unavailable");
        return;
    }

    for stored_seed in [false, true] {
        run_probe(stored_seed);
    }
    run_meta_bg_rejection_probe();
}

fn run_probe(stored_seed: bool) {
    let base = unique_path(if stored_seed { "seed" } else { "uuid" });
    let image = base.with_extension("img");
    let payload = base.with_extension("payload");
    let commands = base.with_extension("debugfs");
    let cleanup = Cleanup([image.clone(), payload.clone(), commands.clone()]);

    fs::write(&payload, b"indexed real-image payload\n").expect("write probe payload");
    let file = fs::File::create(&image).expect("create probe image");
    file.set_len(32 * 1024 * 1024).expect("size probe image");
    run(
        Command::new("mke2fs")
            .args(["-q", "-t", "ext4", "-F", "-O", "^orphan_file"])
            .arg(&image),
        "mke2fs",
    );

    let mut script = String::from("mkdir /indexed\n");
    for index in 0..600 {
        script.push_str(&format!(
            "write {} /indexed/file{index:04}.bin\n",
            payload.display()
        ));
    }
    fs::write(&commands, script).expect("write debugfs command file");
    run(
        Command::new("debugfs")
            .arg("-w")
            .arg("-f")
            .arg(&commands)
            .arg(&image),
        "debugfs",
    );
    run(
        Command::new("e2fsck").args(["-f", "-y", "-D"]).arg(&image),
        "e2fsck -D",
    );
    if stored_seed {
        run(
            Command::new("tune2fs")
                .args(["-O", "metadata_csum_seed"])
                .arg(&image),
            "tune2fs metadata_csum_seed",
        );
    }

    let bytes = fs::read(&image).expect("read generated ext4 image");
    let ext4 = Ext4Image::mount(&bytes).expect("mount generated ext4 image");
    assert_eq!(
        ext4.read_file(b"/indexed/file0427.bin").unwrap(),
        b"indexed real-image payload\n"
    );
    let entries = ext4
        .read_dir(b"/indexed")
        .expect("enumerate indexed directory");
    assert_eq!(entries.len(), 602);
    assert!(entries.iter().any(|entry| entry.name() == b"file0000.bin"));
    assert!(entries.iter().any(|entry| entry.name() == b"file0599.bin"));
    for (index, entry) in entries.iter().enumerate() {
        assert!(
            !entries[..index]
                .iter()
                .any(|prior| prior.inode == entry.inode && prior.name() == entry.name())
        );
    }

    drop(cleanup);
}

fn run_meta_bg_rejection_probe() {
    let image = unique_path("meta-bg").with_extension("img");
    let cleanup = FileCleanup(image.clone());
    let file = fs::File::create(&image).expect("create meta_bg probe image");
    file.set_len(64 * 1024 * 1024)
        .expect("size meta_bg probe image");
    run(
        Command::new("mke2fs")
            .args([
                "-q",
                "-t",
                "ext4",
                "-F",
                "-O",
                "meta_bg,^resize_inode,^orphan_file",
            ])
            .arg(&image),
        "mke2fs meta_bg",
    );

    let bytes = fs::read(&image).expect("read generated meta_bg image");
    assert!(matches!(
        Ext4Image::mount(&bytes),
        Err(Ext4ImageError::UnsupportedFeature(0x0010))
    ));

    drop(cleanup);
}

fn tool_available(tool: &str) -> bool {
    Command::new(tool).arg("-V").output().is_ok()
}

fn run(command: &mut Command, label: &str) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("run {label}: {error}"));
    assert!(
        output.status.success(),
        "{label} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn unique_path(profile: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "yarm-ext4-htree-{profile}-{}-{nanos}",
        std::process::id()
    ))
}

struct Cleanup([PathBuf; 3]);
struct FileCleanup(PathBuf);

impl Drop for FileCleanup {
    fn drop(&mut self) {
        remove_if_present(&self.0);
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            remove_if_present(path);
        }
    }
}

fn remove_if_present(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!("failed to remove {}: {error}", path.display()),
    }
}
