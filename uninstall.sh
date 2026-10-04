#!/usr/bin/env bash
# Uninstall only registered, unchanged installation files.
set -euo pipefail
prefix=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
yes=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix) [[ $# -ge 2 ]] || { echo 'Missing value for --prefix' >&2; exit 1; }; prefix=$2; shift 2 ;;
        --yes) yes=true; shift ;;
        --help|-h) echo 'Usage: bash uninstall.sh [--prefix INSTALL_DIR] [--yes]'; exit 0 ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done
if [[ -x "$prefix/runtime/python/bin/python3" ]]; then
    python="$prefix/runtime/python/bin/python3"
else
    python=$(command -v python3) || { echo 'No maintenance Python found; use the copy of uninstall.sh in your online installation' >&2; exit 1; }
fi
args=(-I -B "$prefix/scripts/uninstall-online.py" --prefix "$prefix")
if [[ "$yes" == true ]]; then args+=(--yes); fi
exec "$python" "${args[@]}"
