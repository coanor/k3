"""检查发行文档白名单和旧安装载荷的文档清理。"""

import os
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from dist_manuals import copy_manuals


class ManualTests(unittest.TestCase):
    def test_legacy_payload_keeps_only_user_manuals(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs/research").mkdir(parents=True)
            (root / "docs/research/internal.md").write_text("开发记录")
            for name in ("README.md", "INSTALL.md", "INSTALL-PACKAGE.md"):
                (root / name).write_text("旧文档")
            (root / "k3.exe").write_bytes(b"application")
            copy_manuals(root)
            self.assertEqual({path.name for path in (root / "docs").iterdir()},
                             {"user-manual.md", "offline-package.md", "install-packages.md"})
            self.assertEqual({path.name for path in root.iterdir()}, {"docs", "k3.exe"})
            self.assertEqual((root / "k3.exe").read_bytes(), b"application")

    @unittest.skipIf(os.name == "nt", "Windows 创建符号链接需要额外权限")
    def test_legacy_docs_symlink_does_not_modify_its_target(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            outside = work / "outside"
            outside.mkdir()
            (outside / "preserved.md").write_text("保留内容")
            root = work / "package"
            root.mkdir()
            (root / "docs").symlink_to(outside, target_is_directory=True)
            copy_manuals(root)
            self.assertFalse((root / "docs").is_symlink())
            self.assertEqual((outside / "preserved.md").read_text(), "保留内容")
