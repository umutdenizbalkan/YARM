// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! BL5c / BUILD-GATES1 — a QEMU run boots only artifacts it can bind to a checked build.
//!
//! Every kernel is published through one path (`artifact_publish_kernel`): the no-FP check runs on
//! the final ELF (and raw image) and only a pass publishes the bytes with a provenance record —
//! Cargo feature selection and default-feature setting, resolved features, target, profile, role,
//! source commit/tree, image hash and the check's result. Every launcher calls the guard
//! (`artifact_boot_guard`) before QEMU starts.
//!
//! These are behavioural controls: they run the REAL library, the REAL no-FP checker on synthetic
//! kernels from `tests/fixtures/nofp/mkfixture.py`, and REAL smoke scripts against a stub QEMU that
//! records whether it was ever started. Each refusal must happen before the stub runs.

use std::path::{Path, PathBuf};
use std::process::Command;

const X86_CORE: &str = include_str!("../scripts/qemu-x86_64-core-smoke.sh");
const A64_CORE: &str = include_str!("../scripts/qemu-aarch64-core-smoke.sh");
const RV_CORE: &str = include_str!("../scripts/qemu-riscv64-core-smoke.sh");

fn repo() -> &'static str {
    env!("CARGO_MANIFEST_DIR")
}

fn bash(script: &str, args: &[&str], path_prefix: Option<&Path>) -> (bool, String) {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(script).arg("binding").args(args);
    cmd.current_dir(repo());
    if let Some(p) = path_prefix {
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{path}", p.display()));
    }
    let out = cmd
        .output()
        .expect("bash must be available — the smokes themselves depend on it");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("yarm-bg1-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }
    fn s(&self) -> &str {
        self.0.to_str().unwrap()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Builds the synthetic kernels: one that passes with no features, one that passes only with
/// `x86-vmi2-witness`, and one with an ordinary `fninit` that never passes.
fn fixtures(dir: &Path) {
    let st = Command::new("python3")
        .args(["-I", "tests/fixtures/nofp/mkfixture.py", "--out"])
        .arg(dir.join("fx"))
        .args(["x86_baseline", "x86_user_vmi2_ok", "x86_ordinary_fninit"])
        .current_dir(repo())
        .status()
        .expect("python3 is required to build the fixture kernels");
    assert!(st.success(), "fixture builder failed");
}

/// Shell helpers shared by the controls: publish a fixture kernel as a build would, and record an
/// initramfs with the feature arguments its servers were built with.
const HELPERS: &str = r#"
set -uo pipefail
source scripts/lib/artifact-binding.sh
d="$1"; fx="$d/fx"
export ARTIFACT_BUILT_BY=artifact_binding_scope
pubk() { # FIXTURE DIR FEATURE_ARGS
  mkdir -p "$2"
  artifact_publish_kernel x86_64 "$fx/$1.elf" "$3" "$2/kernel_boot.elf" >"$2/publish.log" 2>&1
}
initrd() { # DIR FEATURE_ARGS
  mkdir -p "$1"
  printf 'cpio %s\n' "$2" >"$1/initramfs-core.cpio"
  artifact_record_initramfs x86_64 "$1/initramfs-core.cpio" "$2" x86_64-yarm-user-none release >/dev/null
}
guard() { # LABEL DIR [REQ...]
  local l="$1" g="$2"; shift 2
  if artifact_boot_guard x86_64 "$g/kernel_boot.elf" "$g/initramfs-core.cpio" "$@" >"$d/$l.txt" 2>&1
  then echo "VERDICT_$l=accept"; else echo "VERDICT_$l=refuse"; fi
  sed "s/^/  [$l] /" "$d/$l.txt"
}
"#;

fn verdict(text: &str, label: &str) -> String {
    let key = format!("VERDICT_{label}=");
    text.lines()
        .find_map(|l| l.strip_prefix(&key))
        .unwrap_or_else(|| panic!("no verdict {label} in:\n{text}"))
        .to_string()
}

#[test]
fn the_guard_accepts_checked_builds_and_refuses_every_unbound_set() {
    let t = Scratch::new("guard");
    fixtures(&t.0);
    let script = format!(
        "{HELPERS}{}",
        r#"
pubk x86_baseline "$d/plain" "--no-default-features"; echo "PUBLISH_plain=$?"
initrd "$d/plain" "--no-default-features"
pubk x86_user_vmi2_ok "$d/wit" "--no-default-features --features x86-vmi2-witness"; echo "PUBLISH_wit=$?"
initrd "$d/wit" "--no-default-features --features x86-vmi2-witness"
cat "$d/wit/kernel_boot.elf.provenance"
guard plain "$d/plain" kernel=none initramfs=none
guard wit "$d/wit" kernel=x86-vmi2-witness initramfs=x86-vmi2-witness
guard wit_contains "$d/wit" kernel:x86-vmi2-witness
# Wrong kernel features / wrong initramfs features.
guard wrong_kernel "$d/plain" kernel=x86-vmi2-witness
guard wrong_initramfs "$d/wit" kernel=x86-vmi2-witness initramfs=none
# A stale or swapped image: the witness kernel's bytes under the plain kernel's record.
mkdir -p "$d/swap"; cp "$d/plain/"* "$d/swap/"; cp "$d/wit/kernel_boot.elf" "$d/swap/kernel_boot.elf"
guard swapped "$d/swap"
mkdir -p "$d/stale"; cp "$d/plain/"* "$d/stale/"; printf '\0' >>"$d/stale/initramfs-core.cpio"
guard stale_initramfs "$d/stale"
# Missing metadata: the right bytes with no record, for either role.
mkdir -p "$d/nokrec"; cp "$d/plain/"* "$d/nokrec/"; rm "$d/nokrec/kernel_boot.elf.provenance"
guard missing_kernel_record "$d/nokrec"
mkdir -p "$d/noirec"; cp "$d/plain/"* "$d/noirec/"; rm "$d/noirec/initramfs-core.cpio.provenance"
guard missing_initramfs_record "$d/noirec"
# A missing or failed no-FP check, and a record relabeled after the fact.
mkdir -p "$d/norpt"; cp "$d/plain/"* "$d/norpt/"; rm "$d/norpt/kernel_boot.elf.nofp.txt"
guard missing_nofp_report "$d/norpt"
mkdir -p "$d/nofp"; cp "$d/plain/"* "$d/nofp/"; sed -i 's/^nofp_result=pass$/nofp_result=fail/' "$d/nofp/kernel_boot.elf.provenance"
guard failed_nofp_record "$d/nofp"
mkdir -p "$d/relabel"; cp "$d/plain/"* "$d/relabel/"; sed -i 's/^features=none$/features=x86-vmi2-witness/' "$d/relabel/kernel_boot.elf.provenance"
guard relabeled "$d/relabel" kernel=x86-vmi2-witness
mkdir -p "$d/oldsrc"; cp "$d/plain/"* "$d/oldsrc/"; sed -i 's/^source_state=.*/source_state=clean:0000000000000000000000000000000000000000/' "$d/oldsrc/kernel_boot.elf.provenance"
guard other_source "$d/oldsrc"
# Another architecture's guard.
artifact_boot_guard aarch64 "$d/plain/kernel_boot.elf" "$d/plain/initramfs-core.cpio" >/dev/null 2>&1 && echo VERDICT_other_arch=accept || echo VERDICT_other_arch=refuse
# A kernel that fails the no-FP check is never published, and a failed publish removes the image
# a previous publish left at the same path.
pubk x86_ordinary_fninit "$d/fp" "--no-default-features"; echo "PUBLISH_fp=$?"
[[ -e "$d/fp/kernel_boot.elf" ]] && echo FP_IMAGE=present || echo FP_IMAGE=absent
pubk x86_user_vmi2_ok "$d/nofeat" "--no-default-features"; echo "PUBLISH_nofeat=$?"
cp -r "$d/plain" "$d/over"
pubk x86_ordinary_fninit "$d/over" "--no-default-features"; echo "PUBLISH_over=$?"
[[ -e "$d/over/kernel_boot.elf" || -e "$d/over/kernel_boot.elf.provenance" ]] && echo OVER_IMAGE=present || echo OVER_IMAGE=absent
guard over "$d/over"
"#
    );
    let (_, text) = bash(&script, &[t.s()], None);

    assert!(text.contains("PUBLISH_plain=0"), "{text}");
    assert!(text.contains("PUBLISH_wit=0"), "{text}");
    for field in [
        "yarm_provenance=1\n",
        "role=kernel\n",
        "arch=x86_64\n",
        "cargo_feature_args=--no-default-features --features x86-vmi2-witness\n",
        "default_features=0\n",
        "features=x86-vmi2-witness\n",
        "resolved_features=x86-vmi2-witness\n",
        "nofp_result=pass\n",
        "compile_env=none\n",
    ] {
        assert!(text.contains(field), "record field {field:?}:\n{text}");
    }
    assert!(text.contains("\nsha256="), "{text}");
    assert!(text.contains("\ncommit="), "{text}");
    assert!(text.contains("\ntree="), "{text}");

    for ok in ["plain", "wit", "wit_contains"] {
        assert_eq!(verdict(&text, ok), "accept", "{ok}:\n{text}");
    }
    for (label, why) in [
        (
            "wrong_kernel",
            "requires the kernel built with features=x86-vmi2-witness",
        ),
        (
            "wrong_initramfs",
            "requires the initramfs built with features=none",
        ),
        ("swapped", "is not the recorded artifact"),
        ("stale_initramfs", "is not the recorded artifact"),
        ("missing_kernel_record", "has no provenance record"),
        ("missing_initramfs_record", "has no provenance record"),
        ("missing_nofp_report", "is missing or changed"),
        ("failed_nofp_record", "has no passing no-FP check"),
        ("relabeled", "but its no-FP check judged features=none"),
        ("other_source", "a cached artifact from another source"),
        ("over", "does not exist"),
    ] {
        assert_eq!(verdict(&text, label), "refuse", "{label}:\n{text}");
        assert!(
            text.contains(&format!("[{label}] [error] ARTIFACT-BINDING:"))
                && text
                    .lines()
                    .any(|l| l.starts_with(&format!("  [{label}]")) && l.contains(why)),
            "{label} must refuse because `{why}`:\n{text}"
        );
    }
    assert_eq!(verdict(&text, "other_arch"), "refuse");
    assert!(text.contains("PUBLISH_fp=1"), "{text}");
    assert!(text.contains("FP_IMAGE=absent"), "{text}");
    assert!(text.contains("PUBLISH_nofeat=1"), "{text}");
    assert!(text.contains("PUBLISH_over=1"), "{text}");
    assert!(text.contains("OVER_IMAGE=absent"), "{text}");
}

/// A stub QEMU on PATH that records every start. A positive control shows the stub is reached
/// when the guard accepts, so its absence in a refusal is meaningful.
fn stub_qemu(dir: &Path) -> PathBuf {
    let bin = dir.join("stub-bin");
    std::fs::create_dir_all(&bin).unwrap();
    for name in [
        "qemu-system-x86_64",
        "qemu-system-aarch64",
        "qemu-system-riscv64",
    ] {
        let p = bin.join(name);
        std::fs::write(
            &p,
            format!(
                "#!/usr/bin/env bash\n\
                 if [[ \"${{1:-}}\" == --version ]]; then echo 'QEMU emulator version 0.0 (stub)'; exit 0; fi\n\
                 echo \"$0 $*\" >>\"{}/qemu-started\"\nexit 0\n",
                dir.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    bin
}

#[test]
fn real_launchers_refuse_cached_and_skip_build_artifacts_before_qemu_starts() {
    let t = Scratch::new("launch");
    fixtures(&t.0);
    let bin = stub_qemu(&t.0);
    let script = format!(
        "{HELPERS}{}",
        r#"
pubk x86_baseline "$d/plain" "--no-default-features"
initrd "$d/plain" "--no-default-features"
started() { [[ -s "$d/qemu-started" ]] && echo yes || echo no; }
core() { # LABEL KERNEL INITRAMFS [ENV...]
  local l="$1" k="$2" i="$3"; shift 3
  rm -f "$d/qemu-started"
  env KERNEL_IMAGE="$k" INITRAMFS_IMAGE="$i" LOGFILE="$d/$l.boot.log" SMOKE_LOG="$d/$l.smoke.log" \
    TIMEOUT_SECS=5 QEMU_SMOKE_STRICT=0 "$@" scripts/qemu-x86_64-core-smoke.sh >"$d/$l.log" 2>&1
  echo "CORE_$l rc=$? qemu=$(started)"
  grep -h "ARTIFACT_BINDING \|ARTIFACT-BINDING" "$d/$l.log" | sed "s/^/  [$l] /"
}
# The checked default set passes the guard (the synthetic kernel then stops at the later PVH-note
# pre-flight, exit 0, so this control only shows the guard accepted it).
core ok "$d/plain/kernel_boot.elf" "$d/plain/initramfs-core.cpio"
# A cached kernel copied away from its record.
mkdir -p "$d/cached"; cp "$d/plain/kernel_boot.elf" "$d/plain/initramfs-core.cpio" "$d/cached/"
cp "$d/plain/initramfs-core.cpio.provenance" "$d/cached/"
core cached "$d/cached/kernel_boot.elf" "$d/cached/initramfs-core.cpio"
# A runner that requires a witness kernel, handed the feature-free one.
core wrong "$d/plain/kernel_boot.elf" "$d/plain/initramfs-core.cpio" ARTIFACT_REQUIRE="kernel=x86-vmi2-witness"
# A skip-build witness run whose cached build directory holds incompatible artifacts.
mkdir -p "$d/c1"; cp -r "$d/plain" "$d/c1/build"
rm -f "$d/qemu-started"
SKIP_BUILD=1 LOGDIR="$d/c1" TIMEOUT_SECS=5 scripts/qemu-context1-witness-smoke.sh x86_64 >"$d/c1.log" 2>&1
echo "C1 rc=$? qemu=$(started)"
grep -h "CONTEXT1_WITNESS_SEAL\|ARTIFACT-BINDING" "$d/c1.log" | sed 's/^/  [c1] /'
# The VMI2 witness skip-build path: refused over the feature-free kernel, and (positive control)
# reaching the stub QEMU over a checked x86-vmi2-witness build.
mkdir -p "$d/v2bad"; cp -r "$d/plain" "$d/v2bad/build"
rm -f "$d/qemu-started"
SKIP_BUILD=1 LOGDIR="$d/v2bad" TIMEOUT_SECS=5 scripts/qemu-x86_64-vmi2-witness-smoke.sh >"$d/v2bad.log" 2>&1
echo "V2BAD rc=$? qemu=$(started)"
grep -h "ARTIFACT-BINDING" "$d/v2bad.log" | sed 's/^/  [v2bad] /'
pubk x86_user_vmi2_ok "$d/v2ok/build" "--no-default-features --features x86-vmi2-witness"; echo "PUBLISH_v2ok=$?"
initrd "$d/v2ok/build" "--no-default-features"
rm -f "$d/qemu-started"
SKIP_BUILD=1 LOGDIR="$d/v2ok" TIMEOUT_SECS=5 scripts/qemu-x86_64-vmi2-witness-smoke.sh >"$d/v2ok.log" 2>&1
echo "V2OK rc=$? qemu=$(started)"
grep -h "ARTIFACT_BINDING " "$d/v2ok.log" | sed 's/^/  [v2ok] /'
"#
    );
    let (_, text) = bash(&script, &[t.s()], Some(&bin));
    assert!(text.contains("CORE_ok rc=0 qemu=no"), "{text}");
    assert!(text.contains("[ok] ARTIFACT_BINDING arch=x86_64"), "{text}");
    assert!(text.contains(" result=ok"), "{text}");
    assert!(text.contains("V2BAD rc=1 qemu=no"), "{text}");
    assert!(text.contains("[v2bad] [error] ARTIFACT-BINDING: this run requires the kernel built with features=x86-vmi2-witness"), "{text}");
    assert!(text.contains("PUBLISH_v2ok=0"), "{text}");
    assert!(
        text.lines()
            .any(|l| l.starts_with("V2OK rc=") && l.ends_with("qemu=yes")),
        "positive control: an accepted skip-build set must reach QEMU:\n{text}"
    );
    for label in ["cached", "wrong"] {
        assert!(
            text.contains(&format!("CORE_{label} rc=1 qemu=no")),
            "{label} must be refused before QEMU:\n{text}"
        );
    }
    assert!(
        text.contains("[cached] [error] ARTIFACT-BINDING:"),
        "{text}"
    );
    assert!(text.contains("has no provenance record"), "{text}");
    assert!(
        text.contains("requires the kernel built with features=x86-vmi2-witness"),
        "{text}"
    );
    assert!(text.contains("C1 rc=1 qemu=no"), "{text}");
    assert!(
        text.contains("CONTEXT1_WITNESS_SEAL arch=x86_64 result=fail reason=artifact-binding"),
        "{text}"
    );
}

fn before(src: &str, first: &str, then: &str) -> bool {
    match (src.find(first), src.find(then)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

/// Source contract: every core smoke calls the guard, unconditionally, before it boots.
#[test]
fn every_core_smoke_binds_its_artifacts_before_it_boots_them() {
    for (src, call) in [
        (
            X86_CORE,
            "artifact_binding_check x86_64 \"$QEMU_SMOKE_STRICT\" \"$KERNEL_IMAGE\" \"$INITRAMFS_IMAGE\" \"${artifact_required[@]}\" || exit 1",
        ),
        (
            A64_CORE,
            "artifact_binding_check aarch64 \"$QEMU_SMOKE_STRICT\" \"$KERNEL_IMAGE\" \"$INITRAMFS_IMAGE\" ${ARTIFACT_REQUIRE:-} || exit 1",
        ),
        (
            RV_CORE,
            "artifact_binding_check riscv64 \"$QEMU_SMOKE_STRICT\" \"$KERNEL_IMAGE\" \"$INITRAMFS_IMAGE\" ${ARTIFACT_REQUIRE:-} || exit 1",
        ),
    ] {
        assert_eq!(src.matches(call).count(), 1, "{call}");
        assert!(
            before(src, call, "-kernel \"$KERNEL_IMAGE\""),
            "bound before QEMU: {call}"
        );
    }
    assert!(X86_CORE.contains("artifact_required+=(\"initramfs:timer-contract-witness\")"));
    assert!(X86_CORE.contains("timer contract witness absent"));
}

/// Every QEMU launcher in `scripts/` reaches the guard: directly, through a core smoke
/// (`ARTIFACT_REQUIRE`), through the server-dies runner, or through a launcher that is itself
/// guarded. A new launcher must join one of these.
#[test]
fn every_qemu_launcher_is_guarded() {
    let guarded = [
        "artifact_boot_guard",
        "artifact_binding_check",
        "ARTIFACT_REQUIRE",
        "serverdies_main",
    ];
    // Launchers whose QEMU argv is built by a caller that guards first.
    let delegated = [
        "qemu-x86-deterministic.sh",
        "qemu-riscv64-uart-irq-driver.py",
        "qemu-ipc-recv-v2-oracle-smoke.sh",
        "qemu-smoke-common.sh",
    ];
    let mut unguarded = Vec::new();
    for dir in ["scripts", "scripts/lib"] {
        for e in std::fs::read_dir(Path::new(repo()).join(dir)).unwrap() {
            let p = e.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            if !(name.ends_with(".sh") || name.ends_with(".py")) || !p.is_file() {
                continue;
            }
            let src = std::fs::read_to_string(&p).unwrap_or_default();
            let boots = src.contains("-kernel \"$")
                || src.contains("\"-kernel\"")
                || src.contains("-kernel \"${");
            if !boots || name.starts_with("build-rpi5") || delegated.contains(&name.as_str()) {
                continue;
            }
            if !guarded.iter().any(|g| src.contains(g)) {
                unguarded.push(format!("{dir}/{name}"));
            }
        }
    }
    assert!(
        unguarded.is_empty(),
        "QEMU launchers without a pre-boot artifact guard: {unguarded:?}"
    );
    let rv = include_str!("../scripts/qemu-ipc-recv-v2-oracle-smoke.sh");
    assert!(rv.contains("CORE_SMOKE=\"$HERE/qemu-x86_64-core-smoke.sh\""));
}

/// The compile-time environment the recorder captures is every `option_env!` the tree reads.
#[test]
fn the_recorded_compile_environment_is_every_option_env_in_the_tree() {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let s = std::fs::read_to_string(&p).unwrap_or_default();
                for part in s.split("option_env!(\"").skip(1) {
                    out.push(part.split('"').next().unwrap().to_string());
                }
            }
        }
    }
    let mut kernel = Vec::new();
    walk(&Path::new(repo()).join("src"), &mut kernel);
    let mut servers = Vec::new();
    walk(&Path::new(repo()).join("crates"), &mut servers);
    kernel.sort();
    kernel.dedup();
    servers.sort();
    servers.dedup();
    let py = include_str!("../scripts/artifact-provenance.py");
    assert!(
        kernel.is_empty(),
        "the kernel crate reads {kernel:?}: record it"
    );
    let list = py
        .split("\"initramfs\": (")
        .nth(1)
        .unwrap()
        .split("),")
        .next()
        .unwrap();
    let recorded: Vec<String> = list
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(recorded, servers);
    assert!(py.contains("    \"kernel\": (),"));
}
