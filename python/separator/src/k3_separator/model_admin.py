"""受 registry 约束的模型管理命令。"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from .errors import WorkerError
from .models import ModelRegistry
from .runtime import AudioSeparatorRuntime


def main() -> None:
    parser = argparse.ArgumentParser(description="列出或安装 K3 separator 模型")
    parser.add_argument("--model-dir", type=Path, required=True)
    parser.add_argument("--models", type=Path, help="合并到内置列表的 JSON registry")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("list", help="列出 allow-listed 模型")
    install = commands.add_parser("install", help="下载并校验 registry-pinned 模型")
    install.add_argument("model_id")
    args = parser.parse_args()
    registry = ModelRegistry.load(args.models)
    if args.command == "list":
        print(json.dumps({"models": registry.list()}, ensure_ascii=False, indent=2))
        return
    model = registry.get(args.model_id)
    try:
        installed = AudioSeparatorRuntime(args.model_dir, backend="cpu").install_model(
            model
        )
    except WorkerError as error:
        parser.error(f"{error.code}: {error.message}")
    print(installed)


if __name__ == "__main__":
    main()
