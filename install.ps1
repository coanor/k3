# Windows 在线安装入口，兼容 Windows PowerShell 5.1。
[CmdletBinding()]
param(
    [string]$InstallDir,
    [string]$Repo = 'coanor/k3',
    [string]$Version = 'latest',
    [string]$SourceDir,
    [string]$ModelCache,
    [switch]$Yes
)
$ErrorActionPreference = 'Stop'
if ($Repo -notmatch '^[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+$') { throw '仓库必须是 owner/repo' }
if ($Version -ne 'latest' -and $Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') { throw '版本必须是 latest 或 v数字.数字.数字' }
$architecture = [Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITECTURE', 'Machine')
if (-not $architecture) { $architecture = $env:PROCESSOR_ARCHITEW6432; if (-not $architecture) { $architecture = $env:PROCESSOR_ARCHITECTURE } }
switch ($architecture) {
    'AMD64' { $machine = 'x86_64'; $digest = 'a86c9dc7bad9b03f388583b7187c05fe9951c2e0d392217e8fd43d97787f6ec2' }
    'ARM64' { $machine = 'aarch64'; $digest = '1efb2654b06e7063d4ac1fc9d49a9bda9a6704d82f035b589a2751a592f14151' }
    default { throw '仅支持 Windows x64 和 ARM64' }
}
$os = [Version](Get-CimInstance Win32_OperatingSystem).Version
if ($os.Major -lt 10 -or ($machine -eq 'aarch64' -and $os.Build -lt 22000)) { throw '需要 Windows 10/11；ARM64 需要 Windows 11' }
$required = 8GB
Write-Host "目标平台：Windows $machine"
if ($machine -eq 'aarch64') {
    Write-Host 'Windows ARM64 当前仅安装原生 CLI/TUI，没有 GUI、Python 分离环境和模型。'
    $required = 512MB
}
if ($Yes -and -not $InstallDir) { throw '-Yes 必须同时指定 -InstallDir' }
if (-not $InstallDir) {
    $drives = @(Get-PSDrive -PSProvider FileSystem | Where-Object { $null -ne $_.Free -and $_.Free -gt 0 })
    if ($drives.Count -eq 0) { throw '没有可用磁盘，请用 -InstallDir 指定本地安装目录' }
    Write-Host '请选择磁盘：程序、模型、下载缓存和临时文件都将使用该磁盘。'
    for ($index = 0; $index -lt $drives.Count; $index++) {
        Write-Host ("[{0}] {1}  剩余 {2:N1} GiB" -f ($index + 1), $drives[$index].Root, ($drives[$index].Free / 1GB))
    }
    $choice = Read-Host '输入磁盘编号'
    $number = 0
    if (-not [int]::TryParse($choice, [ref]$number) -or $number -lt 1 -or $number -gt $drives.Count) { throw '无效磁盘编号' }
    $defaultPath = Join-Path $drives[$number - 1].Root 'K3'
    $InstallDir = Read-Host "请输入安装目录 [$defaultPath]"
    if (-not $InstallDir) { $InstallDir = $defaultPath }
}
$InstallDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InstallDir)
if ($InstallDir.StartsWith('\\')) { throw '请安装到本机磁盘，不支持网络共享目录' }
if (Test-Path -LiteralPath $InstallDir) {
    $existing = Get-Item -LiteralPath $InstallDir -Force
    if (-not $existing.PSIsContainer -or ($existing.Attributes -band [IO.FileAttributes]::ReparsePoint) -or @(Get-ChildItem -LiteralPath $InstallDir -Force).Count -gt 0) { throw '安装目录必须不存在或为空，不能覆盖现有文件' }
}
$driveInfo = New-Object IO.DriveInfo ([IO.Path]::GetPathRoot($InstallDir))
if ($driveInfo.DriveType -eq [IO.DriveType]::Network) { throw '请安装到本机磁盘，不支持映射网络磁盘' }
if (-not $driveInfo.IsReady -or $driveInfo.AvailableFreeSpace -lt $required) { throw '所选磁盘空间不足，请换一个目录或磁盘' }
Write-Host "安装目录：$InstallDir"
Write-Host ("磁盘剩余：{0:N1} GiB；安装峰值预留：{1:N1} GiB" -f ($driveInfo.AvailableFreeSpace / 1GB), ($required / 1GB))
Write-Host '完整安装下载量约 1–2 GiB，安装后约 2–4 GiB；精简 CLI 安装明显更小。实际取决于平台和依赖。'
if (-not $Yes) {
    $answer = Read-Host '确认开始下载和安装？[y/N]'
    if ($answer -notin @('y', 'Y', 'yes', 'YES')) { Write-Host '已取消，未下载任何组件。'; return }
}
$parent = Split-Path -Parent $InstallDir
[void][IO.Directory]::CreateDirectory($parent)
$work = Join-Path $parent ('.k3-bootstrap-' + [Guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($work)
$variables = @('UV_CACHE_DIR', 'UV_PYTHON_INSTALL_DIR', 'UV_NO_CONFIG', 'TMPDIR', 'TMP', 'TEMP', 'PYTHONUTF8', 'VIRTUAL_ENV', 'PYTHONHOME', 'PYTHONPATH')
$previous = @{}
foreach ($name in $variables) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
$previousTls = [Net.ServicePointManager]::SecurityProtocol
$previousProgress = $ProgressPreference
try {
    $ProgressPreference = 'SilentlyContinue'
    $env:UV_CACHE_DIR = Join-Path $work 'cache'
    $env:UV_PYTHON_INSTALL_DIR = Join-Path $work 'python'
    $env:UV_NO_CONFIG = '1'
    $env:TMPDIR = Join-Path $work 'tmp'
    $env:TMP = $env:TMPDIR; $env:TEMP = $env:TMPDIR; $env:PYTHONUTF8 = '1'
    Remove-Item Env:VIRTUAL_ENV, Env:PYTHONHOME, Env:PYTHONPATH -ErrorAction SilentlyContinue
    [void][IO.Directory]::CreateDirectory($env:TMPDIR)
    [Net.ServicePointManager]::SecurityProtocol = $previousTls -bor [Net.SecurityProtocolType]::Tls12
    $archive = Join-Path $work 'uv.zip'
    Write-Host '下载已固定版本并校验的安装工具 uv 0.12.13'
    Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/astral-sh/uv/releases/download/0.12.13/uv-$machine-pc-windows-msvc.zip" -OutFile $archive -TimeoutSec 300
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $digest) { throw 'uv SHA-256 不匹配' }
    Expand-Archive -LiteralPath $archive -DestinationPath (Join-Path $work 'uv')
    $uv = Join-Path $work 'uv\uv.exe'
    & $uv --no-config python install 3.13.15 --install-dir $env:UV_PYTHON_INSTALL_DIR --no-bin --no-registry
    if ($LASTEXITCODE -ne 0) { throw '独立 Python 下载失败' }
    $installations = @(Get-ChildItem -LiteralPath $env:UV_PYTHON_INSTALL_DIR -Directory -Filter 'cpython-3.13.15-*')
    if ($installations.Count -ne 1) { throw '无法定位独立 Python' }
    $python = Join-Path $installations[0].FullName 'python.exe'
    # PowerShell 5.1 调用原生程序时会丢弃空字符串参数，使用显式哨兵。
    $sourceArgument = '-'
    if ($SourceDir) { $sourceArgument = $SourceDir }
    # 与 Unix 入口相同：API 下载、隔离认证、校验支持文件后才执行。
    $bootstrap = @'
import hashlib, json, os, re, shutil, sys, urllib.request, zipfile
from pathlib import Path, PurePosixPath
repo, version, directory, source = sys.argv[1:]
if source == '-': source = ''
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
'@
    & $python -I -c $bootstrap $Repo $Version $work $sourceArgument
    if ($LASTEXITCODE -ne 0) { throw '安装支持文件下载或校验失败' }
    $arguments = @('-I', (Join-Path $work 'support\scripts\install-online.py'), '--prefix', $InstallDir, '--repo', $Repo, '--uv', $uv)
    if ($SourceDir) { $arguments += @('--assets-dir', (Join-Path $SourceDir 'platform')) }
    if ($ModelCache) { $arguments += @('--model-cache', $ModelCache) }
    & $python @arguments
    if ($LASTEXITCODE -ne 0) { throw 'K3 安装或启动检查失败' }
} finally {
    foreach ($name in $variables) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
    [Net.ServicePointManager]::SecurityProtocol = $previousTls
    $ProgressPreference = $previousProgress
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
