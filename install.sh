#!/usr/bin/env bash
# Linux/macOS 在线安装入口；所有大型下载和缓存写入所选磁盘。
# 使用完整命令块，管道下载被截断时不会提前执行安装步骤。
{
set -euo pipefail

repo=coanor/k3
version=latest
prefix=
source_dir=
model_cache=
confirmed=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix|--repo|--version|--source-dir|--model-cache)
            [[ $# -ge 2 ]] || { echo "缺少 $1 的值" >&2; exit 1; }
            case "$1" in
                --prefix) prefix=$2 ;; --repo) repo=$2 ;; --version) version=$2 ;;
                --source-dir) source_dir=$2 ;; --model-cache) model_cache=$2 ;;
            esac
            shift 2 ;;
        --yes) confirmed=true; shift ;;
        --help|-h)
            echo '用法：bash install.sh [--prefix 安装目录] [--repo owner/repo] [--version v版本] [--yes]'
            echo '--source-dir 本地发行目录；--model-cache 已下载的模型目录；均仍校验完整性。'
            exit 0 ;;
        *) echo "未知选项：$1" >&2; exit 1 ;;
    esac
done
[[ "$repo" =~ ^[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+$ ]] || { echo '仓库必须是 owner/repo' >&2; exit 1; }
[[ "$version" == latest || "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo '版本必须是 latest 或 v数字.数字.数字' >&2; exit 1; }
command -v curl >/dev/null || { echo '请先安装 curl' >&2; exit 1; }
command -v tar >/dev/null || { echo '请先安装 tar' >&2; exit 1; }
case "$(uname -s)" in
    Linux)
        system=linux; uv_os=unknown-linux-gnu; default_prefix="$HOME/.local/share/k3"
        libc=$(getconf GNU_LIBC_VERSION 2>/dev/null) || { echo '需要 glibc 2.39 或更高，不支持 musl 系统' >&2; exit 1; }
        libc=${libc#glibc }
        [[ ${libc%%.*} -gt 2 || ( ${libc%%.*} -eq 2 && ${libc#*.} -ge 39 ) ]] || { echo '需要 glibc 2.39 或更高' >&2; exit 1; } ;;
    Darwin)
        system=macos; uv_os=apple-darwin; default_prefix="$HOME/Applications/K3"
        mac_version=$(sw_vers -productVersion)
        [[ ${mac_version%%.*} -ge 14 ]] || { echo '需要 macOS 14 或更高' >&2; exit 1; } ;;
    *) echo '此脚本仅支持 Linux/macOS，Windows 请使用 install.ps1' >&2; exit 1 ;;
esac
machine=$(uname -m)
if [[ "$system" == macos && $(sysctl -n hw.optional.arm64 2>/dev/null || true) == 1 ]]; then machine=arm64; fi
case "$machine" in x86_64) ;; arm64|aarch64) machine=aarch64 ;; *) echo '仅支持 x86_64 和 ARM64' >&2; exit 1 ;; esac
case "$system-$machine" in
    linux-x86_64) uv_digest=745765a3b6e360ad76743599ae5c42e9278c7edf8bbff9fc76d05bf2623a04dd ;;
    linux-aarch64) uv_digest=2eaa5d94f5db7b3a1a092156b9420459e42ab0217d917fe74a876309cef9b5e9 ;;
    macos-x86_64) uv_digest=5e287ef61cb6a9b61b3a83fef124fd143e400468a7dac794230147a810e17119 ;;
    macos-aarch64) uv_digest=7e6ddb9316acc00f2296c82ff4d99977870ee34b2f0ddcae9444d714db9364ed ;;
esac
required=$((8 * 1024 * 1024))
echo "目标平台：$system $machine"
if [[ "$system" == macos ]]; then
    echo 'macOS 当前提供 CLI/TUI，没有 GUI。'
    if [[ "$machine" == x86_64 ]]; then
        echo 'Intel macOS 仅安装 CLI/TUI，不安装 Python 分离环境和模型。'
        required=$((512 * 1024))
    fi
