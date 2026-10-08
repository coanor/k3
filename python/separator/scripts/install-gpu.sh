#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
venv_path="${1:-$repo_root/.venv-separator}"

if ! command -v uv >/dev/null 2>&1; then
    echo "k3-separator: uv is required: https://docs.astral.sh/uv/" >&2
    exit 1
fi

uv venv --allow-existing --python python3 "$venv_path"
"$venv_path/bin/python" "$repo_root/python/separator/scripts/install-runtime.py" \
    --python "$venv_path/bin/python" --uv "$(command -v uv)" --backend auto

# audio-separator declares diffq on Linux. Its extension needs Python.h, while
# the built-in K3 checkpoints do not use it, so install the reviewed direct
# runtime set without dependency expansion.
uv pip install --python "$venv_path/bin/python" \
    'audio-separator>=0.44.5,<0.45' --no-deps
uv pip uninstall --python "$venv_path/bin/python" onnxruntime-gpu
uv pip install --python "$venv_path/bin/python" \
    --reinstall-package onnxruntime \
    -r "$repo_root/python/separator/requirements-runtime.txt" \
    'onnxruntime==1.24.4'
uv pip install --python "$venv_path/bin/python" \
    -e "$repo_root/python/separator" --no-deps

"$venv_path/bin/python" -c \
    'from audio_separator.separator import Separator; print("Separation worker ready")'
