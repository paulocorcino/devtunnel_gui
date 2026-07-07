<#
.SYNOPSIS
  Builds the Microsoft Store (MSIX) package for TunnelDeck for Dev Tunnels.

.DESCRIPTION
  1. Compiles the release executable with the `store` cargo feature (self-install,
     update-checker and HKCU auto-start compiled out — the package owns those).
  2. Renders the visual assets from the procedural app icon (gen_msix_assets).
  3. Assembles a package layout, substituting the Partner Center identity values
     into AppxManifest.xml.
  4. Packs it into an .msix with makeappx.exe from the Windows SDK.
  5. Optionally signs it with a local test certificate for sideload testing, and/or
     runs the Windows App Certification Kit (WACK).

  The .msix you upload to Partner Center must be UNSIGNED (the Store re-signs it) —
  so only pass -Sign when you want to install/test locally, and produce a separate
  unsigned package for submission.

.PARAMETER IdentityName
  Package/Identity/Name from Partner Center (Product identity page).

.PARAMETER PublisherId
  Package/Identity/Publisher from Partner Center, e.g. "CN=1234ABCD-...".

.PARAMETER PublisherDisplayName
  The publisher display name from Partner Center.

.PARAMETER Version
  4-part version a.b.c.0 (the 4th part must be 0 for the Store). Default 0.1.0.0.

.PARAMETER Sign
  Sign the package with -CertPath for local sideload testing. Do NOT submit a signed
  package to the Store.

.PARAMETER Wack
  Run the Windows App Certification Kit against the built package after packing.

.EXAMPLE
  # Submission package (unsigned):
  .\build-msix.ps1 -IdentityName 12345Publisher.TunnelDeck `
                   -PublisherId "CN=ABCDEF01-2345-6789-ABCD-EF0123456789" `
                   -PublisherDisplayName "Paulo Corcino" -Version 0.1.0.0

.EXAMPLE
  # Local test package, self-signed and validated:
  .\build-msix.ps1 -IdentityName 12345Publisher.TunnelDeck `
                   -PublisherId "CN=Paulo Corcino" -PublisherDisplayName "Paulo Corcino" `
                   -Sign -CertPath .\TunnelDeck-test.pfx -Wack
#>
[CmdletBinding()]
param(
    [string] $IdentityName,
    [string] $PublisherId,
    [string] $PublisherDisplayName,
    [string] $Version,
    [switch] $Sign,
    [string] $CertPath,
    [string] $CertPassword,
    [switch] $Wack,
    # .env file with the Partner Center identity values. Any parameter you pass
    # explicitly wins over the file.
    [string] $EnvFile
)

$ErrorActionPreference = "Stop"
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot  = Resolve-Path (Join-Path $scriptDir "..\..")
$layout    = Join-Path $scriptDir "layout"
$outDir    = Join-Path $scriptDir "out"

# --- Load identity from .env (parameters passed explicitly take precedence) ----
# Fill in packaging\msix\.env (copy from .env.example) so you can just run
# `.\build-msix.ps1` with no arguments.
if (-not $EnvFile) { $EnvFile = Join-Path $scriptDir ".env" }
$envMap = @{}
if (Test-Path $EnvFile) {
    Write-Host "Loading identity from $EnvFile"
    foreach ($line in Get-Content $EnvFile) {
        $t = $line.Trim()
        if ($t -eq "" -or $t.StartsWith("#")) { continue }
        $kv = $t -split '=', 2
        if ($kv.Count -eq 2) { $envMap[$kv[0].Trim()] = $kv[1].Trim().Trim('"') }
    }
}
if (-not $IdentityName)         { $IdentityName         = $envMap["IDENTITY_NAME"] }
if (-not $PublisherId)          { $PublisherId          = $envMap["PUBLISHER_ID"] }
if (-not $PublisherDisplayName) { $PublisherDisplayName = $envMap["PUBLISHER_DISPLAY_NAME"] }
if (-not $Version)              { $Version              = $envMap["VERSION"] }
if (-not $CertPath)             { $CertPath             = $envMap["CERT_PATH"] }
if (-not $CertPassword)         { $CertPassword         = $envMap["CERT_PASSWORD"] }
if (-not $Version)              { $Version              = "0.1.0.0" }

# A relative CERT_PATH is resolved against the repo root, so the .env value works
# no matter which directory you run the script from.
if ($CertPath -and -not [System.IO.Path]::IsPathRooted($CertPath)) {
    $CertPath = Join-Path $repoRoot $CertPath
}

$missing = @()
if (-not $IdentityName)         { $missing += "IdentityName / IDENTITY_NAME" }
if (-not $PublisherId)          { $missing += "PublisherId / PUBLISHER_ID" }
if (-not $PublisherDisplayName) { $missing += "PublisherDisplayName / PUBLISHER_DISPLAY_NAME" }
if ($missing.Count -gt 0) {
    throw "Missing identity value(s): $($missing -join ', '). Set them in $EnvFile (copy .env.example) or pass as parameters. Get them from Partner Center > Product identity."
}

