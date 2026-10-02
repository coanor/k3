# K3

K3 is an offline-first terminal karaoke application under active development.
The current vertical slice creates durable song projects, imports LRC lyrics,
models recording sessions, and isolates local stem-separation workers behind a
small Rust interface.

For installation and end-user workflows, see the
[Chinese user manual](docs/user-manual.md).

## Build and test

```bash
cargo test --all
cargo run -p k3 -- --help
```

## Create and inspect a project

```bash
cargo run -p k3 -- new \
  --root ./songs/example \
  --song /path/to/song.flac \
  --lyrics /path/to/song.lrc \
  --title "Example"

cargo run -p k3 -- show --project ./songs/example
cargo run -p k3 -- tui --project ./songs/example
```

Run the local Python worker through the Rust `StemSeparator` adapter:

```bash
cargo run -p k3 -- separate \
  --project ./songs/example \
  --profile quality \
  --model mel-band-roformer-kim-vocal-2 \
  --worker ./.venv-separator/bin/k3-separator \
  --model-dir ~/.cache/k3/models
```

The microphone/audio-device adapter is not part of this milestone. Local model
separation is available through the Python worker and Rust process adapter
described below. See [the MVP specification](docs/mvp-spec.md) for the original
slice boundaries and acceptance criteria.

## Local separation worker

The Python JSON-lines worker under [`python/separator`](python/separator) runs
the local model outside the Rust process. It supports profile defaults, explicit
checkpoint selection, custom registries, SHA-256 provenance, and normalized
`vocals.wav` / `accompaniment.wav` output. See its
[README](python/separator/README.md) for installation and protocol examples.

## 离线发行包

Windows x86_64、Linux x86_64 与 macOS Apple Silicon 的完整包包含程序、独立
Python、CPU 分离依赖、FFmpeg 和默认模型；解压后无需安装 Python 或在线下载模型。
Intel macOS 提供纯 CLI 包。构建入口为 `make dist` 和 GitHub Actions，具体用法见
[离线发行包说明](docs/offline-package.md)。
