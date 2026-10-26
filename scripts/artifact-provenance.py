#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""BUILD-GATES1 — artifact provenance: the ONE implementation that records what a kernel or
initramfs is, and that verifies a boot set before QEMU starts.

    artifact-provenance.py features   --feature-args ARGS
    artifact-provenance.py record-kernel    --arch A --elf ELF [--boot BIN] --feature-args ARGS
                                            --target T --profile P --nofp-report R --built-by S
    artifact-provenance.py record-initramfs --arch A --cpio CPIO --feature-args ARGS
                                            --target T --profile P --built-by S
    artifact-provenance.py verify  --arch A --kernel K --initramfs I [--require REQ ...]
    artifact-provenance.py show    ARTIFACT

Every published artifact has a sidecar `<artifact>.provenance` (`key=value` lines) written by the
command that produced it, from the arguments that command actually used:

  * role, arch, Cargo target and profile, the exact Cargo feature arguments, the default-feature
    setting and the requested features, and the features Cargo RESOLVES from them (`cargo tree`,
    never guessed);
  * the source it was built from: commit, tree and a source state (the tree when the tracked
    files are clean, otherwise a digest of `git diff HEAD`);
  * its SHA-256 (and, for a raw boot image, the ELF it was converted from);
  * for a kernel, the no-FP result of `scripts/check-kernel-nofp.sh` on that exact ELF (and boot
    image): the checker's own `NOFP_CHECK` line, bound to the image hash, and the report's hash.

`verify` refuses — non-zero, before any boot — unless the kernel and initramfs are exactly the
recorded artifacts (hash), for this arch, built from the current source state, the kernel's
recorded no-FP check passed on this very ELF, and every feature requirement holds:

    kernel=FEATS / initramfs=FEATS    the requested feature set is exactly FEATS (`none` for none)
    kernel:FEAT  / initramfs:FEAT     FEAT is among the requested features

Nothing here rewrites a record to fit an image: a mismatch is reported and refused, and the only
way to obtain a valid record is to build (or rebuild) the artifact.
"""

import argparse
import hashlib
import os
import re
import subprocess
import sys

REPO = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
SUFFIX = ".provenance"
VERSION = "1"
# Compile-time environment read with `option_env!` — build configuration that is not a Cargo
# feature — per artifact role: the kernel crate (`src/`) reads none; the server crates packed into
# the initramfs (`crates/`) read these. A hosted test pins both lists against every `option_env!`
# in the tree.
COMPILE_ENV = {
    "kernel": (),
    "initramfs": ("SUPERVISOR_RESTART_TEST", "YARM_SUPERVISOR_RESTART_TEST"),
}


def compile_env(role):
    """`NAME=VALUE;…` for the role's compile-time inputs set in the building command's env."""
    got = [f"{n}={os.environ[n]}" for n in COMPILE_ENV[role] if os.environ.get(n)]
    return ";".join(got) or "none"


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def feature_record(args):
    """(default_features, features, all) from Cargo feature arguments, as Cargo reads them."""
    toks = args.split()
    default, every, feats, i = 1, False, [], 0
    while i < len(toks):
        t = toks[i]
        if t == "--no-default-features":
            default = 0
        elif t == "--all-features":
            every = True
        elif t in ("--features", "-F") and i + 1 < len(toks):
            feats += toks[i + 1].split(",")
            i += 1
        elif t.startswith("--features="):
            feats += t[len("--features="):].split(",")
        i += 1
    feats = sorted({f.split("/")[-1] for f in feats if f})
    return default, ("all" if every else (",".join(feats) or "none"))


def resolved_features(args, package="yarm"):
    """The package's enabled features as Cargo resolves them for these arguments."""
    cmd = ["cargo", "tree", "-q", "-p", package, "-e", "normal", "--depth", "0", "-f", "{f}"]
    cmd += args.split()
    r = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit(f"[provenance] cannot resolve features with `{' '.join(cmd)}`: "
                         f"{r.stderr.strip()[:300]}")
    lines = [l.strip() for l in r.stdout.splitlines() if l.strip()]
    return lines[0] if lines else "none"


def git(*args):
    r = subprocess.run(["git", *args], cwd=REPO, capture_output=True, text=True)
    return r.stdout.strip() if r.returncode == 0 else ""


