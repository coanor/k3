#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
venv_path="${1:-$repo_root/.venv-separator}"
python_spec="${2:-3.13}"

if ! command -v uv >/dev/null 2>&1; then
    echo "k3-separator: 需要 uv：https://docs.astral.sh/uv/" >&2
    exit 1
fi

uv venv --allow-existing --python "$python_spec" "$venv_path"
uv pip install --python "$venv_path/bin/python" \
    torch==2.11.0 torchvision==0.26.0 torchaudio==2.11.0 \
    --index-url https://download.pytorch.org/whl/cpu

# 与 GPU 安装保持同一组经过审查的直接依赖，但使用 CPU ONNX Runtime。
uv pip install --python "$venv_path/bin/python" \
    'audio-separator[cpu]>=0.44.5,<0.45' --no-deps
uv pip install --python "$venv_path/bin/python" \
    -r "$repo_root/python/separator/requirements-runtime.txt" \
    'onnxruntime>=1.17'
uv pip install --python "$venv_path/bin/python" \
    -e "$repo_root/python/separator" --no-deps

"$venv_path/bin/python" -c \
    'import torch; assert not torch.cuda.is_available(); from audio_separator.separator import Separator; print("CPU worker ready")'
