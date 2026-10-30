$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$manifest = Get-Content -Raw (Join-Path $repo 'Cargo.toml')
$match = [regex]::Match($manifest, '(?m)^version = "([^"]+)"\r?$')
if (-not $match.Success) { throw 'Could not read the package version from Cargo.toml' }
$version = $match.Groups[1].Value
$dist = Join-Path $repo 'dist'
$expected = @(
    (Join-Path $dist 'openmixer-windows-x64.exe'),
    (Join-Path $dist "openmixer-v$version-windows-x64.zip"),
    (Join-Path $dist "openmixer-v$version-windows-x64-setup.exe")
)

foreach ($path in $expected) {
    if (-not (Test-Path -LiteralPath $path)) { throw "Missing package: $path" }
    if ((Get-Item -LiteralPath $path).Length -eq 0) { throw "Empty package: $path" }
}

$archive = [IO.Compression.ZipFile]::OpenRead($expected[1])
try {
    $entries = $archive.Entries.Name
    foreach ($required in 'openmixer.exe', 'README.md', 'LICENSE') {
        if ($required -notin $entries) { throw "Portable ZIP is missing $required" }
    }
} finally {
    $archive.Dispose()
}

Write-Host "Windows packages valid for OpenMixer v$version"
