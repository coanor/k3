# K3 separation worker

The worker keeps model inference outside the Rust process. It reads JSON-lines from
stdin and writes exactly one JSON response per request to stdout. Dependency logs
go to stderr. The protocol uses UTF-8, including original song names and paths.

## Installation

Online installation and upgrades prepare standalone Python, hardware-compatible
PyTorch, FFmpeg and default models. Application startup does not download runtime
dependencies. Portable offline packages default to CPU; see the
[installation guide](../../docs/install-packages.md#hardware-selection).

For a Linux source checkout with `uv` installed:

```bash
bash python/separator/scripts/install-gpu.sh
source .venv-separator/bin/activate
```

This script automatically detects NVIDIA capability, driver version and VRAM.
GTX 750 Ti uses CUDA 12.6; Blackwell uses CUDA 12.8. Setup validates real kernels
and installs CPU dependencies if accelerator validation fails. To force CPU setup:

```bash
bash python/separator/scripts/install-cpu.sh
```

Source Windows setup requires 64-bit Python 3.11 and the Python Launcher:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\install-separator.ps1
# Force a CPU runtime:
powershell -NoProfile -ExecutionPolicy Bypass -File .\install-separator.ps1 -Backend cpu
```

The script creates `.venv-separator`, installs dependencies and updates worker/model
paths in `config.json`. Existing separation preferences are preserved. System FFmpeg
and `uv` are not required on Windows. Source install scripts use a controlled direct
dependency set because built-in checkpoints do not need optional `diffq` extensions.

## Inference device

The GUI saves **Auto (GPU preferred)**, **CPU only**, or **GPU only** in song
preparation and Settings. Choices apply to new jobs. The CLI accepts
`k3 separate --project PATH --device auto|cpu|gpu`; the worker accepts the same
`--device` values and defaults to `K3_DEVICE` or `auto`.

CPU prevents GPU inference without uninstalling dependencies. GPU requires working
CUDA/MPS kernels and returns `gpu_unavailable` rather than falling back to CPU.
Audio decoding and file writing still use CPU. GPU-only MDX requires a non-native
segment size (128 for built-in models); native segment 256 uses CPU ONNX and is
rejected. Switching devices does not download dependencies.

## Protocol

```bash
python -m k3_separator --model-dir ~/.cache/k3/models --device auto
```

Example requests:

```json
{"id":1,"method":"health"}
{"id":2,"method":"list_models"}
{"id":3,"method":"separate","params":{"input_path":"/music/song.flac","output_dir":"/project/stems","profile":"fast","preserve_backing_vocals":false}}
```

Health includes runtime availability, hardware recommendations, `device_selection`
and `selected_backend` (null until an inference backend has been selected).
Successful separation returns absolute paths to versioned vocals/accompaniment WAV
files and model provenance. Existing outputs require explicit `"overwrite": true`.
Missing model/profile/options use installation recommendations; explicit values win.

## Models and backing vocals

Built-in profiles are Fast (UVR MDX karaoke), Balanced (UVR MDX instrumental HQ),
Quality (BS-RoFormer or Mel-Band RoFormer), and Compatible (HTDemucs ensemble).
Checkpoints are cached in `~/.cache/k3/models` unless `--model-dir` overrides it.
Default models work offline after installation; optional models may download on use.

With `preserve_backing_vocals: true`, a second `uvr-mdx-karaoke-2` pass separates
lead/backing vocals and mixes backing vocals into the accompaniment. Fast/Balanced
hardware recommendations disable this extra pass; other requests default to enabling
it. Provenance records both models and their effective runtime options. All stages
must succeed before new stems are committed.

Use `--models /path/to/models.json` to merge checkpoints over the built-in registry.
Each model specifies its ID, filename, architecture, profiles, output stems, license,
source URL and runtime options. Production registries should pin `download_url`
and `expected_sha256`: downloads are verified before deserialization. Only use trusted
checkpoints, since PyTorch files can contain executable pickle data.

## Progress

`K3_SEPARATION_PROGRESS_PATH` enables atomic UTF-8 JSON progress updates:

```json
{"schema_version":1,"phase":"separating_vocals","fraction":0.37}
```

Phases include preparing, loading/separating vocals, loading/separating backing
vocals, writing audio and saving the project. A fraction is null when inference
progress cannot be measured. GUI jobs use a temporary `.k3-progress-<UUID>` folder
and remove it after completion/failure. Progress never enters stdout responses;
missing/unwritable progress files do not interrupt separation.

## Tests

```bash
PYTHONPATH=python/separator/src \
  python -m unittest discover -s python/separator/tests -v
```

Tests use temporary fixtures and fake inference to avoid model downloads.
