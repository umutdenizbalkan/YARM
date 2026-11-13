"""FUTEX-TIME1: the RISC-V drain's typed result lane (fifth part)."""
import pathlib, sys
p=pathlib.Path(sys.argv[1]) / "src/kernel/boot/tests.rs"; s=p.read_text()
old='''            tail.contains("frame.set_ok(0, 0, 0);") && tail.contains("frame.set_err("),'''
new='''            // FUTEX-TIME1 re-derivation: a success still CLEARS the error lane — `set_ok` with the
            // class's success `ret0` (`0` for a send, `1` for a timed FutexWait).
            tail.contains("frame.set_ok(done.success_ret0(), 0, 0);")
                && tail.contains("frame.set_err("),'''
assert s.count(old)==1, s.count(old)
s=s.replace(old,new); p.write_text(s); print("rederived5")
