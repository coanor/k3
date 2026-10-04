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
desktop_shortcut=ask
while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix|--repo|--version|--source-dir|--model-cache)
            [[ $# -ge 2 ]] || { echo "Missing value for $1" >&2; exit 1; }
            case "$1" in
                --prefix) prefix=$2 ;; --repo) repo=$2 ;; --version) version=$2 ;;
                --source-dir) source_dir=$2 ;; --model-cache) model_cache=$2 ;;
            esac
            shift 2 ;;
        --yes) confirmed=true; shift ;;
        --desktop-shortcut) desktop_shortcut=yes; shift ;;
        --no-desktop-shortcut) desktop_shortcut=no; shift ;;
        --help|-h)
            echo 'Usage: bash install.sh [--prefix INSTALL_DIR] [--repo owner/repo] [--version vX.Y.Z] [--yes]'
            echo '--desktop-shortcut / --no-desktop-shortcut: choose whether to create a desktop icon; GUI platforms also get an application menu entry.'
            echo '--source-dir: local release directory; --model-cache: downloaded models. Integrity checks still apply.'
            exit 0 ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done
[[ "$repo" =~ ^[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+$ ]] || { echo 'Repository must use owner/repo format' >&2; exit 1; }
[[ "$version" == latest || "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'Version must be latest or vX.Y.Z' >&2; exit 1; }
command -v curl >/dev/null || { echo 'Please install curl first' >&2; exit 1; }
command -v tar >/dev/null || { echo 'Please install tar first' >&2; exit 1; }
case "$(uname -s)" in
    Linux)
        system=linux; uv_os=unknown-linux-gnu; default_prefix="$HOME/.local/share/k3"
        libc=$(getconf GNU_LIBC_VERSION 2>/dev/null) || { echo 'glibc 2.39 or later is required; musl systems are unsupported' >&2; exit 1; }
        libc=${libc#glibc }
        [[ ${libc%%.*} -gt 2 || ( ${libc%%.*} -eq 2 && ${libc#*.} -ge 39 ) ]] || { echo 'glibc 2.39 or later is required' >&2; exit 1; } ;;
    Darwin)
        system=macos; uv_os=apple-darwin; default_prefix="$HOME/Applications/K3"
        mac_version=$(sw_vers -productVersion)
        [[ ${mac_version%%.*} -ge 14 ]] || { echo 'macOS 14 or later is required' >&2; exit 1; } ;;
    *) echo 'This script supports Linux and macOS. On Windows, use install.ps1' >&2; exit 1 ;;
esac
machine=$(uname -m)
if [[ "$system" == macos && $(sysctl -n hw.optional.arm64 2>/dev/null || true) == 1 ]]; then machine=arm64; fi
case "$machine" in x86_64) ;; arm64|aarch64) machine=aarch64 ;; *) echo 'Only x86_64 and ARM64 are supported' >&2; exit 1 ;; esac
case "$system-$machine" in
    linux-x86_64) uv_digest=745765a3b6e360ad76743599ae5c42e9278c7edf8bbff9fc76d05bf2623a04dd ;;
    linux-aarch64) uv_digest=2eaa5d94f5db7b3a1a092156b9420459e42ab0217d917fe74a876309cef9b5e9 ;;
    macos-x86_64) uv_digest=5e287ef61cb6a9b61b3a83fef124fd143e400468a7dac794230147a810e17119 ;;
    macos-aarch64) uv_digest=7e6ddb9316acc00f2296c82ff4d99977870ee34b2f0ddcae9444d714db9364ed ;;
esac
required=$((8 * 1024 * 1024))
echo "Target platform: $system $machine"
if [[ "$system" == macos ]]; then
    echo 'macOS currently provides CLI/TUI only, without a GUI.'
    if [[ "$machine" == x86_64 ]]; then
        echo 'Intel macOS installs CLI/TUI only, without the Python separation runtime or models.'
        required=$((512 * 1024))
    fi
fi
if [[ "$confirmed" == true && -z "$prefix" ]]; then echo '--yes requires --prefix' >&2; exit 1; fi
# 管道的标准输入用于脚本；从独立的终端描述符读取选盘和确认。
if [[ -z "$prefix" || "$confirmed" != true ]]; then
    if [[ -t 0 ]]; then
        exec 3<&0
    elif ! { exec 3</dev/tty; } 2>/dev/null; then
        echo 'Non-interactive installation requires --prefix and --yes' >&2; exit 1
    fi
