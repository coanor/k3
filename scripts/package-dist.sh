#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $# -gt 4 ]]; then
    echo "用法：package-dist.sh <linux|windows|macos> <包名> <CLI 二进制路径> [GUI 二进制路径]" >&2
    exit 2
fi

platform="$1"
package_name="$2"
binary_path="$3"
gui_binary_path="${4:-}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
dist_dir="${DIST_DIR:-$repo_root/dist}"

if [[ ! -f "$binary_path" ]]; then
    echo "找不到待打包二进制：$binary_path" >&2
    exit 1
fi
if [[ "$platform" != "macos" && ( -z "$gui_binary_path" || ! -f "$gui_binary_path" ) ]]; then
    echo "找不到待打包 GUI 二进制：${gui_binary_path:-<未提供>}" >&2
    exit 1
fi
if [[ ! "$package_name" =~ ^k3-[a-z0-9_-]+$ ]]; then
    echo "无效包名：$package_name" >&2
    exit 1
fi

staging="$(mktemp -d "${TMPDIR:-/tmp}/k3-package.XXXXXX")"
package_dir="$staging/$package_name"
trap 'rm -rf -- "$staging"' EXIT
mkdir -p "$package_dir"

cp "$repo_root/README.md" "$package_dir/"
cp -R "$repo_root/docs" "$package_dir/"

copy_separator_worker() {
    local separator_dir="$package_dir/python/separator"
    mkdir -p "$separator_dir/scripts" "$separator_dir/src/k3_separator"
    cp "$repo_root/python/separator/README.md" \
        "$repo_root/python/separator/pyproject.toml" \
        "$repo_root/python/separator/requirements-runtime.txt" \
        "$separator_dir/"
    install -m 0755 "$repo_root"/python/separator/scripts/*.sh "$separator_dir/scripts/"
    cp "$repo_root"/python/separator/src/k3_separator/*.py \
        "$separator_dir/src/k3_separator/"
}

mkdir -p "$dist_dir"
case "$platform" in
    linux)
        install -m 0755 "$binary_path" "$package_dir/k3"
        install -m 0755 "$gui_binary_path" "$package_dir/k3-gui"
        install -Dm 0644 "$repo_root/crates/k3-gui/assets/k3.desktop" \
            "$package_dir/share/applications/k3.desktop"
        install -Dm 0644 "$repo_root/crates/k3-gui/assets/k3.svg" \
            "$package_dir/share/icons/hicolor/scalable/apps/k3.svg"
        install -Dm 0644 "$repo_root/crates/k3-gui/assets/fonts/OFL.txt" \
            "$package_dir/licenses/SourceHanSansCN-OFL.txt"
        install -Dm 0644 \
            "$repo_root/crates/k3-gui/assets/licenses/LicenseRef-Slint-Royalty-free-2.0.md" \
            "$package_dir/licenses/LicenseRef-Slint-Royalty-free-2.0.md"
        install -m 0755 "$repo_root/separate.sh" "$package_dir/separate.sh"
        copy_separator_worker
        archive="$dist_dir/$package_name.tar.gz"
        tar -C "$staging" -czf "$archive" "$package_name"
        ;;
    macos)
        install -m 0755 "$binary_path" "$package_dir/k3"
        archive="$dist_dir/$package_name.tar.gz"
        tar -C "$staging" -czf "$archive" "$package_name"
        ;;
    windows)
        cp "$binary_path" "$package_dir/k3.exe"
        cp "$gui_binary_path" "$package_dir/k3-gui.exe"
        cp "$repo_root/separate.ps1" "$repo_root/install-separator.ps1" "$package_dir/"
        copy_separator_worker
        archive="$dist_dir/$package_name.zip"
        python3 -m zipfile -c "$archive" "$package_dir"
        ;;
    *)
        echo "不支持的发行平台：$platform" >&2
        exit 2
        ;;
esac

archive_name="$(basename "$archive")"
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$dist_dir" && sha256sum "$archive_name" > "$archive_name.sha256")
else
    (cd "$dist_dir" && shasum -a 256 "$archive_name" > "$archive_name.sha256")
fi
printf '已生成：%s\n' "$archive"
