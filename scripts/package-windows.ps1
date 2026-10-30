param(
    [string]$InnoCompiler
)

$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$dist = Join-Path $repo 'dist'
$staging = Join-Path $repo 'target/package-windows'

function Assert-RepoChild([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    $prefix = $repo.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $full.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to modify a path outside the repository: $full"
    }
}

Assert-RepoChild $dist
Assert-RepoChild $staging

$manifest = Get-Content -Raw (Join-Path $repo 'Cargo.toml')
$match = [regex]::Match($manifest, '(?m)^version = "([^"]+)"\r?$')
if (-not $match.Success) { throw 'Could not read the package version from Cargo.toml' }
$version = $match.Groups[1].Value

& cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }

if (Test-Path -LiteralPath $dist) { Remove-Item -LiteralPath $dist -Recurse -Force }
if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
New-Item -ItemType Directory -Path $dist, $staging | Out-Null

$builtExe = Join-Path $repo 'target/release/openmixer.exe'
Copy-Item -LiteralPath $builtExe -Destination (Join-Path $dist 'openmixer-windows-x64.exe')
Copy-Item -LiteralPath $builtExe -Destination (Join-Path $staging 'openmixer.exe')
Copy-Item -LiteralPath (Join-Path $repo 'README.md') -Destination $staging
Copy-Item -LiteralPath (Join-Path $repo 'LICENSE') -Destination $staging
Compress-Archive -Path (Join-Path $staging '*') -DestinationPath (Join-Path $dist "openmixer-v$version-windows-x64.zip") -CompressionLevel Optimal

if (-not $InnoCompiler) {
    $registryRoots = @(
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
    )
    $install = Get-ItemProperty $registryRoots -ErrorAction SilentlyContinue |
        Where-Object DisplayName -Like 'Inno Setup*' |
        Select-Object -First 1
    if ($install.InstallLocation) {
        $InnoCompiler = Join-Path $install.InstallLocation 'ISCC.exe'
    }
}
if (-not $InnoCompiler -or -not (Test-Path -LiteralPath $InnoCompiler)) {
    throw 'Inno Setup 6 was not found. Install JRSoftware.InnoSetup or pass -InnoCompiler.'
}

& $InnoCompiler "/DMyAppVersion=$version" (Join-Path $repo 'packaging/openmixer.iss')
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup compilation failed' }

& (Join-Path $PSScriptRoot 'test-icon.ps1')
& (Join-Path $PSScriptRoot 'test-package.ps1')
