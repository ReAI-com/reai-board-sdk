# Builds the ReAI Vibe Board Windows virtual-microphone driver package.
#
# Usage (from the repository root, PowerShell 5+ or pwsh):
#   pwsh -File scripts/build-driver-windows.ps1 -Platform x64 -Configuration Release
#   pwsh -File scripts/build-driver-windows.ps1 -Platform x64 -TestSign   # + self-signed test cert
#
# Requirements:
#   - Visual Studio 2022 with the C++ build tools and the Windows Driver Kit
#     (WDK) installed ("WindowsKernelModeDriver10.0" platform toolset).
#   - For -TestSign: Windows SDK signtool.exe (ships with the WDK/SDK).
#
# Outputs land in <repo>/virtual-mic/driver-windows/out/<Configuration><Platform>/:
#   ReAIVibeBoardVirtualMic.sys / .inf / .cat
# With -TestSign, the .sys and .cat are signed by a freshly created local
# code-signing certificate (exported as out/testsign.cer). Loading the driver
# on a machine then requires:  bcdedit /set TESTSIGNING ON  (+ reboot) and
# importing that .cer into Trusted Root + TrustedPublisher.
param(
    [ValidateSet("x64", "ARM64")]
    [string]$Platform = "x64",
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Release",
    [switch]$TestSign
)

$ErrorActionPreference = "Stop"

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$driverDir = Join-Path $repoRoot "virtual-mic\driver-windows"
$solution = Join-Path $driverDir "ReAIVibeBoardVirtualMic.sln"

if (-not (Test-Path $solution)) {
    throw "driver solution not found: $solution"
}

# --- locate MSBuild via vswhere ---------------------------------------------
$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) {
    throw "vswhere.exe not found; install Visual Studio 2022 (Build Tools suffice)."
}

$msbuild = & $vswhere -latest -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -find MSBuild\**\Bin\MSBuild.exe | Select-Object -First 1
if (-not $msbuild) {
    throw "MSBuild.exe not found; install the VS C++ workload."
}

# --- locate the WDK (needed by the driver toolset) --------------------------
$wdkRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10"
if (-not (Test-Path (Join-Path $wdkRoot "Include"))) {
    throw "Windows Driver Kit not found under $wdkRoot; install the WDK that matches Visual Studio."
}

Write-Host "== msbuild $Configuration|$Platform =="
& $msbuild $solution "/m" "/p:Configuration=$Configuration" "/p:Platform=$Platform"
if ($LASTEXITCODE -ne 0) {
    throw "msbuild failed with exit code $LASTEXITCODE"
}

# --- collect the package ----------------------------------------------------
$outDir = Join-Path $driverDir "out\$Configuration$Platform"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$packageDir = Get-ChildItem -Path $driverDir -Recurse -Filter "ReAIVibeBoardVirtualMic.inf" |
    Where-Object { $_.FullName -match [regex]::Escape("$Configuration") -and $_.FullName -match [regex]::Escape($Platform) } |
    Select-Object -First 1
if (-not $packageDir) {
    throw "built INF not found; check msbuild output paths"
}
$packageRoot = Split-Path $packageDir.FullName

foreach ($name in @("ReAIVibeBoardVirtualMic.sys", "ReAIVibeBoardVirtualMic.inf", "ReAIVibeBoardVirtualMic.cat")) {
    $file = Get-ChildItem -Path $packageRoot -Recurse -Filter $name | Select-Object -First 1
    if ($file) {
        Copy-Item $file.FullName $outDir -Force
    } else {
        Write-Warning "missing package file: $name"
    }
}

# --- optional test signing ---------------------------------------------------
if ($TestSign) {
    $signtool = Get-ChildItem -Path (Join-Path $wdkRoot "bin") -Recurse -Filter "signtool.exe" |
        Where-Object { $_.FullName -match "x64" } | Select-Object -First 1
    if (-not $signtool) { throw "signtool.exe not found under $wdkRoot\bin" }

    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=ReAI Vibe Board Virtual Mic (Test)" `
        -KeyUsage DigitalSignature -FriendlyName "ReAI Vibe Board Virtual Mic (Test)" `
        -CertStoreLocation "Cert:\CurrentUser\My"
    $pfx = Join-Path $outDir "testsign.pfx"
    $password = ConvertTo-SecureString -String "reai-vbm" -Force -AsPlainText
    Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $password | Out-Null
    Export-Certificate -Cert $cert -FilePath (Join-Path $outDir "testsign.cer") | Out-Null

    foreach ($ext in @(".cat", ".sys")) {
        $file = Get-ChildItem -Path $outDir -Filter "ReAIVibeBoardVirtualMic$ext"
        & $signtool.FullName sign /f $pfx /p "reai-vbm" /fd SHA256 $file.FullName
        if ($LASTEXITCODE -ne 0) { throw "signtool failed for $($file.Name)" }
    }
    Remove-Item $pfx -Force
    Write-Host "== test-signed; install testsign.cer into Trusted Root + TrustedPublisher,"
    Write-Host "   run 'bcdedit /set TESTSIGNING ON' and reboot, then: pnputil /add-driver ReAIVibeBoardVirtualMic.inf /install"
}

Write-Host "== done: $outDir =="
