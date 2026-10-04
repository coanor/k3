#!/usr/bin/env bash
# Use the shipped bootstrap to stage and verify all components before replacing files.
set -euo pipefail
root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo=${K3_RELEASE_REPO:-coanor/k3}
if [[ -z ${K3_RELEASE_REPO:-} && -x "$root/runtime/python/bin/python3" && -f "$root/scripts/installation_state.py" ]]; then
    repo=$("$root/runtime/python/bin/python3" -I -B "$root/scripts/installation_state.py" --prefix "$root")
fi
exec bash "$root/install.sh" --prefix "$root" --repo "$repo" --update "$@"
