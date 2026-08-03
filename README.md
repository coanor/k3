# K3

K3 is an offline-first terminal karaoke application under active development.
The current vertical slice creates durable song projects, imports LRC lyrics,
models recording sessions, and isolates local stem-separation workers behind a
small Rust interface.

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

The microphone/audio-device and model-process adapters are deliberately not
part of this first milestone. See [the MVP specification](docs/mvp-spec.md) for
scope and acceptance criteria.

