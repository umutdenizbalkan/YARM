"""FUTEX-TIME1: the fingerprint + barrier total (third part of the tests.rs re-derivations)."""
import pathlib, sys
p=pathlib.Path(sys.argv[1]) / "src/kernel/boot/tests.rs"; s=p.read_text()
old='''                    .sum::<usize>(),
            45,
            "FUTEX-FOUND1: 47 -> 45, the five futex writes became three keyed owners. \\
             U9-FUTEX-WAIT-FINAL §3 added the futex registration's exact undo, 46 -> 47 — the \\'''
new='''                    .sum::<usize>(),
            46,
            "FUTEX-TIME1: 45 -> 46, the timeout claim is a new writer. \\
             FUTEX-FOUND1: 47 -> 45, the five futex writes became three keyed owners. \\
             U9-FUTEX-WAIT-FINAL §3 added the futex registration's exact undo, 46 -> 47 — the \\'''
assert s.count(old)==1, s.count(old)
s=s.replace(old,new); p.write_text(s); print("rederived3")
