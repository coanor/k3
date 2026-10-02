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
	dist-macos dist-macos-x86_64 dist-macos-aarch64 dist-all dist-runtime dist-windows-cli installer

help:
	@printf '%s\n' \
		'K3 常用目标：' \
		'  make build                  构建当前平台 release' \
		'  make test                   运行 workspace 测试' \
		'  make lint                   运行严格 Clippy' \
		'  make check                  依次运行 test 和 lint' \
		'  make dist                   构建当前平台离线发行包' \
		'  make installer              构建当前平台系统安装包' \
		'  make dist-linux             构建 Linux x86_64 包' \
		'  make dist-windows           在 Windows 原生构建离线包' \
		'  make dist-windows-cli       使用 cargo-xwin 构建 Windows 纯 CLI 包' \
		'  make dist-macos             构建本机架构 macOS 包（Intel 仅 CLI）' \
		'  make dist-all REF=main      通过 gh 触发四平台 GitHub Actions' \
		'' \
		'可选变量：DIST_DIR、CARGO、PYTHON、MODEL_CACHE、RUNTIME_DIR、LLVM_BIN、REF'

build:
	$(CARGO) build --release --locked --bins

test:
	$(CARGO) test --workspace --locked

lint:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

check: test lint

dist: dist-host

dist-host:
	@case "$$(uname -s)" in \
		Linux) $(MAKE) dist-linux ;; \
		Darwin) \
			case "$$(uname -m)" in \
				x86_64) $(MAKE) dist-macos-x86_64 ;; \
				arm64) $(MAKE) dist-macos-aarch64 ;; \
				*) printf '不支持的 macOS 架构：%s\n' "$$(uname -m)" >&2; exit 1 ;; \
			esac ;; \
		MINGW*|MSYS*|CYGWIN*) $(MAKE) dist-windows ;; \
		*) printf '不支持的本地平台：%s\n' "$$(uname -s)" >&2; exit 1 ;; \
	esac

dist-linux:
	@test "$$(uname -s)" = Linux || { echo 'dist-linux 必须在 Linux 上运行' >&2; exit 1; }
	@test "$$(uname -m)" = x86_64 || { echo '完整 Linux 包需要 x86_64 主机' >&2; exit 1; }
	$(MAKE) dist-runtime
	$(CARGO) build --release --locked --bins --package k3 --package k3-gui --target x86_64-unknown-linux-gnu
	DIST_DIR="$(DIST_DIR)" $(PYTHON) scripts/package-dist.py \
		linux k3-linux-x86_64 target/x86_64-unknown-linux-gnu/release/k3 "$(RUNTIME_DIR)" \
		--gui-binary target/x86_64-unknown-linux-gnu/release/k3-gui
	$(PYTHON) scripts/check-dist.py "$(DIST_DIR)/k3-linux-x86_64.tar.gz"

dist-runtime:
	@if [[ ! -f "$(RUNTIME_DIR)/bundle-manifest.json" ]]; then \
		args=(--output "$(RUNTIME_DIR)"); \
		if [[ -n "$(MODEL_CACHE)" ]]; then args+=(--model-cache "$(MODEL_CACHE)"); fi; \
		$(PYTHON) scripts/build-runtime.py "$${args[@]}"; \
	else \
		printf '复用离线 runtime：%s\n' "$(RUNTIME_DIR)"; \
		$(PYTHON) scripts/refresh-worker.py "$(RUNTIME_DIR)/python"; \
	fi

dist-windows:
	@case "$$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ;; \
		*) echo '完整 Windows 包需要 Windows 原生环境；可用 make dist-all 或 dist-windows-cli' >&2; exit 1 ;; esac
	$(MAKE) dist-runtime
	RUSTFLAGS="$(RUSTFLAGS) -C target-feature=+crt-static" \
		$(CARGO) build --release --locked --bins --package k3 --package k3-gui --target x86_64-pc-windows-msvc
	$(PYTHON) scripts/package-dist.py windows k3-windows-x86_64 \
		target/x86_64-pc-windows-msvc/release/k3.exe "$(RUNTIME_DIR)" \
		--gui-binary target/x86_64-pc-windows-msvc/release/k3-gui.exe --dist-dir "$(DIST_DIR)"
	$(PYTHON) scripts/check-dist.py "$(DIST_DIR)/k3-windows-x86_64.zip"

dist-windows-cli:
	@command -v cargo-xwin >/dev/null 2>&1 || { \
		echo '缺少 cargo-xwin；请先运行 cargo install cargo-xwin' >&2; exit 1; \
	}
	@if [[ -n "$(LLVM_BIN)" ]]; then export PATH="$(LLVM_BIN):$$PATH"; fi; \
	RUSTFLAGS='-C target-feature=+crt-static' \
		$(CARGO) xwin build --release --locked --bins --package k3 --target x86_64-pc-windows-msvc
	DIST_DIR="$(DIST_DIR)" $(PYTHON) scripts/package-dist.py \
		windows k3-windows-x86_64-cli target/x86_64-pc-windows-msvc/release/k3.exe --cli-only

dist-macos:
	@test "$$(uname -s)" = Darwin || { echo 'macOS 包必须在 macOS 上构建' >&2; exit 1; }
	$(MAKE) dist-host

dist-macos-x86_64:
	@test "$$(uname -s)" = Darwin || { echo 'macOS 包必须在 macOS 上构建' >&2; exit 1; }
	rustup target add x86_64-apple-darwin
	$(CARGO) build --release --locked --bins --package k3 --target x86_64-apple-darwin
	DIST_DIR="$(DIST_DIR)" $(PYTHON) scripts/package-dist.py \
		macos k3-macos-x86_64-cli target/x86_64-apple-darwin/release/k3 --cli-only
	$(PYTHON) scripts/check-dist.py "$(DIST_DIR)/k3-macos-x86_64-cli.tar.gz" --cli-only

dist-macos-aarch64:
	@test "$$(uname -s)" = Darwin || { echo 'macOS 包必须在 macOS 上构建' >&2; exit 1; }
	@test "$$(uname -m)" = arm64 || { echo '完整 Apple Silicon 包需要 arm64 主机' >&2; exit 1; }
	$(MAKE) dist-runtime
	rustup target add aarch64-apple-darwin
	$(CARGO) build --release --locked --bins --package k3 --target aarch64-apple-darwin
	DIST_DIR="$(DIST_DIR)" $(PYTHON) scripts/package-dist.py \
		macos k3-macos-aarch64 target/aarch64-apple-darwin/release/k3 "$(RUNTIME_DIR)"
	$(PYTHON) scripts/check-dist.py "$(DIST_DIR)/k3-macos-aarch64.tar.gz"

dist-all:
	@command -v gh >/dev/null 2>&1 || { echo '缺少 GitHub CLI（gh）' >&2; exit 1; }
	@gh auth status >/dev/null
	gh workflow run dist.yml --ref "$(REF)"
	@printf '已触发 Build distributions：ref=%s\n' "$(REF)"

installer: dist-host
	@case "$$(uname -s)" in \
		Linux) archive=k3-linux-x86_64.tar.gz ;; \
		Darwin) case "$$(uname -m)" in \
			arm64) archive=k3-macos-aarch64.tar.gz ;; \
			x86_64) archive=k3-macos-x86_64-cli.tar.gz ;; esac ;; \
		MINGW*|MSYS*|CYGWIN*) archive=k3-windows-x86_64.zip ;; \
	esac; \
	$(PYTHON) scripts/build-installer.py "$(DIST_DIR)/$$archive" --output-dir "$(DIST_DIR)/installers"
