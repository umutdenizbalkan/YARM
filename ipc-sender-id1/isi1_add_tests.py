#!/usr/bin/env python3
"""IPC-SENDER-ID1: add the permanent regressions. Usage: isi1_add_tests.py <wt> <scratch>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); sc = pathlib.Path(sys.argv[2])
T = wt / "src/kernel/boot/tests.rs"; s = T.read_text()
# the shared-region child module goes at the end of `mod shared_region {`
a = s.index("        mod shared_region {\n")
i = s.index("{", a); depth = 0
while True:
    c = s[i]
    if c == "{": depth += 1
    elif c == "}":
        depth -= 1
        if depth == 0: break
    i += 1
# i is the closing brace of mod shared_region (brace counting is safe here: the module's string
# literals and comments are balanced, which the compiler check below confirms)
line_start = s.rfind("\n", 0, i) + 1
s = s[:line_start] + (sc / "isi1_tests_sr.rs").read_text().lstrip("\n") + s[line_start:]
s = s.rstrip("\n") + "\n" + (sc / "isi1_tests.rs").read_text()
T.write_text(s)
print("tests added")
