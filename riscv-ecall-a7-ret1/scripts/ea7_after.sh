#!/usr/bin/env bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
until [ -e $S/ea7_chain.done ]; do sleep 30; pgrep -f ea7_chain.sh >/dev/null || break; done
$S/ea7_addA.sh > $S/ea7_addA.out 2>&1
