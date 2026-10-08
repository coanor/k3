# Windows 在线安装入口，兼容 Windows PowerShell 5.1。
[CmdletBinding()]
param(
    [string]$InstallDir,
    [string]$Repo = 'coanor/k3',
    [string]$Version = 'latest',
    [string]$SourceDir,
    [string]$ModelCache,
    [switch]$Update,
    [switch]$DesktopShortcut,
    [switch]$NoDesktopShortcut,
    [switch]$Yes
)
$ErrorActionPreference = 'Stop'
if ($DesktopShortcut -and $NoDesktopShortcut) { throw 'Choose only one desktop shortcut option' }
if ($Repo -notmatch '^[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+$') { throw 'Repository must use owner/repo format' }
if ($Version -ne 'latest' -and $Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') { throw 'Version must be latest or vX.Y.Z' }
$architecture = [Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITECTURE', 'Machine')
if (-not $architecture) { $architecture = $env:PROCESSOR_ARCHITEW6432; if (-not $architecture) { $architecture = $env:PROCESSOR_ARCHITECTURE } }
switch ($architecture) {
    'AMD64' { $machine = 'x86_64'; $digest = 'a86c9dc7bad9b03f388583b7187c05fe9951c2e0d392217e8fd43d97787f6ec2' }
    'ARM64' { $machine = 'aarch64'; $digest = '1efb2654b06e7063d4ac1fc9d49a9bda9a6704d82f035b589a2751a592f14151' }
    default { throw 'Only Windows x64 and ARM64 are supported' }
}
$os = [Version](Get-CimInstance Win32_OperatingSystem).Version
if ($os.Major -lt 10 -or ($machine -eq 'aarch64' -and $os.Build -lt 22000)) { throw 'Windows 10/11 is required; ARM64 requires Windows 11' }
$required = 12GB
Write-Host "Target platform: Windows $machine"
if ($machine -eq 'aarch64') {
    Write-Host 'Windows ARM64 currently installs native CLI/TUI only, without a GUI, Python separation runtime or models.'
    $required = 512MB
}
function Find-K3OnlineInstallations {
    $found = New-Object 'System.Collections.Generic.List[string]'
    $seen = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    function Add-K3Installation([string]$Candidate) {
        if (-not $Candidate) { return }
        try {
            $item = Get-Item -LiteralPath $Candidate -Force -ErrorAction Stop
            if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { return }
            $binary = Get-Item -LiteralPath (Join-Path $item.FullName 'k3.exe') -Force -ErrorAction Stop
            if ($binary.PSIsContainer -or ($binary.Attributes -band [IO.FileAttributes]::ReparsePoint)) { return }
            $metadata = $false
            foreach ($name in @('installation-state.json', 'install-manifest.json')) {
                $file = Get-Item -LiteralPath (Join-Path $item.FullName $name) -Force -ErrorAction SilentlyContinue
                if ($file -and -not $file.PSIsContainer -and -not ($file.Attributes -band [IO.FileAttributes]::ReparsePoint)) { $metadata = $true }
            }
            if ($metadata -and $seen.Add($item.FullName)) { $found.Add($item.FullName) }
        } catch { }
    }
    Add-K3Installation $PWD.Path
    Add-K3Installation $PSScriptRoot
    foreach ($key in @(Get-ChildItem -LiteralPath 'HKCU:\Software\K3\OnlineInstallations' -ErrorAction SilentlyContinue)) {
        $record = Get-ItemProperty -LiteralPath $key.PSPath -ErrorAction SilentlyContinue
        if ($record.InstallDir -is [string]) { Add-K3Installation $record.InstallDir }
    }
    foreach ($parent in @($env:LOCALAPPDATA, $env:ProgramFiles, $env:USERPROFILE)) {
        if ($parent) { Add-K3Installation (Join-Path $parent 'K3') }
    }
    if ($env:LOCALAPPDATA) { Add-K3Installation (Join-Path $env:LOCALAPPDATA 'Programs\K3') }
    foreach ($drive in @(Get-PSDrive -PSProvider FileSystem)) { Add-K3Installation (Join-Path $drive.Root 'K3') }
    foreach ($command in @(Get-Command k3.exe -CommandType Application -All -ErrorAction SilentlyContinue)) {
        Add-K3Installation (Split-Path -Parent $command.Source)
    }
    try {
        $shortcutFiles = @()
        foreach ($folder in @([Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('DesktopDirectory'))) {
            if ($folder) { $shortcutFiles += @(Get-ChildItem -LiteralPath $folder -Filter 'K3-*.lnk' -File -ErrorAction SilentlyContinue) }
        }
        # Read the Unicode shell-link interface; WScript.Shell loses some Unicode paths.
        # Compile the reader only when K3 shortcuts actually exist.
        if ($shortcutFiles.Count -gt 0 -and -not ('K3InstallationShortcut' -as [type])) {
            Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Text;
[ComImport, Guid("00021401-0000-0000-C000-000000000046")]
internal class K3DiscoveryLink { }
[ComImport, Guid("000214F9-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
internal interface IK3DiscoveryLinkW {
    void GetPath([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path, int count, IntPtr data, uint flags);
}
public static class K3InstallationShortcut {
    public static string Target(string path) {
        object instance = new K3DiscoveryLink();
        try {
            ((IPersistFile)instance).Load(path, 0);
            StringBuilder target = new StringBuilder(32768);
            ((IK3DiscoveryLinkW)instance).GetPath(target, target.Capacity, IntPtr.Zero, 4);
            return target.ToString();
        } finally { Marshal.FinalReleaseComObject(instance); }
    }
}
'@
        }
        foreach ($shortcut in $shortcutFiles) {
            try {
                $target = [K3InstallationShortcut]::Target($shortcut.FullName)
                if ($target -and [IO.Path]::GetFileName($target) -eq 'k3-gui.exe') { Add-K3Installation (Split-Path -Parent $target) }
            } catch { }
        }
    } catch { }
    return $found.ToArray()
}
if ($Yes -and -not $InstallDir) { throw '-Yes requires -InstallDir' }
if (-not $InstallDir) {
    $installations = @(Find-K3OnlineInstallations)
    if ($installations.Count -eq 1) {
        $InstallDir = $installations[0]
        $Update = $true
        Write-Host "Found existing K3 installation: $InstallDir"
    } elseif ($installations.Count -gt 1) {
        Write-Host 'Multiple K3 installations were found:'
        for ($index = 0; $index -lt $installations.Count; $index++) { Write-Host ("[{0}] {1}" -f ($index + 1), $installations[$index]) }
        Write-Host '[0] Enter another installation directory'
        $choice = Read-Host 'Select installation number'
        $number = 0
        if (-not [int]::TryParse($choice, [ref]$number) -or $number -lt 0 -or $number -gt $installations.Count) { throw 'Invalid installation number' }
        if ($number -gt 0) { $InstallDir = $installations[$number - 1]; $Update = $true }
    }
}
if (-not $InstallDir) {
    $drives = @(Get-PSDrive -PSProvider FileSystem | Where-Object { $null -ne $_.Free -and $_.Free -gt 0 })
    if ($drives.Count -eq 0) { throw 'No disks are available; use -InstallDir to select a local installation directory' }
    Write-Host 'Select a disk for programs, models, download cache and temporary files.'
    for ($index = 0; $index -lt $drives.Count; $index++) {
        Write-Host ("[{0}] {1}  Free: {2:N1} GiB" -f ($index + 1), $drives[$index].Root, ($drives[$index].Free / 1GB))
    }
    $choice = Read-Host 'Enter disk number'
    $number = 0
    if (-not [int]::TryParse($choice, [ref]$number) -or $number -lt 1 -or $number -gt $drives.Count) { throw 'Invalid disk number' }
    $defaultPath = Join-Path $drives[$number - 1].Root 'K3'
    $InstallDir = Read-Host "Enter installation directory [$defaultPath]"
    if (-not $InstallDir) { $InstallDir = $defaultPath }
}
$InstallDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InstallDir)
if ($InstallDir.StartsWith('\\')) { throw 'Install on a local disk; network shares are unsupported' }
if ((Test-Path -LiteralPath (Join-Path $InstallDir 'installation-state.json') -PathType Leaf) -or
    (Test-Path -LiteralPath (Join-Path $InstallDir 'install-manifest.json') -PathType Leaf)) { $Update = $true }
if ($Update) {
    Write-Host 'Close all K3 windows, TUI sessions and separation jobs before updating.'
    $existing = Get-Item -LiteralPath $InstallDir -Force
    if (-not $existing.PSIsContainer -or ($existing.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        (-not (Test-Path -LiteralPath (Join-Path $InstallDir 'installation-state.json')) -and
         -not (Test-Path -LiteralPath (Join-Path $InstallDir 'install-manifest.json')))) { throw 'Update requires a registered online installation directory' }
} elseif (Test-Path -LiteralPath $InstallDir) {
    $existing = Get-Item -LiteralPath $InstallDir -Force
    if (-not $existing.PSIsContainer -or ($existing.Attributes -band [IO.FileAttributes]::ReparsePoint) -or @(Get-ChildItem -LiteralPath $InstallDir -Force).Count -gt 0) { throw 'Installation directory must be absent or empty; existing files cannot be overwritten' }
}
$driveInfo = New-Object IO.DriveInfo ([IO.Path]::GetPathRoot($InstallDir))
if ($driveInfo.DriveType -eq [IO.DriveType]::Network) { throw 'Install on a local disk; mapped network drives are unsupported' }
if (-not $driveInfo.IsReady -or $driveInfo.AvailableFreeSpace -lt $required) { throw 'Insufficient space on the selected disk; choose another directory or disk' }
Write-Host "Installation directory: $InstallDir"
Write-Host ("Free disk space: {0:N1} GiB; reserved space for peak installation usage: {1:N1} GiB" -f ($driveInfo.AvailableFreeSpace / 1GB), ($required / 1GB))
Write-Host 'A full installation downloads about 1-5 GiB and uses about 2-10 GiB, depending on GPU support. CLI-only installations are smaller.'
if (-not $Yes) {
    $prompt = 'Start downloading and installing? [y/N]'
    if ($Update) { $prompt = 'Download and update all K3 components? [y/N]' }
    $answer = Read-Host $prompt
    if ($answer -notin @('y', 'Y', 'yes', 'YES')) { Write-Host 'Cancelled. No components were downloaded.'; return }
}
if (-not $Update -and $machine -eq 'x86_64' -and -not $Yes -and -not $DesktopShortcut -and -not $NoDesktopShortcut) {
    $answer = Read-Host 'Create a desktop shortcut? [y/N]'
    $DesktopShortcut = $answer -in @('y', 'Y', 'yes', 'YES')
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
    Write-Host 'Downloading pinned installer tool uv 0.12.13 with checksum verification'
    Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/astral-sh/uv/releases/download/0.12.13/uv-$machine-pc-windows-msvc.zip" -OutFile $archive -TimeoutSec 300
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $digest) { throw 'uv SHA-256 mismatch' }
    Expand-Archive -LiteralPath $archive -DestinationPath (Join-Path $work 'uv')
    $uv = Join-Path $work 'uv\uv.exe'
    # uv 在 Windows ARM64 上默认选用仿真 x64 Python；固定原生架构以保持平台识别一致。
    & $uv --no-config python install "cpython-3.13.15-windows-$machine-none" --install-dir $env:UV_PYTHON_INSTALL_DIR --no-bin --no-registry
    if ($LASTEXITCODE -ne 0) { throw 'Standalone Python download failed' }
    $installations = @(Get-ChildItem -LiteralPath $env:UV_PYTHON_INSTALL_DIR -Directory -Filter 'cpython-3.13.15-*')
    if ($installations.Count -ne 1) { throw 'Could not locate standalone Python' }
    $python = Join-Path $installations[0].FullName 'python.exe'
    # PowerShell 5.1 调用原生程序时会丢弃空字符串参数，使用显式哨兵。
    $sourceArgument = '-'
    if ($SourceDir) { $sourceArgument = $SourceDir }
    # Public downloads avoid the API; authenticated private downloads retain isolated credentials.
    $bootstrap = @'
import hashlib, json, os, re, shutil, sys, urllib.error, urllib.request, zipfile
from pathlib import Path, PurePosixPath
repo, version, directory, source = sys.argv[1:]
if source == '-': source = ''
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
'@
    & $python -I -c $bootstrap $Repo $Version $work $sourceArgument
    if ($LASTEXITCODE -ne 0) { throw 'Installation support file download or verification failed' }
    $arguments = @('-I', (Join-Path $work 'support\scripts\install-online.py'), '--prefix', $InstallDir, '--repo', $Repo, '--uv', $uv, '--management-python', $installations[0].FullName)
    if ($Update) { $arguments += '--update' }
    if ($SourceDir) { $arguments += @('--assets-dir', (Join-Path $SourceDir 'platform')) }
    if ($DesktopShortcut) { $arguments += '--desktop-shortcut' }
    if ($ModelCache) { $arguments += @('--model-cache', $ModelCache) }
    & $python @arguments
    if ($LASTEXITCODE -ne 0) { throw 'K3 installation or startup check failed' }
} finally {
    foreach ($name in $variables) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
    [Net.ServicePointManager]::SecurityProtocol = $previousTls
    $ProgressPreference = $previousProgress
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