def source_state():
    commit = git("rev-parse", "HEAD") or "unknown"
    tree = git("rev-parse", "HEAD^{tree}") or "unknown"
    dirty = [l for l in git("status", "--porcelain", "--untracked-files=no").splitlines() if l]
    if not dirty:
        state = f"clean:{tree}"
    else:
        diff = subprocess.run(["git", "diff", "HEAD"], cwd=REPO, capture_output=True).stdout
        state = f"dirty:{tree}:{hashlib.sha256(diff).hexdigest()[:24]}"
    return dict(commit=commit, tree=tree, tracked_dirty=str(len(dirty)), source_state=state)


def write_record(path, fields):
    tmp = f"{path}{SUFFIX}.tmp.{os.getpid()}"
    with open(tmp, "w") as f:
        for k, v in fields.items():
            v = str(v).replace("\n", " ")
            f.write(f"{k}={v}\n")
    os.replace(tmp, path + SUFFIX)


def read_record(path):
    rec = {}
    with open(path + SUFFIX) as f:
        for line in f:
            line = line.rstrip("\n")
            if "=" in line:
                k, v = line.split("=", 1)
                rec[k] = v
    return rec


NOFP_LINE = re.compile(r"^NOFP_CHECK .*$", re.M)


def nofp_from_report(report):
    with open(report) as f:
        text = f.read()
    lines = NOFP_LINE.findall(text)
    if len(lines) != 1:
        raise SystemExit(f"[provenance] {report} holds {len(lines)} NOFP_CHECK result lines")
    line = lines[0]
    field = lambda k: (re.search(rf"\b{k}=(\S+)", line) or [None, ""])[1]
    return dict(nofp_result=field("result"), nofp_image_sha256=field("image_sha256"),
                nofp_obligations=field("obligations"), nofp_line=line,
                nofp_report=report, nofp_report_sha256=sha256(report))


def common(arch, role, feature_args, target, profile, built_by):
    default, feats = feature_record(feature_args)
    return dict(yarm_provenance=VERSION, role=role, arch=arch, target=target, profile=profile,
                cargo_feature_args=" ".join(feature_args.split()), default_features=default,
                features=feats, compile_env=compile_env(role), built_by=built_by, **source_state())


def cmd_record_kernel(a):
    base = common(a.arch, "kernel", a.feature_args, a.target, a.profile, a.built_by)
    base["resolved_features"] = resolved_features(a.feature_args)
    nofp = nofp_from_report(a.nofp_report)
    elf_sha = sha256(a.elf)
    if nofp["nofp_image_sha256"] != elf_sha:
        raise SystemExit(f"[provenance] the no-FP report checked {nofp['nofp_image_sha256']}, "
                         f"not this ELF {elf_sha}")
    rel = lambda p, art: os.path.relpath(os.path.abspath(p), os.path.dirname(os.path.abspath(art)))
    elf_rec = dict(base, artifact=os.path.basename(a.elf), format="elf", sha256=elf_sha,
                   elf=rel(a.elf, a.elf), elf_sha256=elf_sha, **nofp)
    elf_rec["nofp_report"] = rel(a.nofp_report, a.elf)
    write_record(a.elf, elf_rec)
    if a.boot:
        boot_rec = dict(elf_rec, artifact=os.path.basename(a.boot), format="raw",
                        sha256=sha256(a.boot), elf=rel(a.elf, a.boot),
                        nofp_report=rel(a.nofp_report, a.boot))
        write_record(a.boot, boot_rec)
    return 0


def cmd_record_initramfs(a):
    rec = common(a.arch, "initramfs", a.feature_args, a.target, a.profile, a.built_by)
    rec.update(artifact=os.path.basename(a.cpio), format="cpio", sha256=sha256(a.cpio))
    write_record(a.cpio, rec)
    return 0


