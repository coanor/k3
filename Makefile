SHELL := bash
.SHELLFLAGS := -eu -o pipefail -c
.DEFAULT_GOAL := help

CARGO ?= cargo
DIST_DIR ?= dist
REF ?= main
LLVM_BIN ?=
PYTHON ?= python3
MODEL_CACHE ?=
RUNTIME_DIR ?= $(DIST_DIR)/runtime-native

.PHONY: help build test lint check dist dist-host dist-linux dist-windows \
	dist-macos dist-macos-x86_64 dist-macos-aarch64 dist-all dist-runtime dist-windows-cli dist-windows-aarch64-cli installer dist-online dist-offline

help:
	@printf '%s\n' \
		'Common K3 targets:' \
		'  make build                  Build a release for the current platform' \
		'  make test                   Run workspace tests' \
		'  make lint                   Run strict Clippy checks' \
		'  make check                  Run tests, then lint' \
		'  make dist                   Build online installation components for the current platform (no models or Python dependencies)' \
		'  make dist-offline           Optional: build a full offline package for the current platform' \
		'  make installer              Optional: build an offline system installer for the current platform' \
		'  make dist-linux             Build a native Linux x86_64/ARM64 package' \
		'  make dist-windows           Build an offline package natively on Windows' \
		'  make dist-windows-aarch64-cli Build a native Windows ARM64 CLI-only package' \
		'  make dist-windows-cli       Build a Windows CLI-only package using cargo-xwin' \
		'  make dist-macos             Build a native macOS package (Intel CLI only)' \
		'  make dist-all REF=main      Trigger GitHub Actions builds for all platforms using gh' \
		'' \
		'Optional variables: DIST_DIR, CARGO, PYTHON, MODEL_CACHE, RUNTIME_DIR, LLVM_BIN, REF'

build:
	$(CARGO) build --release --locked --bins

test:
	$(CARGO) test --workspace --locked

lint:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

check: test lint

dist: dist-online

dist-online:
	@system="$$(uname -s)"; machine="$$(uname -m)"; \
	case "$$machine" in arm64) machine=aarch64 ;; x86_64|aarch64) ;; \
		*) echo 'Only x86_64 or ARM64 is supported' >&2; exit 1 ;; esac; \
	case "$$system" in \
		Linux) platform=linux; target="$$machine-unknown-linux-gnu" ;; \
		Darwin) platform=macos; \
			if [[ $$(sysctl -n hw.optional.arm64 2>/dev/null || true) == 1 ]]; then machine=aarch64; fi; \
			target="$$machine-apple-darwin" ;; \
		MINGW*|MSYS*|CYGWIN*) platform=windows; target="$$machine-pc-windows-msvc"; \
			export RUSTFLAGS="$(RUSTFLAGS) -C target-feature=+crt-static" ;; \
		*) echo 'Unsupported native platform' >&2; exit 1 ;; esac; \
	packages=(--package k3); \
	if [[ "$$platform" == linux || "$$platform-$$machine" == windows-x86_64 ]]; then packages+=(--package k3-gui); fi; \
	rustup target add "$$target"; \
	$(CARGO) build --release --locked --bins "$${packages[@]}" --target "$$target"; \
	extension=; if [[ "$$platform" == windows ]]; then extension=.exe; fi; \
	$(PYTHON) scripts/package-online.py support --output "$(DIST_DIR)/online/support"; \
	$(PYTHON) scripts/package-online.py programs "$$platform" "$$machine" \
		"target/$$target/release/k3$$extension" --output "$(DIST_DIR)/online/platform"

dist-offline: dist-host

dist-host:
	@case "$$(uname -s)" in \
		Linux) $(MAKE) dist-linux ;; \
		Darwin) \
			case "$$(uname -m)" in \
				x86_64) $(MAKE) dist-macos-x86_64 ;; \
				arm64) $(MAKE) dist-macos-aarch64 ;; \
				*) printf 'Unsupported macOS architecture: %s\n' "$$(uname -m)" >&2; exit 1 ;; \
			esac ;; \
		MINGW*|MSYS*|CYGWIN*) case "$$(uname -m)" in \
			aarch64|arm64) $(MAKE) dist-windows-aarch64-cli ;; \
			*) $(MAKE) dist-windows ;; esac ;; \
		*) printf 'Unsupported local platform: %s\n' "$$(uname -s)" >&2; exit 1 ;; \
	esac

dist-linux:
	@test "$$(uname -s)" = Linux || { echo 'dist-linux must run on Linux' >&2; exit 1; }
	@case "$$(uname -m)" in \
		x86_64|aarch64) ;; \
		*) echo 'Full Linux packages require an x86_64 or aarch64 host' >&2; exit 1 ;; esac
	$(MAKE) dist-runtime
	@architecture="$$(uname -m)"; target="$$architecture-unknown-linux-gnu"; \
	$(CARGO) build --release --locked --bins --package k3 --package k3-gui --target "$$target"; \
	$(PYTHON) scripts/package-dist.py linux "k3-linux-$$architecture" \
		"target/$$target/release/k3" "$(RUNTIME_DIR)" \
		--gui-binary "target/$$target/release/k3-gui" --dist-dir "$(DIST_DIR)"; \
	$(PYTHON) scripts/check-dist.py "$(DIST_DIR)/k3-linux-$$architecture.tar.gz"

dist-runtime:
	@if [[ ! -f "$(RUNTIME_DIR)/bundle-manifest.json" ]]; then \
		args=(--output "$(RUNTIME_DIR)"); \
		if [[ -n "$(MODEL_CACHE)" ]]; then args+=(--model-cache "$(MODEL_CACHE)"); fi; \
		$(PYTHON) scripts/build-runtime.py "$${args[@]}"; \
	else \
		printf 'Reusing offline runtime: %s\n' "$(RUNTIME_DIR)"; \
		$(PYTHON) scripts/refresh-worker.py "$(RUNTIME_DIR)/python"; \
	fi

