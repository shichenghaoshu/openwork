[CmdletBinding()]
param(
    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string]$Version,

    [Parameter()]
    [ValidateNotNullOrEmpty()]
    [string]$InstallDir = $(
        if ($env:OPENWORK_INSTALL_DIR) { $env:OPENWORK_INSTALL_DIR }
        else { Join-Path $env:LOCALAPPDATA "Programs\OpenWork\bin" }
    ),

    [Parameter()]
    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$repository = "shichenghaoshu/openwork"

if (-not [Environment]::Is64BitOperatingSystem -or
    [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne
        [System.Runtime.InteropServices.Architecture]::X64) {
    throw "This installer currently supports Windows x64 only."
}

$headers = @{
    Accept = "application/vnd.github+json"
    "X-GitHub-Api-Version" = "2022-11-28"
    "User-Agent" = "OpenWork-Installer"
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repository/releases/latest" -Headers $headers
    $Version = [string]$release.tag_name
}

if ($Version -notmatch '^v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?(?:\+[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$') {
    throw "Invalid release version: $Version"
}

$target = "x86_64-pc-windows-msvc"
$asset = "openwork-$Version-$target.zip"
$baseUrl = "https://github.com/$repository/releases/download/$Version"
$temporaryDir = Join-Path ([IO.Path]::GetTempPath()) ("openwork-install-" + [Guid]::NewGuid())
$stage = $null
$backup = $null
$destination = $null

try {
    New-Item -ItemType Directory -Path $temporaryDir | Out-Null
    $archivePath = Join-Path $temporaryDir $asset
    $checksumPath = "$archivePath.sha256"
    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$asset" -OutFile $archivePath -Headers $headers
    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$asset.sha256" -OutFile $checksumPath -Headers $headers

    $expectedHash = ((Get-Content -LiteralPath $checksumPath -Raw) -split '\s+')[0]
    if ($expectedHash -notmatch '^[0-9A-Fa-f]{64}$') {
        throw "Release checksum is not a valid SHA-256 value."
    }
    $actualHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash
    if (-not $actualHash.Equals($expectedHash, [StringComparison]::OrdinalIgnoreCase)) {
        throw "SHA-256 verification failed."
    }

    Expand-Archive -LiteralPath $archivePath -DestinationPath $temporaryDir
    $extracted = Join-Path $temporaryDir "openwork-$Version-$target\openwork.exe"
    if (-not (Test-Path -LiteralPath $extracted -PathType Leaf)) {
        throw "Release archive does not contain openwork.exe."
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $destination = Join-Path $InstallDir "openwork.exe"
    if ((Test-Path -LiteralPath $destination) -and -not $Force) {
        throw "$destination already exists. Rerun with -Force to preserve a backup and replace it."
    }

    $stage = Join-Path $InstallDir (".openwork.install." + [Guid]::NewGuid() + ".exe")
    Copy-Item -LiteralPath $extracted -Destination $stage
    if (Test-Path -LiteralPath $destination) {
        $timestamp = [DateTime]::UtcNow.ToString("yyyyMMddTHHmmssZ")
        $backup = "$destination.backup.$timestamp.$([Guid]::NewGuid())"
        Move-Item -LiteralPath $destination -Destination $backup
        Write-Host "Preserved previous binary at $backup"
    }
    try {
        Move-Item -LiteralPath $stage -Destination $destination
    }
    catch {
        if ($backup -and (Test-Path -LiteralPath $backup) -and
            -not (Test-Path -LiteralPath $destination)) {
            Move-Item -LiteralPath $backup -Destination $destination
        }
        throw
    }
    $stage = $null
    Write-Host "Installed OpenWork $Version to $destination"
}
finally {
    if ($stage -and (Test-Path -LiteralPath $stage)) {
        Remove-Item -LiteralPath $stage -Force
    }
    if (Test-Path -LiteralPath $temporaryDir) {
        Remove-Item -LiteralPath $temporaryDir -Recurse -Force
    }
}