def verify(arch, kernel, initramfs, requires):
    """Returns (problems, kernel record, initramfs record)."""
    problems = []
    now = source_state()["source_state"]
    recs = {}
    for role, path in (("kernel", kernel), ("initramfs", initramfs)):
        if not os.path.isfile(path):
            problems.append(f"{role} {path} does not exist")
            continue
        if not os.path.isfile(path + SUFFIX):
            problems.append(f"{role} {path} has no provenance record ({path}{SUFFIX}): it was not "
                            f"published by a provenance-recording build, so what it is is unknown")
            continue
        rec = read_record(path)
        recs[role] = rec
        if rec.get("yarm_provenance") != VERSION:
            problems.append(f"{role} record {path}{SUFFIX} is malformed or of another version")
            continue
        if rec.get("role") != role:
            problems.append(f"{path} is recorded as a {rec.get('role') or '?'}, not a {role}")
        if rec.get("arch") != arch:
            problems.append(f"{role} {path} was built for {rec.get('arch') or '?'}, not {arch}")
        actual = sha256(path)
        if rec.get("sha256") != actual:
            problems.append(f"{role} {path} is not the recorded artifact (sha256 {actual}, "
                            f"recorded {rec.get('sha256')}): it was replaced or rebuilt after "
                            f"its record was written")
        if rec.get("source_state") != now:
            problems.append(f"{role} {path} was built from source {rec.get('source_state')}, the "
                            f"checkout is {now}: a cached artifact from another source")
        if role == "kernel":
            here = os.path.dirname(os.path.abspath(path))
            elf = os.path.join(here, rec.get("elf", "")) if rec.get("elf") else ""
            if rec.get("nofp_result") != "pass":
                problems.append(f"kernel {path} has no passing no-FP check "
                                f"(nofp_result={rec.get('nofp_result') or 'missing'})")
            elif not os.path.isfile(elf) or sha256(elf) != rec.get("elf_sha256"):
                problems.append(f"kernel ELF {elf or '?'} the no-FP check judged is missing or "
                                f"changed")
            elif rec.get("nofp_image_sha256") != rec.get("elf_sha256"):
                problems.append(f"the recorded no-FP check judged {rec.get('nofp_image_sha256')}, "
                                f"not this kernel's ELF {rec.get('elf_sha256')}")
            else:
                rpt = os.path.join(here, rec["nofp_report"]) if rec.get("nofp_report") else ""
                if not os.path.isfile(rpt) or sha256(rpt) != rec.get("nofp_report_sha256"):
                    problems.append(f"the no-FP report {rpt or '?'} is missing or changed")
                elif rec.get("nofp_line", "") not in open(rpt).read():
                    problems.append(f"the no-FP report {rpt} does not hold the recorded result")
                else:
                    # The check judged the image under the features its build resolved to (it
                    # expands the requested ones through the crate's feature graph, as Cargo
                    # does). A record whose Cargo-resolved features differ from the set the check
                    # judged, or whose selected features are not what its own feature arguments
                    # select, has been relabeled.
                    m = re.search(r" features=(\S+) ", rec.get("nofp_line", ""))
                    judged = m.group(1) if m else "?"
                    if judged == "-":
                        judged = "none"
                    if judged != rec.get("resolved_features"):
                        problems.append(f"kernel {path} is recorded with resolved features="
                                        f"{rec.get('resolved_features')}, but its no-FP check "
                                        f"judged features={judged}")
        try:
            default, selected = feature_record(rec.get("cargo_feature_args", ""))
        except Exception:  # noqa: BLE001 — an unreadable record is a refusal, not a crash
            default, selected = "?", "?"
        if role == "kernel" and selected not in ("?", "none", "all"):
            resolved = set(rec.get("resolved_features", "").split(","))
            missing = sorted(set(selected.split(",")) - resolved)
            if missing:
                problems.append(f"kernel {path} selects {','.join(missing)}, which is not among "
                                f"its Cargo-resolved features {rec.get('resolved_features')}")
        if (str(default), selected) != (rec.get("default_features"), rec.get("features")):
            problems.append(f"{role} {path} is recorded with default_features="
                            f"{rec.get('default_features')} features={rec.get('features')},"
                            f" but its feature arguments `{rec.get('cargo_feature_args')}`"
                            f" select default_features={default} features={selected}")
    env_want = {"kernel": [], "initramfs": []}
    exact = set()
    for req in requires:
        m = re.fullmatch(r"(kernel|initramfs)@([A-Z0-9_]+=[A-Za-z0-9_.-]+)", req)
        if m:
            env_want[m.group(1)].append(m.group(2))
    for req in requires:
        if re.fullmatch(r"(kernel|initramfs)@([A-Z0-9_]+=[A-Za-z0-9_.-]+)", req):
            continue
        m = re.fullmatch(r"(kernel|initramfs)([=:])([A-Za-z0-9_,.-]+)", req)
        if not m:
            problems.append(f"malformed requirement `{req}`")
            continue
        role, op, want = m.groups()
        rec = recs.get(role)
        if rec is None:
            continue
        if op == "=":
            exact.add(role)
        have = rec.get("features", "")
        if op == "=":
            want_set = "none" if want == "none" else ",".join(sorted(set(want.split(","))))
            if have != want_set:
                problems.append(f"this run requires the {role} built with features={want_set}; it "
                                f"was built with features={have or '?'} "
                                f"({rec.get('cargo_feature_args') or '?'})")
        elif have != "all" and want not in have.split(","):
            problems.append(f"this run requires the {role} built with `{want}`; it was built "
                            f"with features={have or '?'}")
    # Compile-time environment: `ROLE@NAME=V` requires it; an exact `ROLE=FEATS` requirement also
    # means "and no compile-time input beyond the ones required".
    for role in ("kernel", "initramfs"):
        rec = recs.get(role)
        if rec is None or (role not in exact and not env_want[role]):
            continue
        have = rec.get("compile_env", "")
        have_set = set() if have == "none" else set(filter(None, have.split(";")))
        if not have and (env_want[role] or role in exact):
            problems.append(f"{role} record has no compile_env field")
            continue
        for w in env_want[role]:
            if w not in have_set:
                problems.append(f"this run requires the {role} built with {w}; it was built with "
                                f"compile_env={have}")
        if role in exact:
            extra = sorted(have_set - set(env_want[role]))
            if extra:
                problems.append(f"this run requires the {role} built exactly as specified; it was "
                                f"also built with {';'.join(extra)}")
    return problems, recs.get("kernel", {}), recs.get("initramfs", {})


