[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $ProjectDir,
    [Parameter(Mandatory = $true)] [string] $ExePath,
    [Parameter(Mandatory = $true)] [string] $Version,
    [Parameter(Mandatory = $true)] [string] $OutputDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$project = [System.IO.Path]::GetFullPath($ProjectDir)
$executable = [System.IO.Path]::GetFullPath($ExePath)
$output = [System.IO.Path]::GetFullPath($OutputDir)
$temporaryRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }
$packageName = "nemuri-$Version-windows-x86_64"
$stageRoot = Join-Path $temporaryRoot $packageName
$licenseDir = Join-Path $project "packaging\licenses"
$notices = Join-Path $licenseDir "THIRD-PARTY-NOTICES.txt"
$mplLicense = Join-Path $licenseDir "MPL-2.0.txt"
$apacheLicense = Join-Path $licenseDir "Apache-2.0.txt"

foreach ($required in @(
    $executable,
    (Join-Path $project "assets\icons\nemuri.ico"),
    (Join-Path $project "packaging\windows\ChineseSimplified.isl"),
    $notices,
    $mplLicense,
    $apacheLicense
)) {
    if (-not (Test-Path -LiteralPath $required)) {
        throw "Required packaging input was not found: $required"
    }
}

if (Test-Path -LiteralPath $stageRoot) {
    Remove-Item -LiteralPath $stageRoot -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $stageRoot | Out-Null

Copy-Item -LiteralPath $executable -Destination (Join-Path $stageRoot "nemuri.exe") -Force
Copy-Item -LiteralPath (Join-Path $project "packaging\windows\nemuri.cmd") -Destination $stageRoot
Copy-Item -LiteralPath (Join-Path $project "packaging\windows\README-Windows.txt") -Destination $stageRoot
Copy-Item -LiteralPath $notices -Destination $stageRoot
Copy-Item -LiteralPath (Join-Path $project "assets\icons\nemuri.ico") -Destination $stageRoot
Copy-Item -LiteralPath (Join-Path $project "LICENSE") -Destination $stageRoot

$stageLicenses = Join-Path $stageRoot "third-party-licenses"
New-Item -ItemType Directory -Force -Path $stageLicenses | Out-Null
Copy-Item -LiteralPath $mplLicense -Destination $stageLicenses
Copy-Item -LiteralPath $apacheLicense -Destination $stageLicenses

foreach ($required in @(
    (Join-Path $stageRoot "nemuri.exe"),
    (Join-Path $stageRoot "LICENSE"),
    (Join-Path $stageRoot "THIRD-PARTY-NOTICES.txt"),
    (Join-Path $stageLicenses "MPL-2.0.txt"),
    (Join-Path $stageLicenses "Apache-2.0.txt")
)) {
    if (-not (Test-Path -LiteralPath $required)) {
        throw "Staged package is missing required file: $required"
    }
}

$oldPath = $env:PATH
try {
    $env:PATH = "$stageRoot;$oldPath"
    & (Join-Path $stageRoot "nemuri.cmd") --version
    if ($LASTEXITCODE -ne 0) {
        throw "Packaged nemuri launcher failed with exit code $LASTEXITCODE"
    }
}
finally {
    $env:PATH = $oldPath
}

New-Item -ItemType Directory -Force -Path $output | Out-Null
$portableZip = Join-Path $output "$packageName-portable.zip"
if (Test-Path -LiteralPath $portableZip) {
    Remove-Item -LiteralPath $portableZip -Force
}
Compress-Archive -LiteralPath $stageRoot -DestinationPath $portableZip -CompressionLevel Optimal

$isccCommand = Get-Command "ISCC.exe" -ErrorAction SilentlyContinue
$iscc = if ($isccCommand) { $isccCommand.Source } else { $null }
if (-not $iscc) {
    $fallback = "C:\Program Files (x86)\Inno Setup 6\ISCC.exe"
    if (Test-Path -LiteralPath $fallback) {
        $iscc = $fallback
    } else {
        throw "ISCC.exe was not found"
    }
}

$installerScript = Join-Path $project "packaging\windows\nemuri.iss"
& $iscc "/DAppVersion=$Version" "/DStageDir=$stageRoot" "/DOutputDir=$output" $installerScript
if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup compiler exited with code $LASTEXITCODE"
}

$setup = Join-Path $output "$packageName-setup.exe"
if (-not (Test-Path -LiteralPath $setup)) {
    throw "Expected installer was not created: $setup"
}

Write-Host "Windows artifacts:"
Write-Host $portableZip
Write-Host $setup
