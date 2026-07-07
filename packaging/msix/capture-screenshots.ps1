<#
.SYNOPSIS
  Captures the app window for Microsoft Store screenshots.

.DESCRIPTION
  Grabs the TunnelDeck window and composes it, centred, on a 16:9 canvas with a
  soft indigo backdrop - the format the Store expects (min 1366x768; this defaults
  to 1920x1080). Saves both the raw window PNG and the composed canvas PNG to
  docs/store/screenshots/.

  Run this from your signed-in session, once per state you want to show (main
  list, creating a tunnel, settings, about), passing a distinct -Name each time:

    .\capture-screenshots.ps1 -Name 01-main
    .\capture-screenshots.ps1 -Name 02-settings

  Use -Launch to start the app first (otherwise it captures the already-running
  instance). The window must be visible and not minimized to the tray.

.PARAMETER Name
  Base filename (no extension) for this capture.

.PARAMETER Launch
  Start the executable and wait for its window before capturing.

.PARAMETER Exe
  Path to the executable. Defaults to target\release\devtunnel_gui.exe.

.PARAMETER CanvasWidth / CanvasHeight
  Composed canvas size. Default 1920x1080 (16:9). Store minimum is 1366x768.
#>
[CmdletBinding()]
param(
    [string] $Name = "01-main",
    [switch] $Launch,
    [string] $Exe,
    [int] $CanvasWidth = 1920,
    [int] $CanvasHeight = 1080
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot  = Resolve-Path (Join-Path $scriptDir "..\..")
if (-not $Exe) { $Exe = Join-Path $repoRoot "target\release\devtunnel_gui.exe" }
$outDir = Join-Path $repoRoot "docs\store\screenshots"
New-Item -ItemType Directory -Path $outDir -Force | Out-Null

# --- Win32 interop: GetWindowRect + SetForegroundWindow -----------------------
if (-not ("Win32Native" -as [type])) {
    Add-Type @"
using System;
using System.Runtime.InteropServices;
public struct RECT { public int Left, Top, Right, Bottom; }
public static class Win32Native {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int n);
    [DllImport("shcore.dll")] public static extern int SetProcessDpiAwareness(int v);
}
"@
    # Capture in physical pixels so the window isn't scaled/blurred on high DPI.
    try { [Win32Native]::SetProcessDpiAwareness(2) | Out-Null } catch {}
}

# --- Find (or launch) the app window -----------------------------------------
function Get-AppProcess {
    Get-Process -Name "devtunnel_gui" -ErrorAction SilentlyContinue |
        Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
}

if ($Launch) {
    if (-not (Test-Path $Exe)) { throw "Executable not found: $Exe (build it first)." }
    Start-Process $Exe | Out-Null
}

$proc = $null
for ($i = 0; $i -lt 30 -and -not $proc; $i++) {
    $proc = Get-AppProcess
    if (-not $proc) { Start-Sleep -Milliseconds 500 }
}
if (-not $proc) {
    throw "No visible TunnelDeck window found. Start the app (or pass -Launch) and make sure it isn't minimized to the tray."
}

$hwnd = $proc.MainWindowHandle
[Win32Native]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 400

$rect = New-Object RECT
[Win32Native]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
$w = $rect.Right - $rect.Left
$h = $rect.Bottom - $rect.Top
# TunnelDeck starts minimized to the tray, so its only window may be a tiny
# hidden one. Refuse anything too small to be the real UI and tell the user to
# open the window first (click the tray icon).
if ($w -lt 300 -or $h -lt 300) {
    throw "Only a ${w}x${h} window was found - TunnelDeck is minimized to the tray. " +
          "Click the tray icon to open the main window (sign in for real content), then re-run this script."
}

# --- Capture the window ------------------------------------------------------
$shot = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($shot)
$g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size $w, $h))
$g.Dispose()

$rawPath = Join-Path $outDir "$Name-window.png"
$shot.Save($rawPath, [System.Drawing.Imaging.ImageFormat]::Png)

# --- Compose onto a 16:9 canvas with an indigo gradient ----------------------
$canvas = New-Object System.Drawing.Bitmap $CanvasWidth, $CanvasHeight
$cg = [System.Drawing.Graphics]::FromImage($canvas)
$cg.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$cg.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic

$rectF = New-Object System.Drawing.Rectangle 0, 0, $CanvasWidth, $CanvasHeight
$c1 = [System.Drawing.Color]::FromArgb(255, 108, 111, 245)  # #6c6ff5 (icon top)
$c2 = [System.Drawing.Color]::FromArgb(255, 79, 70, 229)    # #4f46e5 (icon bottom)
$brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rectF, $c1, $c2, 90.0
$cg.FillRectangle($brush, $rectF)

# Scale the window down if it exceeds ~78% of the canvas, keeping aspect.
$maxW = [int]($CanvasWidth * 0.78); $maxH = [int]($CanvasHeight * 0.82)
$scale = [Math]::Min([Math]::Min($maxW / $w, $maxH / $h), 1.0)
$dw = [int]($w * $scale); $dh = [int]($h * $scale)
$dx = [int](($CanvasWidth - $dw) / 2); $dy = [int](($CanvasHeight - $dh) / 2)

# Soft shadow behind the window.
$shadow = New-Object System.Drawing.Drawing2D.GraphicsPath
$sr = New-Object System.Drawing.Rectangle ($dx + 8), ($dy + 14), $dw, $dh
$cg.FillRectangle((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(60, 0, 0, 0))), $sr)
$cg.DrawImage($shot, $dx, $dy, $dw, $dh)
$cg.Dispose()

$outPath = Join-Path $outDir "$Name.png"
$canvas.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
$shot.Dispose(); $canvas.Dispose()

Write-Host "Captured window: $rawPath  (${w}x${h})"
Write-Host "Store screenshot: $outPath  (${CanvasWidth}x${CanvasHeight})"
