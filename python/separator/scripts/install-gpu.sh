#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
venv_path="${1:-$repo_root/.venv-separator}"

if ! command -v uv >/dev/null 2>&1; then
    echo "k3-separator: uv is required: https://docs.astral.sh/uv/" >&2
    exit 1
fi

uv venv --allow-existing --python python3 "$venv_path"
uv pip install --python "$venv_path/bin/python" \
    torch==2.11.0 torchvision==0.26.0 torchaudio==2.11.0 \
    --index-url https://download.pytorch.org/whl/cu128

# audio-separator declares diffq on Linux. Its extension needs Python.h, while
# the built-in K3 checkpoints do not use it, so install the reviewed direct
# runtime set without dependency expansion.
uv pip install --python "$venv_path/bin/python" \
    'audio-separator[gpu]>=0.44.5,<0.45' --no-deps
uv pip install --python "$venv_path/bin/python" \
    -r "$repo_root/python/separator/requirements-runtime.txt"
uv pip install --python "$venv_path/bin/python" \
    -e "$repo_root/python/separator" --no-deps

"$venv_path/bin/python" -c \
    'import torch; assert torch.cuda.is_available(); print(torch.cuda.get_device_name(0))'
