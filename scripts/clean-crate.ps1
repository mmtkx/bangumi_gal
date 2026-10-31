<#
.SYNOPSIS
    Targeted cleanup of build artifacts, plus a size report.

.DESCRIPTION
    With -Package, runs `cargo clean -p <name>` for each package that changed,
    which removes only that package's artifacts instead of the whole target/.

    Without arguments it prints where the space went, so you can pick targets.

    Only ever touches target/. Never sources, the database or media files.

.PARAMETER Package
    Cargo package names to clean, for example app-services, library-core.

.PARAMETER Incremental
    Delete target/debug/incremental only. That folder is pure compiler cache
    (regenerated on demand) and is the usual 10G+ offender; removing it never
    touches sources or other crates' artifacts.

.EXAMPLE
    pwsh -File scripts/clean-crate.ps1

.EXAMPLE
    pwsh -File scripts/clean-crate.ps1 -Package app-services,bangumi-sync

.EXAMPLE
    pwsh -File scripts/clean-crate.ps1 -Incremental
#>
[CmdletBinding()]
param(
    [string[]]$Package,
    [switch]$Incremental
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$target = Join-Path $root 'target'

function Get-DirSizeGb {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return 0 }
    $sum = (Get-ChildItem -LiteralPath $Path -Recurse -File -ErrorAction SilentlyContinue |
        Measure-Object -Property Length -Sum).Sum
    if ($null -eq $sum) { return 0 }
    return [math]::Round($sum / 1GB, 2)
}

if (-not $Package -and -not $Incremental) {
    if (-not (Test-Path -LiteralPath $target)) {
        Write-Host 'target/ does not exist, nothing to report.'
        return
    }

    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add(("target/ total: {0} GB" -f (Get-DirSizeGb $target)))
    $lines.Add('breakdown (GB):')
    foreach ($profile in @('debug', 'release')) {
        $profileDir = Join-Path $target $profile
        if (-not (Test-Path -LiteralPath $profileDir)) { continue }
        $lines.Add(("  {0,-10}{1,7:N2}" -f $profile, (Get-DirSizeGb $profileDir)))
        $rows = foreach ($child in (Get-ChildItem -LiteralPath $profileDir -Directory -ErrorAction SilentlyContinue)) {
            [pscustomobject]@{ Name = $child.Name; GB = (Get-DirSizeGb $child.FullName) }
        }
        foreach ($row in ($rows | Sort-Object GB -Descending | Select-Object -First 5)) {
            if ($row.GB -lt 0.01) { continue }
            $lines.Add(("    {0,-12}{1,7:N2}" -f $row.Name, $row.GB))
        }
    }
    $lines.Add('')
    $lines.Add('clean specific crates: pwsh -File scripts/clean-crate.ps1 -Package app-services,library-core')
    $lines.Add('drop stale incremental cache: pwsh -File scripts/clean-crate.ps1 -Incremental')
    Write-Host ($lines -join [Environment]::NewLine)
    return
}

if ($Incremental) {
    $incrementalDir = Join-Path $target 'debug/incremental'
    if (-not (Test-Path -LiteralPath $incrementalDir)) {
        Write-Host 'no target/debug/incremental, nothing to do.'
        return
    }
    $before = Get-DirSizeGb $incrementalDir
    Write-Host "removing $incrementalDir ($before GB of pure compiler cache)"
    Remove-Item -LiteralPath $incrementalDir -Recurse -Force -ErrorAction Stop
    Write-Host ("done. target/ total now: {0} GB" -f (Get-DirSizeGb $target)) -ForegroundColor Green
    return
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $env:PATH = "$HOME\.cargo\bin;$env:PATH"
}

Push-Location $root
try {
    foreach ($name in $Package) {
        Write-Host "== cargo clean -p $name ==" -ForegroundColor Cyan
        & cargo clean -p $name
        if ($LASTEXITCODE -ne 0) {
            Write-Host "FAILED: cargo clean -p $name" -ForegroundColor Red
            exit $LASTEXITCODE
        }
    }
    Write-Host ("target/ total now: {0} GB" -f (Get-DirSizeGb $target)) -ForegroundColor Green
}
finally {
    Pop-Location
}
