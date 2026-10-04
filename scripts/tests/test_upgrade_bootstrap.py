"""Exercise the piped Unix upgrade entry without networking or a system installation."""

import hashlib
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
spec = importlib.util.spec_from_file_location("upgrade_producer", SCRIPTS / "package-online.py")
producer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(producer)


@unittest.skipIf(os.name == "nt", "The Unix upgrade entry requires Bash")
class UpgradeBootstrapTests(unittest.TestCase):
    def test_piped_entry_checks_installer_and_forwards_upgrade_arguments(self):
        for mode in ("valid", "wrong_digest", "wrong_filename", "invalid_checksum", "download_failed", "oversized", "truncated"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory(prefix="K3 upgrade ") as directory:
                base = Path(directory)
                assets = base / "assets"
                producer.support(assets)
                installer = assets / "install.sh"
                installer.write_text('#!/bin/bash\nprintf "%s\\n" "$@" > "$K3_CAPTURE"\nexit 37\n')
                if mode == "oversized":
                    installer.write_bytes(installer.read_bytes() + b"#" * 262144)
                digest = hashlib.sha256(installer.read_bytes()).hexdigest()
                checksum = f"{digest}  install.sh\n"
                if mode == "wrong_digest":
                    checksum = "0" * 64 + "  install.sh\n"
                elif mode == "wrong_filename":
                    checksum = f"{digest}  another.sh\n"
                elif mode == "invalid_checksum":
                    checksum = "invalid\n"
                (assets / "install.sh.sha256").write_text(checksum)
                tools = base / "tools"
                tools.mkdir()
                curl = tools / "curl"
                curl.write_text(f"#!{sys.executable}\n" + '''
import os
from pathlib import Path
import shutil
import sys
args = sys.argv[1:]
url = next(arg for arg in args if arg.startswith("https://"))
if os.environ["K3_MODE"] == "download_failed":
    sys.exit(22)
assert url.startswith(os.environ["K3_RELEASE_BASE"] + "/"), url
shutil.copyfile(Path(os.environ["K3_ASSETS"]) / url.rsplit("/", 1)[1], args[args.index("-o") + 1])
''')
                curl.chmod(0o755)
                capture = base / "arguments"
                arguments = ["--prefix", str(base / "existing 安装目录"), "--yes"]
                environment = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                                   TMPDIR=str(base), K3_CAPTURE=str(capture), K3_ASSETS=str(assets),
                                   K3_MODE=mode, K3_RELEASE_BASE=
                                   f"https://github.com/coanor/k3/releases/download/v{producer.VERSION}")
                bootstrap = (assets / "upgrade.sh").read_text()
                if mode == "truncated":
                    bootstrap = bootstrap[:-2]
                result = subprocess.run(["bash", "-s", "--", *arguments],
                                        input=bootstrap,
                                        env=environment, cwd=tools, capture_output=True, text=True, timeout=30)
                if mode == "valid":
                    self.assertEqual(result.returncode, 37, result.stderr)
                    self.assertEqual(capture.read_text().splitlines(),
                                     ["--update", "--version", f"v{producer.VERSION}", *arguments])
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(capture.exists(), result.stderr)
                self.assertEqual(list(base.glob("k3-upgrade.*")), [])


if __name__ == "__main__":
    unittest.main()
