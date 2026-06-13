<#
.SYNOPSIS
    Build the DBAN live ISO on Windows (native PowerShell + Docker Desktop).

.DESCRIPTION
    A native Windows equivalent of iso/build.sh. Builds the two-stage Docker
    image (static musl binary -> hybrid ISO) and runs it to produce
    dist\dban.iso. Unlike the Bash script under Git Bash, this avoids the MSYS
    path-rewriting traps entirely because PowerShell does not mangle the
    container-side /out mount target.

.PARAMETER Arch
    Target architecture: x86_64 (default) or arm64. arm64 produces a
    UEFI-only image and requires Docker with binfmt/qemu emulation.

.PARAMETER Label
    ISO volume label. Defaults to DBAN_<yyyyMMdd>. The boot-medium detector
    looks for a label beginning "DBAN", so keep that prefix.

.EXAMPLE
    .\iso\build.ps1
    Builds dist\dban.iso for x86_64.

.EXAMPLE
    .\iso\build.ps1 -Arch arm64
    Builds an ARM64 (aarch64) UEFI ISO.
#>
[CmdletBinding()]
param(
    [ValidateSet('x86_64', 'arm64')]
    [string]$Arch = 'x86_64',
    [string]$Label
)

$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$root = Split-Path $here -Parent
$image = "dban-iso-builder-$Arch"
if (-not $Label) { $Label = "DBAN_$(Get-Date -Format 'yyyyMMdd')" }

# Map the friendly arch name to the Rust target triple and Docker platform.
$target, $platform = switch ($Arch) {
    'x86_64' { 'x86_64-unknown-linux-musl', 'linux/amd64' }
    'arm64' { 'aarch64-unknown-linux-musl', 'linux/arm64' }
}

Write-Host ">> Verifying Docker is available..."
& docker version --format '{{.Server.Version}}' | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "Docker does not appear to be running. Start Docker Desktop and retry."
}

Write-Host ">> Building ISO builder image ($Arch)..."
Push-Location $root
try {
    & docker build `
        --platform $platform `
        --build-arg "DBAN_ARCH=$Arch" `
        --build-arg "RUST_TARGET=$target" `
        -t $image -f iso/Dockerfile .
    if ($LASTEXITCODE -ne 0) { throw "docker build failed" }
}
finally {
    Pop-Location
}

Write-Host ">> Producing ISO inside container..."
$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force -Path $dist | Out-Null
& docker run --rm `
    --platform $platform `
    -e "DBAN_LABEL=$Label" `
    -e "DBAN_ARCH=$Arch" `
    -v "${dist}:/out" `
    $image
if ($LASTEXITCODE -ne 0) { throw "ISO assembly failed" }

$iso = Join-Path $dist 'dban.iso'
Write-Host ">> Done. Image at dist\dban.iso"
Get-Item $iso | Select-Object Name, @{n = 'SizeMB'; e = { [math]::Round($_.Length / 1MB, 1) } }, LastWriteTime
