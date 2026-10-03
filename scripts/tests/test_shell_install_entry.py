"""验证管道安装入口保留终端交互，并拒绝执行截断的脚本。"""

import os
from pathlib import Path
import select
import shutil
import subprocess
import tempfile
import time
import unittest


ENTRY = Path(__file__).resolve().parents[2] / "install.sh"


@unittest.skipIf(os.name == "nt", "管道终端回归需要 POSIX PTY")
class ShellInstallEntryTests(unittest.TestCase):
    def test_piped_installer_reads_confirmation_from_terminal_without_creating_files(self):
        import fcntl
        import termios

        if not shutil.which("curl") or not shutil.which("tar"):
            self.skipTest("安装入口要求 curl 和 tar")
        with tempfile.TemporaryDirectory(prefix="K3 管道入口 ") as directory:
            destination = Path(directory) / "所选磁盘" / "K3"
            master, slave = os.openpty()
            command = 'set -o pipefail; cat "$K3_ENTRY_SOURCE" | bash -s -- --version v0.1.0'
            process = subprocess.Popen(
                ["bash", "-c", command], stdin=slave, stdout=slave, stderr=slave,
                env=dict(os.environ, K3_ENTRY_SOURCE=str(ENTRY)),
                preexec_fn=lambda: (os.setsid(), fcntl.ioctl(0, termios.TIOCSCTTY, 0)))
            os.close(slave)
            output = bytearray()
            try:
                os.write(master, (str(destination) + "\nn\n").encode())
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    ready, _, _ = select.select([master], [], [], 0.1)
                    if ready:
                        try:
                            output.extend(os.read(master, 65536))
                        except OSError:
                            break
                    if process.poll() is not None and not ready:
                        break
                process.wait(timeout=1)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
                os.close(master)
            self.assertEqual(process.returncode, 0, output.decode(errors="replace"))
            self.assertIn("Start downloading and installing", output.decode())
            self.assertIn("Cancelled. No components were downloaded.", output.decode())
            self.assertFalse(destination.parent.exists())
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_truncated_download_does_not_start_installation(self):
        source = ENTRY.read_text()
        with tempfile.TemporaryDirectory() as directory:
            partial = Path(directory) / "partial.sh"
            partial.write_text(source[:len(source) // 2])
            result = subprocess.run(
                ["bash", "-c", 'set -o pipefail; { cat "$K3_PARTIAL_SOURCE"; exit 18; } | bash'],
                env=dict(os.environ, K3_PARTIAL_SOURCE=str(partial)),
                capture_output=True, text=True, errors="replace", timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")

    def test_help_from_pipe_does_not_require_a_terminal(self):
        result = subprocess.run(
            ["bash", "-c", 'set -o pipefail; cat "$K3_ENTRY_SOURCE" | bash -s -- --help'],
            env=dict(os.environ, K3_ENTRY_SOURCE=str(ENTRY)),
            capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Usage: bash install.sh", result.stdout)


if __name__ == "__main__":
    unittest.main()
