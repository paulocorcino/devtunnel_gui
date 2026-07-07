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

# --- Win32 interop -----------------------------------------------------------
# The Slint UI window is a separate top-level window (class 'Window Class'); the
# process's MainWindowHandle points at a tiny 16x16 winit helper window, so we
# enumerate all top-level windows for the PID and pick the largest visible one.
if (-not ("WinCap" -as [type])) {
    Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public struct RECT { public int Left, Top, Right, Bottom; }
public static class WinCap {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    // True visible bounds, excluding the invisible DWM resize border / drop shadow
    // that GetWindowRect includes on Windows 10/11 (DWMWA_EXTENDED_FRAME_BOUNDS=9).
    public static RECT VisibleRect(IntPtr h) {
        RECT r;
        if (DwmGetWindowAttribute(h, 9, out r, Marshal.SizeOf(typeof(RECT))) == 0) return r;
        GetWindowRect(h, out r); return r;
    }
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("shcore.dll")] public static extern int SetProcessDpiAwareness(int v);
    public static List<IntPtr> ForPid(uint target) {
        var res = new List<IntPtr>();
        EnumWindows((h,l)=>{ uint p; GetWindowThreadProcessId(h, out p); if(p==target) res.Add(h); return true; }, IntPtr.Zero);
        return res;
    }
}
"@
    # Capture in physical pixels so the window isn't scaled/blurred on high DPI.
    try { [WinCap]::SetProcessDpiAwareness(2) | Out-Null } catch {}
}

# --- Find (or launch) the app window -----------------------------------------
function Find-AppWindow {
    $proc = Get-Process -Name "devtunnel_gui" -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $proc) { return $null }
    $best = [IntPtr]::Zero; $bestArea = 0; $bestRect = $null
    foreach ($h in [WinCap]::ForPid([uint32]$proc.Id)) {
        if (-not [WinCap]::IsWindowVisible($h)) { continue }
        $r = [WinCap]::VisibleRect($h)
        $w = $r.Right - $r.Left; $ht = $r.Bottom - $r.Top
        $area = $w * $ht
        if ($w -ge 300 -and $ht -ge 300 -and $area -gt $bestArea) {
            $best = $h; $bestArea = $area; $bestRect = $r
        }
    }
    if ($best -eq [IntPtr]::Zero) { return $null }
    return @{ Hwnd = $best; Rect = $bestRect }
}

if ($Launch) {
    if (-not (Test-Path $Exe)) { throw "Executable not found: $Exe (build it first)." }
    Start-Process $Exe | Out-Null
}

$win = $null
for ($i = 0; $i -lt 30 -and -not $win; $i++) {
    $win = Find-AppWindow
    if (-not $win) { Start-Sleep -Milliseconds 500 }
}
if (-not $win) {
    throw "No visible TunnelDeck window (>=300x300) found. Open the window from the tray icon, then re-run this script."
}

$hwnd = $win.Hwnd
# Raise the window above everything else so the screen grab isn't of whatever is
# covering it. SetForegroundWindow alone is unreliable from a background process
# (foreground lock), so pin it TOPMOST, capture, then release.
$HWND_TOPMOST = [IntPtr](-1); $HWND_NOTOPMOST = [IntPtr](-2)
$SWP = 0x0001 -bor 0x0002 -bor 0x0040  # NOSIZE | NOMOVE | SHOWWINDOW
[WinCap]::ShowWindow($hwnd, 9) | Out-Null   # SW_RESTORE
[WinCap]::SetWindowPos($hwnd, $HWND_TOPMOST, 0, 0, 0, 0, $SWP) | Out-Null
[WinCap]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 700

$rect = [WinCap]::VisibleRect($hwnd)
$w = $rect.Right - $rect.Left
$h = $rect.Bottom - $rect.Top

# --- Capture the window ------------------------------------------------------
$shot = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($shot)
$g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size $w, $h))
$g.Dispose()

# Release the topmost pin now that the grab is done.
[WinCap]::SetWindowPos($hwnd, $HWND_NOTOPMOST, 0, 0, 0, 0, $SWP) | Out-Null

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
