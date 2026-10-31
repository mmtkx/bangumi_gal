<#
.SYNOPSIS
    Build the distributable exe without leaking the build machine's paths.

.DESCRIPTION
    Rust bakes source paths into panic locations. A plain `cargo build --release`
    therefore ships strings such as
      C:\Users\<your-name>\.cargo\registry\src\...
      B:\...\LAGM\crates\app-services\src\services.rs
    which reveals the build machine's user name and directory layout to anyone
    who runs `strings` on the binary.

    This script sets --remap-path-prefix for the user profile and the repo root
    before building, so those paths read as `cargo/...` and `repo/...` instead.

    Paths containing spaces are rejected: RUSTFLAGS is split on whitespace and
    would break the flag silently.

.PARAMETER Bundled
    Also build the NSIS installer (pnpm tauri build without --no-bundle).

.EXAMPLE
    pwsh -File scripts/build-release.ps1

.EXAMPLE
    pwsh -File scripts/build-release.ps1 -Bundled
#>
[CmdletBinding()]
param(
    [switch]$Bundled
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$appDir = Join-Path $root 'apps/desktop'

$repoPrefix = $root.TrimEnd('\', '/')
$profilePrefix = $env:USERPROFILE.TrimEnd('\', '/')

if (-not $profilePrefix) {
    throw 'USERPROFILE is not set.'
}

foreach ($path in @($repoPrefix, $profilePrefix)) {
    if ($path -match '\s') {
        throw "path contains whitespace, RUSTFLAGS remap would be ambiguous: $path"
    }
}

# 先退出正在运行的 LAGM，否则 Windows 会因文件占用让链接器失败或留下垃圾产物。
$running = Get-Process -Name 'lagm-desktop' -ErrorAction SilentlyContinue
if ($running) {
    throw 'lagm-desktop is running. Close it before building (Windows keeps handles on target/).'
}

$env:RUSTFLAGS = @(
    "--remap-path-prefix=$profilePrefix=cargo",
    "--remap-path-prefix=$repoPrefix=repo"
) -join ' '

Write-Host "RUSTFLAGS=$env:RUSTFLAGS" -ForegroundColor DarkGray

Push-Location $appDir
try {
    $tauriArgs = @('tauri', 'build')
    if (-not $Bundled) { $tauriArgs += '--no-bundle' }

    & pnpm @tauriArgs
    if ($LASTEXITCODE -ne 0) {
        throw "pnpm $($tauriArgs -join ' ') failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}

$exe = Join-Path $root 'target/release/lagm-desktop.exe'
if (Test-Path -LiteralPath $exe) {
    $item = Get-Item -LiteralPath $exe
    $sizeMb = [math]::Round($item.Length / 1MB, 1)
    Write-Host ("built: {0} ({1} MB, {2:yyyy-MM-dd HH:mm:ss})" -f $exe, $sizeMb, $item.LastWriteTime) -ForegroundColor Green

    # 二进制里按 ISO-8859-1 解码后做子串查找：字节与字符一一对应，不会破坏匹配。
    $text = [System.Text.Encoding]::Latin1.GetString([System.IO.File]::ReadAllBytes($exe))
    $leaks = @()
    foreach ($needle in @($profilePrefix, 'C:\Users\', $repoPrefix)) {
        if ($text.Contains($needle)) { $leaks += $needle }
    }
    if ($leaks.Count -eq 0) {
        Write-Host 'path leak check: clean' -ForegroundColor Green
    }
    else {
        Write-Host "path leak check: still contains $($leaks -join ', ')" -ForegroundColor Yellow
    }
}

# 安装包归档到 release-package/：对外分发只需要这一个文件，
# 放在固定位置免得每次到 target 深处翻找。
if ($Bundled) {
    $nsisDir = Join-Path $root 'target/release/bundle/nsis'
    $installer = if (Test-Path -LiteralPath $nsisDir) {
        Get-ChildItem -LiteralPath $nsisDir -Filter '*-setup.exe' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    }
    if ($installer) {
        $packageDir = Join-Path $root 'release-package'
        New-Item -ItemType Directory -Force -Path $packageDir | Out-Null
        # 版本号变化后旧安装包留着会混淆，先清掉同目录的历史安装包。
        Get-ChildItem -LiteralPath $packageDir -Filter '*-setup.exe' | Remove-Item -Force
        $target = Join-Path $packageDir $installer.Name
        Copy-Item -LiteralPath $installer.FullName -Destination $target -Force
        Write-Host ("installer: {0} ({1:N1} MB)" -f $target, ($installer.Length / 1MB)) -ForegroundColor Green
    }
    else {
        Write-Host 'installer: 未找到 NSIS 产物（检查 -Bundled 是否生效）' -ForegroundColor Yellow
    }
}