$msixPath = Join-Path $outDir "TunnelDeck-$Version.msix"

if ($Version -notmatch '^\d+\.\d+\.\d+\.0$') {
    throw "Version must be a.b.c.0 (the 4th part must be 0 for the Store); got '$Version'."
}

# --- Locate the latest Windows SDK bin (makeappx, signtool, appcert) ----------
function Find-SdkTool([string]$name) {
    $roots = @("${env:ProgramFiles(x86)}\Windows Kits\10\bin", "${env:ProgramFiles}\Windows Kits\10\bin")
    $found = foreach ($root in $roots) {
        if (Test-Path $root) {
            Get-ChildItem -Path $root -Recurse -Filter $name -ErrorAction SilentlyContinue |
                Where-Object { $_.FullName -match '\\x64\\' }
        }
    }
    $tool = $found | Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $tool) { throw "$name not found. Install the Windows 10/11 SDK." }
    return $tool.FullName
}

$makeappx = Find-SdkTool "makeappx.exe"
Write-Host "makeappx: $makeappx"

# --- 1. Build the store executable -------------------------------------------
# The `store` feature pulls in `hosting` (Host button) — needs NASM + Strawberry
# Perl on PATH for the vendored-OpenSSL build (see docs/store/README.md).
Write-Host "`n[1/4] Building release executable (--features store)..."
Push-Location $repoRoot
try {
    & cargo build --release --features store --bin devtunnel_gui
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed." }

    # --- 2. Render visual assets ---------------------------------------------
    Write-Host "`n[2/4] Rendering MSIX assets..."
    & cargo run --release --features store --bin gen_msix_assets -- (Join-Path $scriptDir "Assets")
    if ($LASTEXITCODE -ne 0) { throw "asset generation failed." }
}
finally { Pop-Location }

$exe = Join-Path $repoRoot "target\release\devtunnel_gui.exe"
if (-not (Test-Path $exe)) { throw "Built executable not found at $exe" }

# --- 3. Assemble the package layout ------------------------------------------
Write-Host "`n[3/4] Assembling package layout..."
if (Test-Path $layout) { Remove-Item $layout -Recurse -Force }
New-Item -ItemType Directory -Path $layout | Out-Null
New-Item -ItemType Directory -Path $outDir -Force | Out-Null

Copy-Item $exe (Join-Path $layout "devtunnel_gui.exe")
Copy-Item (Join-Path $scriptDir "Assets") (Join-Path $layout "Assets") -Recurse

# Substitute the Partner Center identity values into the manifest.
$manifest = Get-Content (Join-Path $scriptDir "AppxManifest.xml") -Raw
$manifest = $manifest.Replace("__IDENTITY_NAME__", $IdentityName)
$manifest = $manifest.Replace("__PUBLISHER_ID__", $PublisherId)
$manifest = $manifest.Replace("__PUBLISHER_DISPLAY_NAME__", $PublisherDisplayName)
# Case-sensitive so it targets Identity's Version="..." and NOT the lowercase
# version="1.0" in the <?xml ... ?> declaration.
$manifest = $manifest -creplace 'Version="[\d.]+"', "Version=`"$Version`""
Set-Content -Path (Join-Path $layout "AppxManifest.xml") -Value $manifest -Encoding UTF8

# --- 4. Pack -----------------------------------------------------------------
Write-Host "`n[4/4] Packing $msixPath ..."
if (Test-Path $msixPath) { Remove-Item $msixPath -Force }
& $makeappx pack /d $layout /p $msixPath /o
if ($LASTEXITCODE -ne 0) { throw "makeappx pack failed." }
Write-Host "Package built: $msixPath"

# --- Optional: sign for local sideload testing -------------------------------
if ($Sign) {
    if (-not $CertPath) { throw "-Sign requires -CertPath <pfx>." }
    $signtool = Find-SdkTool "signtool.exe"
    Write-Host "`nSigning (local test only — do NOT submit a signed package)..."
    $args = @("sign", "/fd", "SHA256", "/a", "/f", $CertPath)
    if ($CertPassword) { $args += @("/p", $CertPassword) }
    $args += $msixPath
    & $signtool @args
    if ($LASTEXITCODE -ne 0) { throw "signtool failed. The cert's subject must equal Identity/@Publisher ($PublisherId)." }
    Write-Host "Signed. Install locally with:  Add-AppxPackage '$msixPath'"
}

# --- Optional: Windows App Certification Kit ---------------------------------
if ($Wack) {
    $appcert = Find-SdkTool "appcert.exe"
    $report = Join-Path $outDir "wack-report.xml"
    Write-Host "`nRunning Windows App Certification Kit (may take several minutes)..."
    & $appcert reset
    & $appcert test -appxpackagepath $msixPath -reportoutputpath $report
    Write-Host "WACK report: $report"
}

Write-Host "`nDone."
if (-not $Sign) {
    Write-Host "Upload $msixPath to Partner Center (it must stay UNSIGNED for submission)."
}
