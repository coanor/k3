"""Exercise the batch entry point with a real isolated Python environment."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import venv

from k3_separator.hardware import NvidiaGpu, select_runtime, write_plan


@unittest.skipIf(os.name == "nt", "POSIX wrapper requires Bash")
class WrapperDefaultsTests(unittest.TestCase):
    def test_hardware_defaults_and_environment_overrides(self):
        repo = Path(__file__).resolve().parents[3]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = root / "python"
            venv.create(environment, with_pip=False)
            python = environment / "bin/python"
            site = next(environment.glob("lib/python*/site-packages"))
            (site / "worker.pth").write_text(str(repo / "python/separator/src") + "\n")
            plan = select_runtime([NvidiaGpu(0, "GPU-fixture", "750 Ti", (5, 0), 4096, (560, 94))],
                                  "linux", "x86_64")
            write_plan(environment, plan)
            song = root / "song.wav"
            song.write_bytes(b"audio")
            stub = root / "fake-k3"
            stub.write_text(f"#!{sys.executable}\n" + '''
import json
import os
from pathlib import Path
import sys
args = sys.argv[1:]
project = Path(args[args.index('--root' if args[0] == 'new' else '--project') + 1])
if args[0] == 'new':
    project.mkdir(parents=True)
else:
    Path(os.environ['CAPTURE']).write_text(json.dumps(args))
    (project / 'project.json').write_text(json.dumps({'separation': {
        'status': 'ready', 'details': {'vocals': 'vocals.wav', 'accompaniment': 'instrumental.wav'}}}))
''')
            stub.chmod(0o755)
            env = {key: value for key, value in os.environ.items() if not key.startswith("K3_")}
            env.update(K3_BIN=str(stub), K3_PYTHON=str(python), CAPTURE=str(root / "args.json"))
            for name, overrides in (("automatic", {}), ("cpu", {"K3_DEVICE": "cpu"}),
                                    ("gpu", {"K3_DEVICE": "gpu"}), ("explicit", {
                    "K3_PROFILE": "quality", "K3_MODEL": "mel-band-roformer-kim-vocal-2",
                    "K3_SEGMENT_SIZE": "64", "K3_AUTOCAST": "true",
                    "K3_PRESERVE_BACKING_VOCALS": "true"})):
                result = subprocess.run(["bash", str(repo / "separate.sh"), "-f", str(song),
                                         "-d", str(root / name)], env={**env, **overrides},
                                        capture_output=True, text=True, timeout=30)
                self.assertEqual(0, result.returncode, result.stderr)
                args = json.loads((root / "args.json").read_text())
                expected_device = overrides.get("K3_DEVICE", "auto")
                self.assertEqual(expected_device, args[args.index("--device") + 1])
                if name in {"automatic", "cpu", "gpu"}:
                    self.assertEqual("fast", args[args.index("--profile") + 1])
                    self.assertEqual("uvr-mdx-karaoke-2", args[args.index("--model") + 1])
                    self.assertEqual("256" if name == "cpu" else "128", args[args.index("--segment-size") + 1])
                    self.assertIn("--no-autocast", args)
                    self.assertIn("--no-preserve-backing-vocals", args)
                else:
                    self.assertEqual("quality", args[args.index("--profile") + 1])
                    self.assertEqual("mel-band-roformer-kim-vocal-2", args[args.index("--model") + 1])
                    self.assertEqual("64", args[args.index("--segment-size") + 1])
                    self.assertNotIn("--no-autocast", args)
                    self.assertNotIn("--no-preserve-backing-vocals", args)


if __name__ == "__main__":
    unittest.main()
