# Capture a screenshot of the running PDFXI window for visual review.
#
# Usage:
#   powershell -NoProfile -File scripts/capture-ui.ps1 -Out docs/evidence/ui/light.png
#   powershell -NoProfile -File scripts/capture-ui.ps1 -Out dark.png -Arguments '--dark'
#
# The script starts the built viewer, waits for its largest visible top-level
# window, captures that window to a PNG, then closes it. It never installs or
# elevates anything.

param(
    [Parameter(Mandatory = $true)][string]$Out,
    [string]$Exe = '',
    [string]$Arguments = '',
    [int]$WaitMs = 4000
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

if (-not $Exe) {
    foreach ($candidate in @(
        (Join-Path $root 'target\integration\release\pdfxi-ui.exe'),
        (Join-Path $root 'target\release\pdfxi-ui.exe'),
        (Join-Path $root 'target\debug\pdfxi-ui.exe')
    )) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { $Exe = $candidate; break }
    }
}
if (-not $Exe -or -not (Test-Path -LiteralPath $Exe -PathType Leaf)) {
    throw "pdfxi-ui.exe not found; build it with: cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium"
}

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
public static class Win32Capture {
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc proc, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int cmd);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);

    private static List<IntPtr> collect = new List<IntPtr>();
    private static bool Collect(IntPtr hWnd, IntPtr lParam) {
        uint pid;
        GetWindowThreadProcessId(hWnd, out pid);
        if (pid == (uint)lParam.ToInt64() && IsWindowVisible(hWnd)) {
            collect.Add(hWnd);
        }
        return true;
    }

    public static IntPtr LargestWindow(uint targetPid, out int width, out int height) {
        collect = new List<IntPtr>();
        EnumWindows(Collect, new IntPtr((long)targetPid));
        IntPtr best = IntPtr.Zero;
        int bestArea = 0;
        width = 0;
        height = 0;
        foreach (IntPtr hWnd in collect) {
            RECT r;
            if (!GetWindowRect(hWnd, out r)) { continue; }
            int w = r.Right - r.Left;
            int h = r.Bottom - r.Top;
            if (w > 0 && h > 0 && w * h > bestArea) {
                bestArea = w * h;
                best = hWnd;
                width = w;
                height = h;
            }
        }
        return best;
    }
}
"@

$startArgs = @()
if ($Arguments) { $startArgs = $Arguments.Split(' ', [System.StringSplitOptions]::RemoveEmptyEntries) }
[void][Win32Capture]::SetProcessDpiAwarenessContext([IntPtr]-4)
if ($startArgs.Count -gt 0) {
    $process = Start-Process -FilePath $Exe -ArgumentList $startArgs -PassThru
} else {
    $process = Start-Process -FilePath $Exe -PassThru
}
try {
    $deadline = (Get-Date).AddMilliseconds($WaitMs + 12000)
    $handle = [IntPtr]::Zero
    $width = 0
    $height = 0
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 200
        $process.Refresh()
        if ($process.HasExited) { throw "process exited with code $($process.ExitCode)" }
        $width = 0
        $height = 0
        $candidate = [Win32Capture]::LargestWindow([uint32]$process.Id, [ref]$width, [ref]$height)
        if ($candidate -ne [IntPtr]::Zero -and $width -ge 800 -and $height -ge 600) {
            $handle = $candidate
            break
        }
    }
    if ($handle -eq [IntPtr]::Zero) {
        throw "no visible window of at least 800x600 appeared (last ${width}x${height})"
    }
    [void][Win32Capture]::ShowWindow($handle, 9)  # SW_RESTORE
    [void][Win32Capture]::SetForegroundWindow($handle)
    Start-Sleep -Milliseconds $WaitMs
    $process.Refresh()

    $rect = New-Object RECT
    if (-not [Win32Capture]::GetWindowRect($handle, [ref]$rect)) {
        throw "GetWindowRect failed"
    }
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    if ($width -lt 400 -or $height -lt 300) { throw "window has no usable area ($width x $height)" }

    $outPath = [System.IO.Path]::GetFullPath($Out)
    $dir = Split-Path -Parent $outPath
    if ($dir -and -not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }

    $bitmap = New-Object System.Drawing.Bitmap $width, $height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $hdc = $graphics.GetHdc()
    $printed = $false
    try {
        $printed = [Win32Capture]::PrintWindow($handle, $hdc, 2)  # PW_RENDERFULLCONTENT
    }
    finally {
        $graphics.ReleaseHdc($hdc)
    }
    if ($printed) {
        $bitmap.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
        Write-Output "CAPTURED (printwindow) $outPath ($width x $height)"
    } else {
        $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
        $bitmap.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
        Write-Output "CAPTURED (screen) $outPath ($width x $height)"
    }
    $graphics.Dispose()
    $bitmap.Dispose()
}
finally {
    if (-not $process.HasExited) {
        $null = $process.CloseMainWindow()
        Start-Sleep -Milliseconds 600
        if (-not $process.HasExited) { $process.Kill() }
    }
}
