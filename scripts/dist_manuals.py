"""为便携包和安装包准备用户手册，不携带开发文档。"""

from pathlib import Path
import shutil

MANUALS = ("user-manual.md", "offline-package.md", "install-packages.md")
SOURCE = Path(__file__).resolve().parent.parent / "docs"


def copy_manuals(root: Path) -> None:
    """替换文档目录，同时清理旧发行包中的仓库说明和重复手册。"""
    docs = root / "docs"
    if docs.is_symlink() or docs.is_file():
        docs.unlink()
    elif docs.exists():
        shutil.rmtree(docs)
    docs.mkdir()
    for name in MANUALS:
        shutil.copy2(SOURCE / name, docs / name)
    for name in ("README.md", "INSTALL.md", "INSTALL-PACKAGE.md"):
        (root / name).unlink(missing_ok=True)
