$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$svg = Join-Path $repo 'assets/openmixer.svg'
$ico = Join-Path $repo 'assets/openmixer.ico'
$png = Join-Path $repo 'assets/openmixer.png'

if (-not (Test-Path -LiteralPath $svg)) { throw 'Missing assets/openmixer.svg' }
if (-not (Test-Path -LiteralPath $ico)) { throw 'Missing assets/openmixer.ico' }
if (-not (Test-Path -LiteralPath $png)) { throw 'Missing assets/openmixer.png' }

$bytes = [IO.File]::ReadAllBytes($ico)
if ($bytes.Length -lt 6 -or [BitConverter]::ToUInt16($bytes, 2) -ne 1) {
    throw 'openmixer.ico is not a valid ICO container'
}
$count = [BitConverter]::ToUInt16($bytes, 4)
$sizes = for ($index = 0; $index -lt $count; $index++) {
    $width = $bytes[6 + ($index * 16)]
    if ($width -eq 0) { 256 } else { [int]$width }
}
foreach ($required in 16, 32, 48, 256) {
    if ($required -notin $sizes) { throw "ICO is missing ${required}x${required}" }
}

Write-Host "Icon assets valid: $($sizes -join ', ') px"
