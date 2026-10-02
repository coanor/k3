SHELL := bash
.SHELLFLAGS := -eu -o pipefail -c
.DEFAULT_GOAL := help

CARGO ?= cargo
DIST_DIR ?= dist
REF ?= main
LLVM_BIN ?=

.PHONY: help build test lint check dist dist-host dist-linux dist-windows \
	dist-macos dist-macos-x86_64 dist-macos-aarch64 dist-all

help:
	@printf '%s\n' \
		'K3 常用目标：' \
		'  make build                  构建当前平台 release' \
		'  make test                   运行 workspace 测试' \
		'  make lint                   运行严格 Clippy' \
		'  make check                  依次运行 test 和 lint' \
		'  make dist                   构建当前平台发行包' \
		'  make dist-linux             构建 Linux x86_64 包' \
		'  make dist-windows           使用 cargo-xwin 构建 Windows x86_64 包' \
		'  make dist-macos             在 macOS 上构建 Intel 与 Apple Silicon 包' \
		'  make dist-all REF=main      通过 gh 触发四平台 GitHub Actions' \
		'' \
		'可选变量：DIST_DIR、CARGO、LLVM_BIN、REF'

build:
	$(CARGO) build --release --locked

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
		*) printf '不支持的本地平台：%s\n' "$$(uname -s)" >&2; exit 1 ;; \
	esac

dist-linux:
	@test "$$(uname -s)" = Linux || { echo 'dist-linux 必须在 Linux 上运行' >&2; exit 1; }
	$(CARGO) build --release --locked --target x86_64-unknown-linux-gnu \
		--package k3 --package k3-gui
	DIST_DIR="$(DIST_DIR)" scripts/package-dist.sh \
		linux k3-linux-x86_64 target/x86_64-unknown-linux-gnu/release/k3 \
		target/x86_64-unknown-linux-gnu/release/k3-gui

dist-windows:
	@command -v cargo-xwin >/dev/null 2>&1 || { \
		echo '缺少 cargo-xwin；请先运行 cargo install cargo-xwin' >&2; exit 1; \
	}
	@if [[ -n "$(LLVM_BIN)" ]]; then export PATH="$(LLVM_BIN):$$PATH"; fi; \
	RUSTFLAGS='-C target-feature=+crt-static' \
		$(CARGO) xwin build --release --locked --target x86_64-pc-windows-msvc \
			--package k3 --package k3-gui
	DIST_DIR="$(DIST_DIR)" scripts/package-dist.sh \
		windows k3-windows-x86_64 target/x86_64-pc-windows-msvc/release/k3.exe \
		target/x86_64-pc-windows-msvc/release/k3-gui.exe

dist-macos: dist-macos-x86_64 dist-macos-aarch64

dist-macos-x86_64:
	@test "$$(uname -s)" = Darwin || { echo 'macOS 包必须在 macOS 上构建' >&2; exit 1; }
	rustup target add x86_64-apple-darwin
	$(CARGO) build --release --locked --target x86_64-apple-darwin --package k3
	DIST_DIR="$(DIST_DIR)" scripts/package-dist.sh \
		macos k3-macos-x86_64 target/x86_64-apple-darwin/release/k3

dist-macos-aarch64:
	@test "$$(uname -s)" = Darwin || { echo 'macOS 包必须在 macOS 上构建' >&2; exit 1; }
	rustup target add aarch64-apple-darwin
	$(CARGO) build --release --locked --target aarch64-apple-darwin --package k3
	DIST_DIR="$(DIST_DIR)" scripts/package-dist.sh \
		macos k3-macos-aarch64 target/aarch64-apple-darwin/release/k3

dist-all:
	@command -v gh >/dev/null 2>&1 || { echo '缺少 GitHub CLI（gh）' >&2; exit 1; }
	@gh auth status >/dev/null
	gh workflow run dist.yml --ref "$(REF)"
	@printf '已触发 Build distributions：ref=%s\n' "$(REF)"
