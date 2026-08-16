<#
.SYNOPSIS
    Builds the shippable POE Trade Tracker preview and verifies it can run
    on a machine that is not this one.

.DESCRIPTION
    The payload is an explicit allow-list, not "whatever landed in
    target/release". That directory also holds a dozen probe binaries and
    build scratch; shipping it wholesale would leak developer tooling and
    quietly inflate the package.

    Two checks exist because a package that merely *builds* is not a package
    that *runs*:

      - The OCR model and dictionary are hashed against the values the
        recognition crate pins. A silently substituted model would degrade
        recognition with no error, since the ONNX fallback is optional by
        design and its absence looks like a quiet skip.

      - The layout is asserted against how the program actually resolves its
        assets at runtime (exe-adjacent `assets/ocr/`). Getting this wrong is
        invisible on the build machine, where the compile-time source-tree
        path still exists and the fallback keeps working.

.PARAMETER Configuration
    Cargo profile to build. Only `release` is shippable; `debug` exists so
    the script itself can be exercised quickly.
#>
[CmdletBinding()]
param(
    [ValidateSet('release', 'debug')]
    [string] $Configuration = 'release',

    [string] $OutputRoot = (Join-Path $PSScriptRoot '../target/package'),

    # Skips the cargo build and packages what is already there. For iterating
    # on this script, never for producing a release.
    [switch] $SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$TargetDirectory = Join-Path $RepositoryRoot "target/$Configuration"

# The package payload, by intent rather than by directory listing.
$Executables = @('ptt-app.exe')
$NativeLibraries = @('onnxruntime.dll')
$OcrAssets = @('PP-OCRv5_mobile_rec.onnx', 'ppocrv5_dict.txt')

function Assert-File([string] $Path, [string] $Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label not found: $Path"
    }
    return (Resolve-Path -LiteralPath $Path).Path
}

function Get-Sha256([string] $Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

<#
    Reads the hashes the recognition crate refuses to start without, so the
    package is verified against the program's own expectations rather than a
    number copied into this script that could drift from it.
#>
function Get-PinnedAssetHashes {
    $source = Join-Path $RepositoryRoot 'crates/ptt-ocr-onnx/src/assets.rs'
    $text = Get-Content -LiteralPath (Assert-File $source 'OCR asset pins') -Raw
    $pins = @{}
    foreach ($pair in @(
            @{ Constant = 'EXPECTED_MODEL_SHA256'; File = 'PP-OCRv5_mobile_rec.onnx' },
            @{ Constant = 'EXPECTED_DICTIONARY_SHA256'; File = 'ppocrv5_dict.txt' })) {
        $pattern = "$($pair.Constant)\s*:\s*&str\s*=\s*`"([0-9a-fA-F]{64})`""
        $match = [regex]::Match($text, $pattern)
        if (-not $match.Success) {
            throw "could not read $($pair.Constant) from $source"
        }
        $pins[$pair.File] = $match.Groups[1].Value.ToLowerInvariant()
    }
    return $pins
}

if (-not $SkipBuild) {
    Write-Host 'building release binaries...'
    Push-Location $RepositoryRoot
    try {
        & cargo build --release --locked -p ptt-app
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    }
    finally {
        Pop-Location
    }
}

$version = (Get-Content -LiteralPath (Join-Path $RepositoryRoot 'Cargo.toml') -Raw |
    Select-String -Pattern '(?m)^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$stageName = "poe-trade-tracker-$version-preview"
$stage = Join-Path $OutputRoot $stageName

if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage -Force | Out-Null
# Mirrors how the program resolves assets at runtime: beside the executable.
$assetStage = Join-Path $stage 'assets/ocr'
New-Item -ItemType Directory -Path $assetStage -Force | Out-Null
$licenseStage = Join-Path $stage 'licenses'
New-Item -ItemType Directory -Path $licenseStage -Force | Out-Null

$entries = New-Object System.Collections.Generic.List[object]

foreach ($name in ($Executables + $NativeLibraries)) {
    $source = Assert-File (Join-Path $TargetDirectory $name) "payload file $name"
    Copy-Item -LiteralPath $source -Destination (Join-Path $stage $name) -Force
    $entries.Add([pscustomobject]@{ Path = $name; Sha256 = (Get-Sha256 $source) })
}

$pins = Get-PinnedAssetHashes
foreach ($name in $OcrAssets) {
    $source = Assert-File (Join-Path $RepositoryRoot "assets/ocr/$name") "OCR asset $name"
    $actual = Get-Sha256 $source
    if ($pins.ContainsKey($name) -and $pins[$name] -ne $actual) {
        throw "OCR asset $name does not match the hash the recognition crate pins`n  expected $($pins[$name])`n  actual   $actual"
    }
    Copy-Item -LiteralPath $source -Destination (Join-Path $assetStage $name) -Force
    $entries.Add([pscustomobject]@{ Path = "assets/ocr/$name"; Sha256 = $actual })
}

foreach ($license in (Get-ChildItem -LiteralPath (Join-Path $RepositoryRoot 'licenses') -File)) {
    Copy-Item -LiteralPath $license.FullName -Destination (Join-Path $licenseStage $license.Name) -Force
    $entries.Add([pscustomobject]@{
            Path   = "licenses/$($license.Name)"
            Sha256 = (Get-Sha256 $license.FullName)
        })
}

$projectLicense = Assert-File (Join-Path $RepositoryRoot 'LICENSE.md') 'project license'
Copy-Item -LiteralPath $projectLicense -Destination (Join-Path $stage 'LICENSE.md') -Force
$entries.Add([pscustomobject]@{ Path = 'LICENSE.md'; Sha256 = (Get-Sha256 $projectLicense) })

# --- layout assertions: the package must match how the program looks -------
$expectedModel = Join-Path $stage 'assets/ocr/PP-OCRv5_mobile_rec.onnx'
if (-not (Test-Path -LiteralPath $expectedModel)) {
    throw 'the OCR model is not where the program looks for it (assets/ocr beside the exe)'
}
if (-not (Test-Path -LiteralPath (Join-Path $stage 'onnxruntime.dll'))) {
    throw 'onnxruntime.dll must sit beside the executable or the ONNX fallback silently disables itself'
}

$manifest = [pscustomobject]@{
    product       = 'POE Trade Tracker'
    version       = $version
    configuration = $Configuration
    builtAt       = (Get-Date).ToUniversalTime().ToString('o')
    files         = $entries | Sort-Object Path
}
$manifest | ConvertTo-Json -Depth 4 |
    Set-Content -LiteralPath (Join-Path $stage 'MANIFEST.json') -Encoding UTF8

$bytes = (Get-ChildItem -LiteralPath $stage -Recurse -File | Measure-Object -Property Length -Sum).Sum
$mib = [math]::Round($bytes / 1MB, 1)
$limitMib = 55
Write-Host ''
Write-Host "staged $stage"
Write-Host "$($entries.Count) files, $mib MiB (limit $limitMib MiB)"
if ($mib -gt $limitMib) {
    throw "package is $mib MiB, over the $limitMib MiB budget"
}

$zip = Join-Path $OutputRoot "$stageName.zip"
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
Write-Host "archive $zip ($([math]::Round((Get-Item -LiteralPath $zip).Length / 1MB, 1)) MiB compressed)"
Write-Host ''
Write-Host 'package verified: payload allow-listed, OCR assets match their pins, layout matches runtime lookup'
