# What a release binary weighs, against the baseline recorded in docs/electron/size.md, and
# the crates it carries. Builds exactly one configuration; CI fails past -MaxBytes.
#
#   scripts/size-report.ps1                                   # default features
#   scripts/size-report.ps1 -Features wasm                    # the Electron feature set
#   scripts/size-report.ps1 -Profile dist -MaxBytes 25165824  # the shipping budget
param(
    [string]$Bin = "verve",
    [string[]]$Features = @(),
    [ValidateSet("release", "dist")]
    [string]$Profile = "release",
    [int]$MaxBytes = 0,
    [switch]$NoBloat
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$featureArgs = @()
if ($Features.Count -gt 0 -and $Features -ne @("none")) {
    $featureArgs = @("--no-default-features", "--features", ($Features -join ","))
} elseif ($Features -eq @("none")) {
    $featureArgs = @("--no-default-features")
}

$shown = if ($Features.Count -gt 0) { $Features -join "," } else { "default features" }
Write-Host "building $Bin ($Profile) with $shown"
cargo build --profile $Profile --bin $Bin @featureArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$binary = Join-Path $root "target/$Profile/$Bin.exe"
if (-not (Test-Path $binary)) { $binary = Join-Path $root "target/$Profile/$Bin" }
$bytes = (Get-Item $binary).Length
$mib = [math]::Round($bytes / 1MB, 2)

$baseline = $null
$sizes = Join-Path $root "docs/electron/size.md"
if (Test-Path $sizes) {
    $match = Select-String -Path $sizes -Pattern "size-baseline $([regex]::Escape($Bin)) (\d+)" |
        Select-Object -First 1
    if ($match) { $baseline = [int64]$match.Matches[0].Groups[1].Value }
}

$line = "$Bin ($mib MiB, $bytes bytes)"
if ($baseline) {
    $delta = $bytes - $baseline
    $percent = [math]::Round(100 * $delta / $baseline, 1)
    $line += "; baseline $baseline bytes, change $delta bytes ($percent%)"
}
Write-Host $line

if (-not $NoBloat) {
    $bloatArgs = @("bloat", "--profile", $Profile, "--bin", $Bin, "--crates", "-n", "20")
    $bloatArgs += $featureArgs
    cargo @bloatArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

if ($MaxBytes -gt 0 -and $bytes -gt $MaxBytes) {
    Write-Host "$Bin is $bytes bytes, over the $MaxBytes-byte budget" -ForegroundColor Red
    exit 1
}