dist-windows:
	@case "$$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ;; \
		*) echo 'Full Windows packages require native Windows; use make dist-all or dist-windows-cli' >&2; exit 1 ;; esac
	@case "$$(uname -m)" in aarch64|arm64) echo 'On Windows ARM64, use dist-windows-aarch64-cli' >&2; exit 1 ;; esac
	$(MAKE) dist-runtime
	RUSTFLAGS="$(RUSTFLAGS) -C target-feature=+crt-static" \
		$(CARGO) build --release --locked --bins --package k3 --package k3-gui --target x86_64-pc-windows-msvc
	$(PYTHON) scripts/package-dist.py windows k3-windows-x86_64 \
		target/x86_64-pc-windows-msvc/release/k3.exe "$(RUNTIME_DIR)" \
		--gui-binary target/x86_64-pc-windows-msvc/release/k3-gui.exe --dist-dir "$(DIST_DIR)"
	$(PYTHON) scripts/check-dist.py "$(DIST_DIR)/k3-windows-x86_64.zip"

dist-windows-aarch64-cli:
	@case "$$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ;; \
		*) echo 'Windows ARM64 packages require native Windows ARM64' >&2; exit 1 ;; esac
	@case "$$(uname -m)" in aarch64|arm64) ;; \
		*) echo 'Windows ARM64 packages require an ARM64 host' >&2; exit 1 ;; esac
	rustup target add aarch64-pc-windows-msvc
	RUSTFLAGS="$(RUSTFLAGS) -C target-feature=+crt-static" \
		$(CARGO) build --release --locked --bin k3 --package k3 --target aarch64-pc-windows-msvc
	$(PYTHON) scripts/package-dist.py windows k3-windows-aarch64-cli \
		target/aarch64-pc-windows-msvc/release/k3.exe --cli-only --dist-dir "$(DIST_DIR)"
	$(PYTHON) scripts/check-dist.py "$(DIST_DIR)/k3-windows-aarch64-cli.zip" --cli-only

dist-windows-cli:
	@command -v cargo-xwin >/dev/null 2>&1 || { \
		echo 'cargo-xwin is missing; run cargo install cargo-xwin first' >&2; exit 1; \
	}
	@if [[ -n "$(LLVM_BIN)" ]]; then export PATH="$(LLVM_BIN):$$PATH"; fi; \
	RUSTFLAGS='-C target-feature=+crt-static' \
		$(CARGO) xwin build --release --locked --bins --package k3 --target x86_64-pc-windows-msvc
	DIST_DIR="$(DIST_DIR)" $(PYTHON) scripts/package-dist.py \
		windows k3-windows-x86_64-cli target/x86_64-pc-windows-msvc/release/k3.exe --cli-only

dist-macos:
	@test "$$(uname -s)" = Darwin || { echo 'macOS packages must be built on macOS' >&2; exit 1; }
	$(MAKE) dist-host

dist-macos-x86_64:
	@test "$$(uname -s)" = Darwin || { echo 'macOS packages must be built on macOS' >&2; exit 1; }
	rustup target add x86_64-apple-darwin
	$(CARGO) build --release --locked --bins --package k3 --target x86_64-apple-darwin
	DIST_DIR="$(DIST_DIR)" $(PYTHON) scripts/package-dist.py \
		macos k3-macos-x86_64-cli target/x86_64-apple-darwin/release/k3 --cli-only
	$(PYTHON) scripts/check-dist.py "$(DIST_DIR)/k3-macos-x86_64-cli.tar.gz" --cli-only

dist-macos-aarch64:
	@test "$$(uname -s)" = Darwin || { echo 'macOS packages must be built on macOS' >&2; exit 1; }
	@test "$$(uname -m)" = arm64 || { echo 'Full Apple Silicon packages require an arm64 host' >&2; exit 1; }
	$(MAKE) dist-runtime
	rustup target add aarch64-apple-darwin
	$(CARGO) build --release --locked --bins --package k3 --target aarch64-apple-darwin
	DIST_DIR="$(DIST_DIR)" $(PYTHON) scripts/package-dist.py \
		macos k3-macos-aarch64 target/aarch64-apple-darwin/release/k3 "$(RUNTIME_DIR)"
	$(PYTHON) scripts/check-dist.py "$(DIST_DIR)/k3-macos-aarch64.tar.gz"

dist-all:
	@command -v gh >/dev/null 2>&1 || { echo 'GitHub CLI (gh) is missing' >&2; exit 1; }
	@gh auth status >/dev/null
	gh workflow run dist.yml --ref "$(REF)"
	@printf 'Build distributions triggered: ref=%s\n' "$(REF)"

installer: dist-host
	@case "$$(uname -s)" in \
		Linux) archive="k3-linux-$$(uname -m).tar.gz" ;; \
		Darwin) case "$$(uname -m)" in \
			arm64) archive=k3-macos-aarch64.tar.gz ;; \
			x86_64) archive=k3-macos-x86_64-cli.tar.gz ;; esac ;; \
		MINGW*|MSYS*|CYGWIN*) case "$$(uname -m)" in \
			aarch64|arm64) archive=k3-windows-aarch64-cli.zip ;; \
			*) archive=k3-windows-x86_64.zip ;; esac ;; \
	esac; \
	$(PYTHON) scripts/build-installer.py "$(DIST_DIR)/$$archive" --output-dir "$(DIST_DIR)/installers"
