# K3 separation worker

The worker keeps model runtimes outside the Rust process. It reads one JSON
request per stdin line and writes exactly one JSON response per stdout line;
dependency logs are redirected to stderr.

## Install

For the RTX 5070 Ti development machine, use the checked-in installer. It uses
`uv`, PyTorch 2.11 with CUDA 12.8, and avoids the optional `diffq` extension
that requires Ubuntu's Python development headers:

```bash
bash python/separator/scripts/install-gpu.sh
source .venv-separator/bin/activate
```

When system FFmpeg is unavailable, this path installs a user-space FFmpeg
binary and exposes it only to the worker process.

On a system with Python development headers, regular pip installation is also
supported:

```bash
python3 -m venv .venv-separator
source .venv-separator/bin/activate
python -m pip install -e './python/separator[gpu]'
```

For an NVIDIA GPU, install the PyTorch wheel appropriate for the machine before
the editable package. The worker downloads selected checkpoints into
`~/.cache/k3/models` on first use.

## Run

```bash
python -m k3_separator --model-dir ~/.cache/k3/models
```

Requests:

```json
{"id":1,"method":"health"}
{"id":2,"method":"list_models"}
{"id":3,"method":"separate","params":{"input_path":"/music/song.flac","output_dir":"/project/stems","profile":"quality","model_id":"bs-roformer-viperx-1297","options":{"autocast":true,"segment_size":256}}}
```

Successful separation always writes `vocals.wav` and `accompaniment.wav` and
returns their absolute paths plus exact model provenance. Existing outputs are
not overwritten unless `"overwrite": true` is explicitly requested.

The built-in profiles are:

- `fast`: UVR MDX karaoke model
- `balanced`: UVR MDX instrumental HQ model
- `quality`: BS-RoFormer (default) or Mel-Band RoFormer
- `compatible`: HTDemucs fine-tuned ensemble

## Add or override models

Pass `--models /path/to/models.json`. Entries are merged with the built-ins by
ID, so the file can add a checkpoint or replace a built-in definition:

```json
{
  "models": [
    {
      "id": "my-roformer",
      "filename": "my-roformer.ckpt",
      "architecture": "bs-roformer",
      "profiles": ["quality"],
      "output_stems": ["Vocals", "Instrumental"],
      "license": "MIT",
      "source_url": "https://example.invalid/model",
      "download_url": "https://example.invalid/model.ckpt",
      "expected_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
      "runtime_options": {"segment_size": 256, "overlap": 8, "batch_size": 1}
    }
  ]
}
```

Only add trusted checkpoints. PyTorch `.ckpt` files may contain pickle data.
When both `download_url` and `expected_sha256` are present, the worker downloads
to a temporary file and verifies it before `audio-separator` can deserialize it.
Production registries should always pin both fields and retain a license
snapshot.

## Test

Tests use an in-process fake runtime and do not download models:

```bash
PYTHONPATH=python/separator/src \
  python -m unittest discover -s python/separator/tests -v
```
