[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ISCC,
    [string]$ProjectRoot = (Split-Path -Parent $PSScriptRoot),
    [string]$PayloadDirectory,
    [string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$manifest = Get-Content -LiteralPath (Join-Path $ProjectRoot "Cargo.toml") -Raw
$versionMatch = [regex]::Match(
    $manifest,
    '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"'
)
if (-not $versionMatch.Success) {
    throw "Cannot determine workspace version from Cargo.toml"
}
$version = $versionMatch.Groups[1].Value

if (-not $PayloadDirectory) {
    $PayloadDirectory = Join-Path $ProjectRoot "dist\WinSched-$version-windows-x64"
}
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $ProjectRoot "dist\gui-installer"
}

$payloadFiles = @(
    "winsched.exe",
    "winsched-service.exe",
    "winsched-tray.exe",
    "winsched-settings.exe",
    "winsched.toml",
    "secure-data.ps1",
    "README.md",
    "LICENSE"
)
$required = @(
    $ISCC,
    (Join-Path $ProjectRoot "installer\WinSched.iss"),
    (Join-Path $ProjectRoot "LICENSE"),
    (Join-Path $ProjectRoot "assets\tray\winsched.ico"),
    (Join-Path $ProjectRoot "assets\installer\winsched-wizard.png"),
    (Join-Path $ProjectRoot "assets\installer\winsched-wizard-dark.png"),
    (Join-Path $ProjectRoot "assets\installer\winsched-wizard-small.png"),
    (Join-Path $ProjectRoot "assets\installer\winsched-wizard-small-dark.png")
) + @(
    $payloadFiles | ForEach-Object { Join-Path $PayloadDirectory $_ }
) + @(
    (Join-Path $PayloadDirectory "SHA256SUMS")
)
foreach ($path in $required) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Required GUI installer input is missing: $path"
    }
}

$manifestEntries = @{}
Get-Content -LiteralPath (Join-Path $PayloadDirectory "SHA256SUMS") | ForEach-Object {
    if ($_ -notmatch '^(?<hash>[0-9a-f]{64})\s{2}(?<file>[A-Za-z0-9][A-Za-z0-9._-]*)$') {
        throw "Invalid SHA256SUMS line: $_"
    }
    $fileName = $Matches.file
    if ($payloadFiles -notcontains $fileName) {
        throw "Unexpected SHA256SUMS entry: $fileName"
    }
    if ($manifestEntries.ContainsKey($fileName)) {
        throw "Duplicate SHA256SUMS entry: $fileName"
    }
    $manifestEntries[$fileName] = $Matches.hash
    $path = Join-Path $PayloadDirectory $fileName
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "SHA256SUMS target is missing: $path"
    }
    $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $manifestEntries[$fileName]) {
        throw "Frozen payload hash mismatch: $fileName"
    }
}
foreach ($fileName in $payloadFiles) {
    if (-not $manifestEntries.ContainsKey($fileName)) {
        throw "SHA256SUMS is missing required payload: $fileName"
    }
}
if ($manifestEntries.Count -ne $payloadFiles.Count) {
    throw "SHA256SUMS entry count does not match the frozen payload"
}

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$script = Join-Path $ProjectRoot "installer\WinSched.iss"
& $ISCC `
    "/DAppVersion=$version" `
    "/DPayloadDir=$PayloadDirectory" `
    "/DProjectRoot=$ProjectRoot" `
    "/DOutputDir=$OutputDirectory" `
    $script
if ($LASTEXITCODE -ne 0) {
    throw "ISCC failed with exit code $LASTEXITCODE"
}

$setup = Join-Path $OutputDirectory "WinSched-$version-Setup-x64.exe"
if (-not (Test-Path -LiteralPath $setup -PathType Leaf)) {
    throw "ISCC did not produce the expected Setup executable"
}
$hash = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
$hashLine = "$hash  $([IO.Path]::GetFileName($setup))`n"
[IO.File]::WriteAllText(
    "$setup.sha256",
    $hashLine,
    [Text.UTF8Encoding]::new($false)
)

[pscustomobject]@{
    result = "PASS"
    version = $version
    setup = $setup
    sha256 = $hash
    bytes = (Get-Item -LiteralPath $setup).Length
    authenticode = (Get-AuthenticodeSignature -LiteralPath $setup).Status.ToString()
} | ConvertTo-Json -Depth 3
