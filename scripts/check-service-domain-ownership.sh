#!/usr/bin/env bash
set -euo pipefail

bad=0

# network should not depend directly on fs internals
if rg -n "crate::services::fs::|yarm::services::fs::" src/services/network >/dev/null; then
  echo "[fail] network domain must not depend on fs domain internals"
  rg -n "crate::services::fs::|yarm::services::fs::" src/services/network
  bad=1
fi

# ui should not depend directly on fs/network internals
if rg -n "crate::services::(fs|network)::|yarm::services::(fs|network)::" src/services/ui >/dev/null; then
  echo "[fail] ui domain must not depend on fs/network domain internals"
  rg -n "crate::services::(fs|network)::|yarm::services::(fs|network)::" src/services/ui
  bad=1
fi

# control-plane should not depend on fs/network/ui/compatibility internals
if rg -n "crate::services::(fs|network|ui|compatibility)::|yarm::services::(fs|network|ui|compatibility)::" src/services/control_plane >/dev/null; then
  echo "[fail] control_plane domain must not depend on fs/network/ui/compatibility domain internals"
  rg -n "crate::services::(fs|network|ui|compatibility)::|yarm::services::(fs|network|ui|compatibility)::" src/services/control_plane
  bad=1
fi

# compatibility should not depend directly on fs/network/ui internals
if rg -n "crate::services::(fs|network|ui)::|yarm::services::(fs|network|ui)::" src/services/compatibility >/dev/null; then
  echo "[fail] compatibility domain must not depend on fs/network/ui domain internals"
  rg -n "crate::services::(fs|network|ui)::|yarm::services::(fs|network|ui)::" src/services/compatibility
  bad=1
fi

# drivers should not depend directly on fs internals
if rg -n "crate::services::fs::|yarm::services::fs::" src/services/drivers >/dev/null; then
  echo "[fail] drivers domain must not depend on fs domain internals"
  rg -n "crate::services::fs::|yarm::services::fs::" src/services/drivers
  bad=1
fi

if [[ "$bad" -ne 0 ]]; then
  exit 1
fi

echo "[ok] service domain ownership checks passed"
