#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "用法：verify-linux-gui-package.sh <Linux tar.gz>" >&2
    exit 2
fi

archive="$1"
if [[ ! -f "$archive" ]]; then
    echo "找不到 Linux 发行包：$archive" >&2
    exit 1
fi

package_name="$(basename "$archive" .tar.gz)"
contents="$(tar -tzf "$archive")"
for required in \
    "$package_name/k3" \
    "$package_name/k3-gui" \
    "$package_name/share/applications/k3.desktop" \
    "$package_name/share/icons/hicolor/scalable/apps/k3.svg" \
    "$package_name/licenses/SourceHanSansCN-OFL.txt" \
    "$package_name/licenses/LicenseRef-Slint-Royalty-free-2.0.md"
do
    if ! grep -Fxq "$required" <<<"$contents"; then
        echo "Linux 发行包缺少：$required" >&2
        exit 1
    fi
done

echo "Linux GUI 发行包内容验证通过：$archive"
