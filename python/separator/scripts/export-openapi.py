"""更新已提交的 separator HTTP OpenAPI snapshot。"""

from __future__ import annotations

import argparse
import json
import tempfile
from pathlib import Path

from k3_separator.server import ServerConfig, create_app


def main() -> None:
    parser = argparse.ArgumentParser(description="更新 separator OpenAPI snapshot")
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as directory:
        document = create_app(
            ServerConfig(
                data_dir=Path(directory), tokens={"snapshot": "unused"}, runtime="fake"
            )
        ).openapi()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
