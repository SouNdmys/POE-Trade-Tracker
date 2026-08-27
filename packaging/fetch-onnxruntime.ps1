<#
.SYNOPSIS
    Puts `onnxruntime.dll` beside the built executable, fetching it from
    upstream if it is not already there.

.DESCRIPTION
    The ONNX fallback needs a native runtime that cargo never produces:
    `ptt-ocr-onnx` is built with ort's `load-dynamic` and
    `default-features = false`, so nothing downloads the library and nothing
    links it. Until now the DLL simply *was* in `target/release` on the
    author's machine, hand-placed once and never recorded — which meant a
    `cargo clean`, or a fresh clone on any other machine, silently lost the
    ability to produce a working release. `package-preview.ps1` catches that,
    but only at the very end and only with "not found".

    So the provenance lives here instead: a pinned version, a pinned hash of
    the exact file that ships, and the upstream URL it came from. The DLL is
    fetched rather than committed — it is Microsoft's redistributable with a
    stable release URL, and a 16 MB binary that can be reproduced from a hash
    does not need to be in everyone's clone. The hash is what makes that safe:
    a substituted library would be a native code-execution surface, so the
    file is verified before it is ever placed next to the executable.

    Idempotent. If the DLL is already in place with the right hash, this does
    nothing and says so.

.PARAMETER Configuration
    Which target directory to populate. `release` is what ships; `debug`
    exists because the ONNX fallback is just as absent there.

.PARAMETER Force
    Re-fetch even when a correct copy is already in place.
#>
[CmdletBinding()]
param(
    [ValidateSet('release', 'debug')]
    [string] $Configuration = 'release',

    [switch] $Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Pinned to the build the OCR fallback was calibrated against. Bumping this
# means re-running P1's calibration corpus, not just editing two lines.
$OnnxRuntimeVersion = '1.28.0'
$PackageUrl = "https://github.com/microsoft/onnxruntime/releases/download/v$OnnxRuntimeVersion/onnxruntime-win-x64-$OnnxRuntimeVersion.zip"
# SHA-256 of the extracted DLL, not of the archive: the archive is the
# delivery mechanism, the DLL is the thing that ends up executing.
$ExpectedSha256 = '18370c375f07357fa5874344a9d9ac17e6b6fe1eb18b1dd209d79483b4470257'
$EntryPath = "onnxruntime-win-x64-$OnnxRuntimeVersion/lib/onnxruntime.dll"

$RepositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$TargetDirectory = Join-Path $RepositoryRoot "target/$Configuration"
$Destination = Join-Path $TargetDirectory 'onnxruntime.dll'

function Get-Sha256([string] $Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

if ((Test-Path -LiteralPath $Destination -PathType Leaf) -and -not $Force) {
    $actual = Get-Sha256 $Destination
    if ($actual -eq $ExpectedSha256) {
        Write-Host "onnxruntime.dll $OnnxRuntimeVersion already in place ($Configuration)."
        exit 0
    }
    # A wrong hash is worth saying out loud rather than silently overwriting:
    # it usually means someone dropped a different runtime in by hand.
    Write-Warning "onnxruntime.dll present but hashes $actual, expected $ExpectedSha256 - replacing."
}

if (-not (Test-Path -LiteralPath $TargetDirectory -PathType Container)) {
    New-Item -ItemType Directory -Path $TargetDirectory -Force | Out-Null
}

$scratch = Join-Path ([System.IO.Path]::GetTempPath()) ("ptt-ort-" + [System.Guid]::NewGuid().ToString('n'))
New-Item -ItemType Directory -Path $scratch -Force | Out-Null
try {
    $archive = Join-Path $scratch 'onnxruntime.zip'
    Write-Host "Fetching $PackageUrl"
    # ~79 MB; the progress bar makes Invoke-WebRequest an order of magnitude
    # slower on Windows PowerShell, so it is off deliberately.
    $previousProgress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    try {
        Invoke-WebRequest -Uri $PackageUrl -OutFile $archive -UseBasicParsing
    } finally {
        $ProgressPreference = $previousProgress
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
    try {
        # Entry separators differ by producer, so match on the normalised
        # form rather than trusting the archive's spelling.
        $entry = $zip.Entries | Where-Object { $_.FullName.Replace('\', '/') -eq $EntryPath }
        if (-not $entry) {
            throw "archive does not contain $EntryPath - upstream layout changed"
        }
        $staged = Join-Path $scratch 'onnxruntime.dll'
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $staged, $true)

        # Verified in the scratch directory, before anything is placed where
        # the program would load it.
        $actual = Get-Sha256 $staged
        if ($actual -ne $ExpectedSha256) {
            throw "onnxruntime.dll hashes $actual, expected $ExpectedSha256 - refusing to install it"
        }
        Copy-Item -LiteralPath $staged -Destination $Destination -Force
    } finally {
        $zip.Dispose()
    }
} finally {
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "onnxruntime.dll $OnnxRuntimeVersion installed at $Destination"
