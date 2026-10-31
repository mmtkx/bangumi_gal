<#
.SYNOPSIS
    Single-pass verification: fmt check, clippy, tests.

.DESCRIPTION
    Runs everything that must be green before handing work off, in one go.
    It deliberately does NOT run `cargo build`: clippy and test already
    produce the artifacts needed, and a separate build only rewrites them,
    which is the main source of repeated target/ churn.

.PARAMETER Frontend
    Also run pnpm format:check, typecheck, lint and build.
    Use only when the frontend actually changed.

.EXAMPLE
    pwsh -File scripts/verify.ps1

.EXAMPLE
    pwsh -File scripts/verify.ps1 -Frontend
#>
[CmdletBinding()]
param(
    [switch]$Frontend
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        $env:PATH = "$HOME\.cargo\bin;$env:PATH"
    }

    $steps = @(
        @{ Name = 'fmt'; Args = @('fmt', '--all', '--', '--check') },
        @{ Name = 'clippy'; Args = @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings') },
        @{ Name = 'test'; Args = @('test', '--workspace') }
    )

    foreach ($step in $steps) {
        Write-Host "== cargo $($step.Name) ==" -ForegroundColor Cyan
        & cargo @($step.Args)
        if ($LASTEXITCODE -ne 0) {
            Write-Host "FAILED: cargo $($step.Name)" -ForegroundColor Red
            exit $LASTEXITCODE
        }
    }

    if ($Frontend) {
        $appDir = Join-Path $root 'apps/desktop'
        foreach ($script in @('format:check', 'typecheck', 'lint', 'build')) {
            Write-Host "== pnpm $script ==" -ForegroundColor Cyan
            & pnpm --dir $appDir $script
            if ($LASTEXITCODE -ne 0) {
                Write-Host "FAILED: pnpm $script" -ForegroundColor Red
                exit $LASTEXITCODE
            }
        }
    }

    Write-Host 'ALL CHECKS PASSED' -ForegroundColor Green
}
finally {
    Pop-Location
}