def cmd_verify(a):
    problems, k, i = verify(a.arch, a.kernel, a.initramfs, a.require)
    result = "refused" if problems else "ok"
    print(f"ARTIFACT_BINDING arch={a.arch} kernel={a.kernel} kernel_sha256={k.get('sha256', 'none')} "
          f"kernel_features={k.get('features', 'none')} "
          f"kernel_resolved_features={k.get('resolved_features', 'none')} "
          f"kernel_nofp={k.get('nofp_result', 'none')} initramfs={a.initramfs} "
          f"initramfs_sha256={i.get('sha256', 'none')} initramfs_features={i.get('features', 'none')} "
          f"initramfs_compile_env={i.get('compile_env', 'none')} "
          f"source={k.get('source_state', 'none')} required={','.join(a.require) or 'none'} "
          f"result={result}")
    for p in problems:
        print(f"[error] ARTIFACT-BINDING: {p}")
    if problems:
        print("[error] ARTIFACT-BINDING: refusing to boot: the artifacts cannot be bound to a "
              "checked, recorded build of this source")
        return 1
    return 0


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("features")
    f.add_argument("--feature-args", default="")
    rk = sub.add_parser("record-kernel")
    for k in ("--arch", "--elf", "--feature-args", "--target", "--profile", "--nofp-report",
              "--built-by"):
        rk.add_argument(k, required=True)
    rk.add_argument("--boot")
    ri = sub.add_parser("record-initramfs")
    for k in ("--arch", "--cpio", "--feature-args", "--target", "--profile", "--built-by"):
        ri.add_argument(k, required=True)
    v = sub.add_parser("verify")
    for k in ("--arch", "--kernel", "--initramfs"):
        v.add_argument(k, required=True)
    v.add_argument("--require", action="append", default=[])
    s = sub.add_parser("show")
    s.add_argument("artifact")
    a = ap.parse_args()
    if a.cmd == "features":
        d, feats = feature_record(a.feature_args)
        print(f"default_features={d} features={feats}")
        return 0
    if a.cmd == "record-kernel":
        return cmd_record_kernel(a)
    if a.cmd == "record-initramfs":
        return cmd_record_initramfs(a)
    if a.cmd == "verify":
        return cmd_verify(a)
    if a.cmd == "show":
        for k, val in read_record(a.artifact).items():
            print(f"{k}={val}")
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(main())
