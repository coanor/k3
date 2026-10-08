#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
venv_path="${1:-$repo_root/.venv-separator}"
python_spec="${2:-3.13}"

if ! command -v uv >/dev/null 2>&1; then
    echo "k3-separator: uv is required: https://docs.astral.sh/uv/" >&2
    exit 1
fi

uv venv --allow-existing --python "$python_spec" "$venv_path"
"$venv_path/bin/python" "$repo_root/python/separator/scripts/install-runtime.py" \
    --python "$venv_path/bin/python" --uv "$(command -v uv)" --backend cpu

# 与 GPU 安装保持同一组经过审查的直接依赖，但使用 CPU ONNX Runtime。
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
    'import torch; assert not torch.cuda.is_available(); from audio_separator.separator import Separator; print("CPU worker ready")'