fi
if [[ "$confirmed" == true && -z "$prefix" ]]; then echo '--yes 必须同时指定 --prefix' >&2; exit 1; fi
# 管道的标准输入用于脚本；从独立的终端描述符读取选盘和确认。
if [[ -z "$prefix" || "$confirmed" != true ]]; then
    if [[ -t 0 ]]; then
        exec 3<&0
    elif ! { exec 3</dev/tty; } 2>/dev/null; then
        echo '非交互运行请指定 --prefix 和 --yes' >&2; exit 1
    fi
fi
if [[ -z "$prefix" ]]; then
    [[ -t 3 ]] || { echo '非交互运行请指定 --prefix 和 --yes' >&2; exit 1; }
    echo '可用磁盘/挂载点（安装目录、模型、下载缓存和临时文件将使用同一磁盘）：'
    df -h
    read -r -u 3 -p "请输入安装目录 [$default_prefix]：" prefix
    prefix=${prefix:-$default_prefix}
fi
[[ "$prefix" == /* ]] || prefix="$PWD/$prefix"
[[ ! -L "$prefix" && ( ! -e "$prefix" || ( -d "$prefix" && -z $(ls -A "$prefix") ) ) ]] || { echo '安装目录必须不存在或为空，不能覆盖现有文件' >&2; exit 1; }
ancestor=$prefix
while [[ ! -e "$ancestor" ]]; do ancestor=$(dirname "$ancestor"); done
[[ -d "$ancestor" ]] || { echo '安装路径的父目录不是文件夹' >&2; exit 1; }
available=$(df -Pk "$ancestor" | awk 'NR == 2 {print $4}')
[[ "$available" =~ ^[0-9]+$ && "$available" -ge "$required" ]] || { echo '所选磁盘空间不足，请换一个目录或磁盘' >&2; exit 1; }
echo "安装目录：$prefix"
echo "磁盘剩余：$((available / 1024)) MiB；安装峰值预留：$((required / 1024)) MiB"
echo '完整安装下载量约 1–2 GiB，安装后约 2–4 GiB；精简 CLI 安装明显更小。实际取决于平台和依赖。'
if [[ "$confirmed" != true ]]; then
    [[ -t 3 ]] || { echo '非交互运行请加 --yes' >&2; exit 1; }
    read -r -u 3 -p '确认开始下载和安装？[y/N]：' answer
    case "$answer" in y|Y|yes|YES) ;; *) echo '已取消，未下载任何组件。'; exit 0 ;; esac
fi
mkdir -p "$(dirname "$prefix")"
work=$(mktemp -d "$(dirname "$prefix")/.k3-bootstrap.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir "$work/tmp"
export UV_CACHE_DIR="$work/cache" UV_PYTHON_INSTALL_DIR="$work/python" UV_NO_CONFIG=1
export TMPDIR="$work/tmp" TMP="$work/tmp" TEMP="$work/tmp" PYTHONUTF8=1
unset VIRTUAL_ENV PYTHONHOME PYTHONPATH
uv_name="uv-$machine-$uv_os"
echo '下载已固定版本并校验的安装工具 uv 0.12.13'
curl --proto '=https' --proto-redir '=https' -fL --retry 3 --connect-timeout 20 --max-time 300 \
    "https://github.com/astral-sh/uv/releases/download/0.12.13/$uv_name.tar.gz" -o "$work/uv.tar.gz"
if command -v sha256sum >/dev/null; then actual=$(sha256sum "$work/uv.tar.gz" | awk '{print $1}');
else actual=$(shasum -a 256 "$work/uv.tar.gz" | awk '{print $1}'); fi
[[ "$actual" == "$uv_digest" ]] || { echo 'uv SHA-256 不匹配' >&2; exit 1; }
tar -xzf "$work/uv.tar.gz" -C "$work"
uv="$work/$uv_name/uv"
"$uv" --no-config python install 3.13.15 --install-dir "$work/python" --no-bin --no-registry
python_paths=("$work"/python/cpython-3.13.15-*/bin/python3)
[[ ${#python_paths[@]} -eq 1 && -x ${python_paths[0]} ]] || { echo '无法定位独立 Python' >&2; exit 1; }
python=${python_paths[0]}
# GitHub API 支持公开/私有仓库；凭据只发送给 API，不随 CDN 跳转转发。
"$python" -I - "$repo" "$version" "$work" "$source_dir" <<'PY'
import hashlib, json, os, re, shutil, sys, urllib.request, zipfile
from pathlib import Path, PurePosixPath
repo, version, directory, source = sys.argv[1:]
work = Path(directory)
class Redirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, url):
        if not url.startswith('https://'): raise RuntimeError('下载跳转必须使用 HTTPS')
        return super().redirect_request(request, response, code, message, headers, url)
def request(url, accept='application/vnd.github+json'):
    req = urllib.request.Request(url, headers={'Accept': accept, 'User-Agent': 'K3-installer'})
    token = os.environ.get('GITHUB_TOKEN')
    if token:
        if len(token) > 1024 or not re.fullmatch('[A-Za-z0-9_.-]+', token): raise ValueError('GITHUB_TOKEN 格式不正确')
        req.add_unredirected_header('Authorization', 'Bearer ' + token)
    return urllib.request.build_opener(Redirect()).open(req, timeout=60)
if not source:
    endpoint = 'latest' if version == 'latest' else 'tags/' + version
    with request(f'https://api.github.com/repos/{repo}/releases/{endpoint}') as response: release = json.load(response)
    assets = {item['name']: item for item in release['assets']}
for name in ('k3-install-support.zip', 'k3-install-support.zip.sha256'):
    target = work / name
    if source: shutil.copyfile(Path(source) / 'support' / name, target)
    else:
        url = assets[name]['url']
        if not url.startswith(f'https://api.github.com/repos/{repo}/releases/assets/'): raise RuntimeError('无效发行文件地址')
        with request(url, 'application/octet-stream') as response, target.open('wb') as output: shutil.copyfileobj(response, output)
archive = work / 'k3-install-support.zip'
fields = archive.with_name(archive.name + '.sha256').read_text(encoding='ascii').split()
with archive.open('rb') as stream: actual = hashlib.file_digest(stream, 'sha256').hexdigest()
if len(fields) != 2 or fields[1] != archive.name or not re.fullmatch('[0-9a-f]{64}', fields[0]) or fields[0] != actual: raise RuntimeError('安装支持文件 SHA-256 不匹配')
with zipfile.ZipFile(archive) as z:
    for entry in z.infolist():
        p = PurePosixPath(entry.filename)
        if p.is_absolute() or '..' in p.parts or '\\' in entry.filename or ':' in entry.filename: raise RuntimeError('安装支持文件含不安全路径')
    if sum(entry.file_size for entry in z.infolist()) > 20 * 1024**2: raise RuntimeError('安装支持文件异常过大')
    z.extractall(work / 'support')
installed = json.loads((work / 'support/online-version.json').read_text())['version']
if version != 'latest' and version != 'v' + installed: raise RuntimeError('请求版本与安装支持文件不一致')
if not source and release['tag_name'] != 'v' + installed: raise RuntimeError('发行 tag 与安装支持文件版本不一致')
PY
args=(--prefix "$prefix" --repo "$repo" --uv "$uv")
if [[ -n "$source_dir" ]]; then args+=(--assets-dir "$source_dir/platform"); fi
if [[ -n "$model_cache" ]]; then args+=(--model-cache "$model_cache"); fi
"$python" -I "$work/support/scripts/install-online.py" "${args[@]}"
}