fi
if [[ -z "$prefix" ]]; then
    [[ -t 3 ]] || { echo 'Non-interactive installation requires --prefix and --yes' >&2; exit 1; }
    echo 'Available disks/mount points (programs, models, download cache and temporary files will use the selected disk):'
    df -h
    read -r -u 3 -p "Enter installation directory [$default_prefix]: " prefix
    prefix=${prefix:-$default_prefix}
fi
[[ "$prefix" == /* ]] || prefix="$PWD/$prefix"
[[ ! -L "$prefix" && ( ! -e "$prefix" || ( -d "$prefix" && -z $(ls -A "$prefix") ) ) ]] || { echo 'Installation directory must be absent or empty; existing files cannot be overwritten' >&2; exit 1; }
ancestor=$prefix
while [[ ! -e "$ancestor" ]]; do ancestor=$(dirname "$ancestor"); done
[[ -d "$ancestor" ]] || { echo 'The parent installation path is not a directory' >&2; exit 1; }
available=$(df -Pk "$ancestor" | awk 'NR == 2 {print $4}')
[[ "$available" =~ ^[0-9]+$ && "$available" -ge "$required" ]] || { echo 'Insufficient space on the selected disk; choose another directory or disk' >&2; exit 1; }
echo "Installation directory: $prefix"
echo "Free disk space: $((available / 1024)) MiB; reserved space for peak installation usage: $((required / 1024)) MiB"
echo 'A full installation downloads about 1-2 GiB and uses about 2-4 GiB. CLI-only installations are smaller. Sizes vary by platform and dependencies.'
if [[ "$confirmed" != true ]]; then
    [[ -t 3 ]] || { echo 'Non-interactive installation requires --yes' >&2; exit 1; }
    read -r -u 3 -p 'Start downloading and installing? [y/N]: ' answer
    case "$answer" in y|Y|yes|YES) ;; *) echo 'Cancelled. No components were downloaded.'; exit 0 ;; esac
fi
if [[ "$system" == linux && "$desktop_shortcut" == ask && "$confirmed" != true ]]; then
    read -r -u 3 -p 'Create a desktop shortcut? [y/N]: ' answer
    case "$answer" in y|Y|yes|YES) desktop_shortcut=yes ;; *) desktop_shortcut=no ;; esac
fi
mkdir -p "$(dirname "$prefix")"
work=$(mktemp -d "$(dirname "$prefix")/.k3-bootstrap.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir "$work/tmp"
export UV_CACHE_DIR="$work/cache" UV_PYTHON_INSTALL_DIR="$work/python" UV_NO_CONFIG=1
export TMPDIR="$work/tmp" TMP="$work/tmp" TEMP="$work/tmp" PYTHONUTF8=1
unset VIRTUAL_ENV PYTHONHOME PYTHONPATH
uv_name="uv-$machine-$uv_os"
echo 'Downloading pinned installer tool uv 0.12.13 with checksum verification'
curl --proto '=https' --proto-redir '=https' -fL --retry 3 --connect-timeout 20 --max-time 300 \
    "https://github.com/astral-sh/uv/releases/download/0.12.13/$uv_name.tar.gz" -o "$work/uv.tar.gz"
if command -v sha256sum >/dev/null; then actual=$(sha256sum "$work/uv.tar.gz" | awk '{print $1}');
else actual=$(shasum -a 256 "$work/uv.tar.gz" | awk '{print $1}'); fi
[[ "$actual" == "$uv_digest" ]] || { echo 'uv SHA-256 mismatch' >&2; exit 1; }
tar -xzf "$work/uv.tar.gz" -C "$work"
uv="$work/$uv_name/uv"
"$uv" --no-config python install 3.13.15 --install-dir "$work/python" --no-bin --no-registry
python_paths=("$work"/python/cpython-3.13.15-*/bin/python3)
[[ ${#python_paths[@]} -eq 1 && -x ${python_paths[0]} ]] || { echo 'Could not locate standalone Python' >&2; exit 1; }
python=${python_paths[0]}
# Public downloads avoid the API; credentials are restricted to private API downloads.
"$python" -I - "$repo" "$version" "$work" "$source_dir" <<'PY'
import hashlib, json, os, re, shutil, sys, urllib.error, urllib.request, zipfile
from pathlib import Path, PurePosixPath
repo, version, directory, source = sys.argv[1:]
work = Path(directory)
class Redirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, url):
        if not url.startswith('https://'): raise RuntimeError('Download redirects must use HTTPS')
        return super().redirect_request(request, response, code, message, headers, url)
def request(url, accept='application/vnd.github+json'):
    req = urllib.request.Request(url, headers={'Accept': accept, 'User-Agent': 'K3-installer'})
    token = os.environ.get('GITHUB_TOKEN')
    if token and url.startswith('https://api.github.com/repos/'):
        if len(token) > 1024 or not re.fullmatch('[A-Za-z0-9_.-]+', token): raise ValueError('Invalid GITHUB_TOKEN format')
        req.add_unredirected_header('Authorization', 'Bearer ' + token)
    return urllib.request.build_opener(Redirect()).open(req, timeout=60)
# Public release downloads do not consume the anonymous GitHub REST API quota.
route = 'latest/download' if version == 'latest' else 'download/' + version
base = f'https://github.com/{repo}/releases/{route}'
release = None
assets = None
def asset_response(name):
    global release, assets
    if assets is None:
        try:
            return request(base + '/' + name, 'application/octet-stream')
        except urllib.error.HTTPError as error:
            # Private repositories return 404 on public download URLs.
            if error.code != 404 or not os.environ.get('GITHUB_TOKEN'): raise
        endpoint = 'latest' if version == 'latest' else 'tags/' + version
        with request(f'https://api.github.com/repos/{repo}/releases/{endpoint}') as response: release = json.load(response)
        if version != 'latest' and release.get('tag_name') != version: raise RuntimeError('Release tag does not match requested version')
        assets = {item['name']: item for item in release['assets']}
    url = assets[name]['url']
    if not url.startswith(f'https://api.github.com/repos/{repo}/releases/assets/'): raise RuntimeError('Invalid release asset URL')
    return request(url, 'application/octet-stream')
for name in ('k3-install-support.zip', 'k3-install-support.zip.sha256'):
    target = work / name
    if source: shutil.copyfile(Path(source) / 'support' / name, target)
    else:
        with asset_response(name) as response, target.open('wb') as output: shutil.copyfileobj(response, output)
archive = work / 'k3-install-support.zip'
fields = archive.with_name(archive.name + '.sha256').read_text(encoding='ascii').split()
with archive.open('rb') as stream: actual = hashlib.file_digest(stream, 'sha256').hexdigest()
if len(fields) != 2 or fields[1] != archive.name or not re.fullmatch('[0-9a-f]{64}', fields[0]) or fields[0] != actual: raise RuntimeError('Installation support archive SHA-256 mismatch')
with zipfile.ZipFile(archive) as z:
    for entry in z.infolist():
        p = PurePosixPath(entry.filename)
        if p.is_absolute() or '..' in p.parts or '\\' in entry.filename or ':' in entry.filename: raise RuntimeError('Installation support archive contains an unsafe path')
    if sum(entry.file_size for entry in z.infolist()) > 20 * 1024**2: raise RuntimeError('Installation support archive exceeds the size limit')
    z.extractall(work / 'support')
installed = json.loads((work / 'support/online-version.json').read_text())['version']
if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', installed): raise RuntimeError('Invalid installation support file version')
if version != 'latest' and version != 'v' + installed: raise RuntimeError('Requested version does not match installation support files')
if release is not None and release['tag_name'] != 'v' + installed: raise RuntimeError('Release tag does not match installation support file version')
PY
args=(--prefix "$prefix" --repo "$repo" --uv "$uv")
if [[ "$desktop_shortcut" == yes ]]; then args+=(--desktop-shortcut); fi
if [[ -n "$source_dir" ]]; then args+=(--assets-dir "$source_dir/platform"); fi
if [[ -n "$model_cache" ]]; then args+=(--model-cache "$model_cache"); fi
"$python" -I "$work/support/scripts/install-online.py" "${args[@]}"
}
