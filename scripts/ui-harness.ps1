# UI Automation harness for the PDFXI viewer (Windows).
#
# Dot-source it from a script in this repository:
#   . (Join-Path $PSScriptRoot 'ui-harness.ps1')
# It drives a PDFXI viewer through UI Automation and posted window messages,
# and captures the window with PrintWindow. It never edits the repository;
# screenshots and redirected stdout/stderr go to $script:RvDir.
#
# Callers set $script:RvUi to the viewer executable and $script:RvDir to the
# output directory before calling Start-Ui.

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

if (-not ('RvW' -as [type])) {
Add-Type -ReferencedAssemblies System.Drawing @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class RvW {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L; public int T; public int R; public int B; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool ScreenToClient(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr ctx);
    [DllImport("user32.dll")] public static extern bool IsHungAppWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern uint MapVirtualKey(uint code, uint mapType);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
    [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint flags);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr parent, EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern int GetDlgCtrlID(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr h);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, EntryPoint="SendMessageW")] public static extern IntPtr SendMessageText(IntPtr h, uint msg, IntPtr w, string l);
    [DllImport("user32.dll", EntryPoint="SendMessageW")] public static extern IntPtr SendMessageRaw(IntPtr h, uint msg, IntPtr w, IntPtr l);

    private static List<IntPtr> titled = new List<IntPtr>();
    private static string wantTitle = "";
    private static bool CollectTitled(IntPtr h, IntPtr l) {
        if (IsWindowVisible(h) && Title(h).IndexOf(wantTitle, StringComparison.OrdinalIgnoreCase) >= 0) { titled.Add(h); }
        return true;
    }
    // Visible top-level windows of ANY process whose title contains the text
    // (used for the print driver's "Save Print Output As" dialog).
    public static IntPtr[] TopLevelByTitle(string part) {
        titled = new List<IntPtr>(); wantTitle = part;
        EnumWindows(CollectTitled, IntPtr.Zero);
        return titled.ToArray();
    }

    private static List<IntPtr> kids = new List<IntPtr>();
    private static bool CollectKid(IntPtr h, IntPtr l) { kids.Add(h); return true; }
    public static IntPtr[] Children(IntPtr parent) {
        kids = new List<IntPtr>();
        EnumChildWindows(parent, CollectKid, IntPtr.Zero);
        return kids.ToArray();
    }
    // The file-name edit of a common file dialog: an "Edit" whose parent chain
    // contains a ComboBox (Open dialogs) or whose control id is 1001 (Save dialogs).
    public static IntPtr FileNameEdit(IntPtr dialog) {
        IntPtr fallback = IntPtr.Zero;
        foreach (IntPtr h in Children(dialog)) {
            if (Class(h) != "Edit") { continue; }
            IntPtr p = GetParent(h);
            string pc = p == IntPtr.Zero ? "" : Class(p);
            if (pc == "ComboBox" || pc == "ComboBoxEx32") { return h; }
            int id = GetDlgCtrlID(h);
            if (id == 1001 || id == 1148) { fallback = h; }
        }
        return fallback;
    }
    public static IntPtr ChildButton(IntPtr dialog, int id) {
        foreach (IntPtr h in Children(dialog)) {
            if (Class(h) == "Button" && GetDlgCtrlID(h) == id && IsWindowVisible(h)) { return h; }
        }
        return IntPtr.Zero;
    }

    [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Explicit)] public struct INPUTUNION { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
    [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION u; }
    [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] inputs, int size);

    public static void MouseButton(bool down, bool right) {
        INPUT[] a = new INPUT[1];
        a[0].type = 0;
        a[0].u.mi.dwFlags = right ? (down ? 0x0008u : 0x0010u) : (down ? 0x0002u : 0x0004u);
        SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
    }
    public static void MouseWheel(int delta) {
        INPUT[] a = new INPUT[1];
        a[0].type = 0;
        a[0].u.mi.dwFlags = 0x0800u;
        a[0].u.mi.mouseData = unchecked((uint)delta);
        SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
    }
    public static void Key(ushort vk, bool down) {
        INPUT[] a = new INPUT[1];
        a[0].type = 1;
        a[0].u.ki.wVk = vk;
        a[0].u.ki.wScan = (ushort)MapVirtualKey(vk, 0);
        a[0].u.ki.dwFlags = down ? 0u : 0x0002u;
        SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
    }
    public static void UnicodeChar(char c) {
        INPUT[] a = new INPUT[2];
        a[0].type = 1; a[0].u.ki.wScan = (ushort)c; a[0].u.ki.dwFlags = 0x0004u;
        a[1].type = 1; a[1].u.ki.wScan = (ushort)c; a[1].u.ki.dwFlags = 0x0004u | 0x0002u;
        SendInput(2, a, Marshal.SizeOf(typeof(INPUT)));
    }

    private static List<IntPtr> found = new List<IntPtr>();
    private static uint wantPid = 0;
    private static bool Collect(IntPtr h, IntPtr l) {
        uint pid; GetWindowThreadProcessId(h, out pid);
        if (pid == wantPid && IsWindowVisible(h)) { found.Add(h); }
        return true;
    }
    // Visible top-level windows of a process, excluding winit's hidden
    // "Winit Thread Event Target" helper window, which is not a UI surface.
    public static IntPtr[] WindowsOf(uint pid) {
        found = new List<IntPtr>(); wantPid = pid;
        EnumWindows(Collect, IntPtr.Zero);
        List<IntPtr> real = new List<IntPtr>();
        foreach (IntPtr h in found) {
            if (Class(h) != "Winit Thread Event Target") { real.Add(h); }
        }
        return real.ToArray();
    }
    public static string Title(IntPtr h) { StringBuilder s = new StringBuilder(512); GetWindowText(h, s, 512); return s.ToString(); }
    public static string Class(IntPtr h) { StringBuilder s = new StringBuilder(256); GetClassName(h, s, 256); return s.ToString(); }
    public static IntPtr MakeLParam(int x, int y) { return (IntPtr)(((y & 0xFFFF) << 16) | (x & 0xFFFF)); }

    // Colour statistics over a sparse sample grid; share=1.0 means a flat image.
    public static string Stats(System.Drawing.Bitmap bmp) {
        Dictionary<int,int> counts = new Dictionary<int,int>();
        int n = 0;
        for (int y = 0; y < bmp.Height; y += 8) {
            for (int x = 0; x < bmp.Width; x += 8) {
                int c = bmp.GetPixel(x, y).ToArgb();
                int v; counts.TryGetValue(c, out v); counts[c] = v + 1; n++;
            }
        }
        int best = 0; int bestColor = 0;
        foreach (KeyValuePair<int,int> kv in counts) { if (kv.Value > best) { best = kv.Value; bestColor = kv.Key; } }
        return String.Format("colors={0} dominant=#{1:X8} share={2:F3}", counts.Count, bestColor, (double)best / Math.Max(1, n));
    }
}
"@
}
[void][RvW]::SetThreadDpiAwarenessContext([IntPtr]-4)

$script:RvDir = $PSScriptRoot
$script:RvUi  = Join-Path $env:LOCALAPPDATA 'Programs\PDFXI\pdfxi-ui.exe'
$script:RvCli = Join-Path $env:LOCALAPPDATA 'Programs\PDFXI\pdfxi.exe'

function Get-UiProc {
    @(Get-Process pdfxi-ui -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $script:RvUi })
}

function Get-UiWin {
    # Largest visible top-level window of the installed viewer process.
    $procs = Get-UiProc
    if (-not $procs.Count) { throw 'installed pdfxi-ui is not running' }
    $best = [IntPtr]::Zero; $area = 0
    foreach ($p in $procs) {
        foreach ($h in [RvW]::WindowsOf([uint32]$p.Id)) {
            $r = New-Object RvW+RECT
            [void][RvW]::GetWindowRect($h, [ref]$r)
            $a = ($r.R - $r.L) * ($r.B - $r.T)
            if ($a -gt $area) { $area = $a; $best = $h }
        }
    }
    if ($best -eq [IntPtr]::Zero) { throw 'no visible window' }
    $best
}

function Get-AllUiWins {
    foreach ($p in Get-UiProc) {
        foreach ($h in [RvW]::WindowsOf([uint32]$p.Id)) {
            $r = New-Object RvW+RECT
            [void][RvW]::GetWindowRect($h, [ref]$r)
            "pid=$($p.Id) hwnd=$h class='$([RvW]::Class($h))' title='$([RvW]::Title($h))' rect=($($r.L),$($r.T),$($r.R-$r.L)x$($r.B-$r.T)) hung=$([RvW]::IsHungAppWindow($h))"
        }
    }
}

function Start-Ui {
    param([string[]]$ArgList = @(), [int]$WaitMs = 5000, [int]$Width = 0, [int]$Height = 0)
    $NoResize = ($Width -le 0 -or $Height -le 0)
    $errFile = Join-Path $script:RvDir 'ui-stderr.txt'
    $outFile = Join-Path $script:RvDir 'ui-stdout.txt'
    if ($ArgList.Count) {
        $p = Start-Process -FilePath $script:RvUi -ArgumentList $ArgList -PassThru -RedirectStandardError $errFile -RedirectStandardOutput $outFile
    } else {
        $p = Start-Process -FilePath $script:RvUi -PassThru -RedirectStandardError $errFile -RedirectStandardOutput $outFile
    }
    $deadline = (Get-Date).AddMilliseconds($WaitMs + 15000)
    $window = [IntPtr]::Zero
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 250
        $p.Refresh()
        if ($p.HasExited) { throw "viewer exited early with code $($p.ExitCode)" }
        $wins = [RvW]::WindowsOf([uint32]$p.Id)
        if ($wins.Count) { $window = $wins[0]; break }
    }
    if ($window -eq [IntPtr]::Zero) { throw 'viewer window did not appear' }
    Start-Sleep -Milliseconds $WaitMs
    if (-not $NoResize) {
        # SWP_NOMOVE(2)|SWP_NOZORDER(4)|SWP_NOACTIVATE(0x10)
        [void][RvW]::SetWindowPos($window, [IntPtr]::Zero, 0, 0, $Width, $Height, 0x16)
        Start-Sleep -Milliseconds 700
    }
    "STARTED pid=$($p.Id) hwnd=$window responding=$($p.Responding) title='$([RvW]::Title($window))'"
}

function Shot {
    param([Parameter(Mandatory = $true)][string]$Name, [IntPtr]$Hwnd = [IntPtr]::Zero)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $r = New-Object RvW+RECT
    [void][RvW]::GetWindowRect($Hwnd, [ref]$r)
    $w = $r.R - $r.L; $h = $r.B - $r.T
    if ($w -le 0 -or $h -le 0) { throw "window has no area ($w x $h)" }
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    try { $ok = [RvW]::PrintWindow($Hwnd, $hdc, 2) } finally { $g.ReleaseHdc($hdc) }
    $path = Join-Path $script:RvDir ($Name + '.png')
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $stats = [RvW]::Stats($bmp)
    $g.Dispose(); $bmp.Dispose()
    "SHOT $path ${w}x${h} printwindow=$ok $stats"
}

function Get-UiRoot {
    param([IntPtr]$Hwnd = [IntPtr]::Zero)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    [System.Windows.Automation.AutomationElement]::FromHandle($Hwnd)
}

function Get-ElPatterns {
    param($El)
    $names = @()
    foreach ($p in $El.GetSupportedPatterns()) { $names += ($p.ProgrammaticName -replace 'PatternIdentifiers\.Pattern$', '' -replace 'Pattern$', '') }
    $names -join ','
}

function Format-El {
    param($El, [int]$Depth = 0)
    try {
        $c = $El.Current
        $rect = $c.BoundingRectangle
        $rs = if ($rect.IsEmpty) { 'empty' } else { '{0:0},{1:0},{2:0}x{3:0}' -f $rect.X, $rect.Y, $rect.Width, $rect.Height }
        $type = $c.ControlType.ProgrammaticName -replace '^ControlType\.', ''
        $val = ''
        $vp = $null
        if ($El.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$vp)) { $val = " value='" + $vp.Current.Value + "'" }
        $tp = $null
        if ($El.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$tp)) { $val += " toggle=" + $tp.Current.ToggleState }
        $sp = $null
        if ($El.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$sp)) { $val += " selected=" + $sp.Current.IsSelected }
        ('{0}{1} "{2}"{3} en={4} off={5} [{6}] {{{7}}}' -f ('  ' * $Depth), $type, $c.Name, $val, $c.IsEnabled, $c.IsOffscreen, $rs, (Get-ElPatterns $El))
    } catch { ('{0}<stale element>' -f ('  ' * $Depth)) }
}

function Tree {
    # Dump the UI Automation tree of the viewer window.
    param([int]$MaxDepth = 12, [string]$Match = '', [IntPtr]$Hwnd = [IntPtr]::Zero, [int]$MaxNodes = 1500)
    $root = Get-UiRoot $Hwnd
    $walker = [System.Windows.Automation.TreeWalker]::RawViewWalker
    $script:RvCount = 0
    $stack = New-Object System.Collections.Stack
    $stack.Push(@($root, 0))
    $out = New-Object System.Collections.Generic.List[string]
    while ($stack.Count -and $script:RvCount -lt $MaxNodes) {
        $item = $stack.Pop(); $el = $item[0]; $d = $item[1]
        $script:RvCount++
        $line = Format-El $el $d
        if (-not $Match -or $line -match $Match) { $out.Add($line) }
        if ($d -lt $MaxDepth) {
            $kids = @()
            try { $k = $walker.GetFirstChild($el); while ($k) { $kids += $k; $k = $walker.GetNextSibling($k) } } catch { }
            for ($i = $kids.Count - 1; $i -ge 0; $i--) { $stack.Push(@($kids[$i], $d + 1)) }
        }
    }
    $out
    "TREE nodes=$($script:RvCount)"
}

function Find-El {
    # Find elements by exact Name (or -Regex), optionally by control type name.
    param([Parameter(Mandatory = $true)][string]$Name, [switch]$Regex, [string]$Type = '', [IntPtr]$Hwnd = [IntPtr]::Zero)
    $root = Get-UiRoot $Hwnd
    $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    $hits = @()
    foreach ($el in $all) {
        try {
            $n = $el.Current.Name
            $t = $el.Current.ControlType.ProgrammaticName -replace '^ControlType\.', ''
            $ok = if ($Regex) { $n -match $Name } else { $n -eq $Name }
            if ($ok -and (-not $Type -or $t -eq $Type)) { $hits += $el }
        } catch { }
    }
    , $hits
}

function Invoke-El {
    # Activate a control by name through UI Automation (Invoke/Toggle/SelectionItem).
    param([Parameter(Mandatory = $true)][string]$Name, [switch]$Regex, [string]$Type = '', [int]$Index = 0, [int]$SettleMs = 600, [IntPtr]$Hwnd = [IntPtr]::Zero)
    $hits = Find-El -Name $Name -Regex:$Regex -Type $Type -Hwnd $Hwnd
    if (-not $hits.Count) { "INVOKE '$Name' -> NOT FOUND"; return }
    $el = $hits[$Index]
    $how = 'none'
    $p = $null
    if ($el.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { $p.Invoke(); $how = 'Invoke' }
    elseif ($el.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$p)) { $p.Toggle(); $how = 'Toggle' }
    elseif ($el.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$p)) { $p.Select(); $how = 'Select' }
    elseif ($el.TryGetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern, [ref]$p)) { $p.Expand(); $how = 'Expand' }
    Start-Sleep -Milliseconds $SettleMs
    "INVOKE '$Name' hits=$($hits.Count) idx=$Index via=$how enabled=$($el.Current.IsEnabled)"
}

function Set-ElValue {
    param([Parameter(Mandatory = $true)][string]$Name, [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Value, [switch]$Regex, [string]$Type = '', [int]$Index = 0, [IntPtr]$Hwnd = [IntPtr]::Zero)
    $hits = Find-El -Name $Name -Regex:$Regex -Type $Type -Hwnd $Hwnd
    if (-not $hits.Count) { "SETVALUE '$Name' -> NOT FOUND"; return }
    $el = $hits[$Index]
    $p = $null
    if ($el.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$p)) {
        try { $p.SetValue($Value); Start-Sleep -Milliseconds 400; "SETVALUE '$Name' ok now='$($p.Current.Value)'" } catch { "SETVALUE '$Name' failed: $($_.Exception.Message)" }
    } else { "SETVALUE '$Name' -> no ValuePattern" }
}

function Click-Client {
    # Post a left click at client-area pixel coordinates (does not move the real cursor).
    param([int]$X, [int]$Y, [IntPtr]$Hwnd = [IntPtr]::Zero, [int]$SettleMs = 500, [switch]$Double, [switch]$Right)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $lp = [RvW]::MakeLParam($X, $Y)
    $down = 0x0201; $up = 0x0202; $btn = 1
    if ($Right) { $down = 0x0204; $up = 0x0205; $btn = 2 }
    # Queue move/down/up back-to-back: egui drops button events once it has seen
    # the WM_MOUSELEAVE that follows a synthetic move while the real cursor is
    # elsewhere, so all three must be queued before that leave is generated.
    [void][RvW]::PostMessage($Hwnd, 0x0200, [IntPtr]::Zero, $lp)   # WM_MOUSEMOVE
    [void][RvW]::PostMessage($Hwnd, $down, [IntPtr]$btn, $lp)
    [void][RvW]::PostMessage($Hwnd, $up, [IntPtr]::Zero, $lp)
    if ($Double) {
        [void][RvW]::PostMessage($Hwnd, 0x0200, [IntPtr]::Zero, $lp)
        [void][RvW]::PostMessage($Hwnd, $down, [IntPtr]$btn, $lp)
        [void][RvW]::PostMessage($Hwnd, $up, [IntPtr]::Zero, $lp)
    }
    Start-Sleep -Milliseconds $SettleMs
    "CLICK client=($X,$Y)"
}

function Click-Screen {
    # Post a click given SCREEN pixel coordinates (e.g. from a UIA bounding rectangle).
    param([int]$X, [int]$Y, [IntPtr]$Hwnd = [IntPtr]::Zero, [int]$SettleMs = 500, [switch]$Double)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $pt = New-Object RvW+POINT; $pt.X = $X; $pt.Y = $Y
    [void][RvW]::ScreenToClient($Hwnd, [ref]$pt)
    Click-Client -X $pt.X -Y $pt.Y -Hwnd $Hwnd -SettleMs $SettleMs -Double:$Double
}

function Click-El {
    # Click the centre of a named element with posted mouse messages.
    param([Parameter(Mandatory = $true)][string]$Name, [switch]$Regex, [string]$Type = '', [int]$Index = 0, [int]$SettleMs = 600, [IntPtr]$Hwnd = [IntPtr]::Zero, [switch]$Double)
    $hits = Find-El -Name $Name -Regex:$Regex -Type $Type -Hwnd $Hwnd
    if (-not $hits.Count) { "CLICKEL '$Name' -> NOT FOUND"; return }
    $r = $hits[$Index].Current.BoundingRectangle
    if ($r.IsEmpty) { "CLICKEL '$Name' -> empty rect"; return }
    $cx = [int]($r.X + $r.Width / 2); $cy = [int]($r.Y + $r.Height / 2)
    Click-Screen -X $cx -Y $cy -Hwnd $Hwnd -SettleMs $SettleMs -Double:$Double
    "CLICKEL '$Name' hits=$($hits.Count) idx=$Index screen=($cx,$cy)"
}

function Drag-Client {
    param([int]$X1, [int]$Y1, [int]$X2, [int]$Y2, [IntPtr]$Hwnd = [IntPtr]::Zero, [int]$Steps = 12, [int]$SettleMs = 500)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    [void][RvW]::PostMessage($Hwnd, 0x0200, [IntPtr]::Zero, [RvW]::MakeLParam($X1, $Y1))
    Start-Sleep -Milliseconds 80
    [void][RvW]::PostMessage($Hwnd, 0x0201, [IntPtr]1, [RvW]::MakeLParam($X1, $Y1))
    for ($i = 1; $i -le $Steps; $i++) {
        $x = [int]($X1 + ($X2 - $X1) * $i / $Steps); $y = [int]($Y1 + ($Y2 - $Y1) * $i / $Steps)
        Start-Sleep -Milliseconds 30
        [void][RvW]::PostMessage($Hwnd, 0x0200, [IntPtr]1, [RvW]::MakeLParam($x, $y))
    }
    Start-Sleep -Milliseconds 80
    [void][RvW]::PostMessage($Hwnd, 0x0202, [IntPtr]::Zero, [RvW]::MakeLParam($X2, $Y2))
    Start-Sleep -Milliseconds $SettleMs
    "DRAG client=($X1,$Y1)->($X2,$Y2)"
}

function Wheel-Client {
    param([int]$X, [int]$Y, [int]$Notches = -3, [IntPtr]$Hwnd = [IntPtr]::Zero, [int]$SettleMs = 500)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $pt = New-Object RvW+POINT; $pt.X = $X; $pt.Y = $Y
    [void][RvW]::ClientToScreen($Hwnd, [ref]$pt)
    [void][RvW]::PostMessage($Hwnd, 0x0200, [IntPtr]::Zero, [RvW]::MakeLParam($X, $Y))
    Start-Sleep -Milliseconds 60
    $wparam = [IntPtr]([int64]((($Notches * 120) -band 0xFFFF) -shl 16))
    [void][RvW]::PostMessage($Hwnd, 0x020A, $wparam, [RvW]::MakeLParam($pt.X, $pt.Y))  # WM_MOUSEWHEEL uses screen coords
    Start-Sleep -Milliseconds $SettleMs
    "WHEEL client=($X,$Y) notches=$Notches"
}

function Post-Key {
    # Post a plain key press (no modifiers) by virtual-key code; optional character for text input.
    param([int]$Vk, [string]$Char = '', [IntPtr]$Hwnd = [IntPtr]::Zero, [int]$SettleMs = 150)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $scan = [int][RvW]::MapVirtualKey([uint32]$Vk, 0)
    $lpDown = [IntPtr]([int64](1 -bor ($scan -shl 16)))
    $lpUp = [IntPtr]([int64](1 -bor ($scan -shl 16)) -bor 0xC0000000L)
    [void][RvW]::PostMessage($Hwnd, 0x0100, [IntPtr]$Vk, $lpDown)
    if ($Char) { [void][RvW]::PostMessage($Hwnd, 0x0102, [IntPtr][int][char]$Char, $lpDown) }
    Start-Sleep -Milliseconds 30
    [void][RvW]::PostMessage($Hwnd, 0x0101, [IntPtr]$Vk, $lpUp)
    Start-Sleep -Milliseconds $SettleMs
}

function Post-Text {
    param([Parameter(Mandatory = $true)][string]$Text, [IntPtr]$Hwnd = [IntPtr]::Zero)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    foreach ($ch in $Text.ToCharArray()) {
        $vk = [int][char]([string]$ch).ToUpperInvariant()
        if ($ch -eq ' ') { $vk = 0x20 }
        Post-Key -Vk $vk -Char ([string]$ch) -Hwnd $Hwnd -SettleMs 40
    }
    Start-Sleep -Milliseconds 300
    "TEXT '$Text'"
}

function Get-WinOrigin {
    param([IntPtr]$Hwnd = [IntPtr]::Zero)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $r = New-Object RvW+RECT
    [void][RvW]::GetWindowRect($Hwnd, [ref]$r)
    , @($r.L, $r.T, ($r.R - $r.L), ($r.B - $r.T))
}

function Invoke-Pt {
    # UIA-invoke the smallest invokable element containing a WINDOW-relative pixel
    # (the same coordinates as in a Shot screenshot).
    param([int]$X, [int]$Y, [int]$SettleMs = 700, [IntPtr]$Hwnd = [IntPtr]::Zero)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $o = Get-WinOrigin $Hwnd
    $sx = $o[0] + $X; $sy = $o[1] + $Y
    $root = Get-UiRoot $Hwnd
    $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    $best = $null; $bestArea = [double]::MaxValue
    foreach ($el in $all) {
        try {
            $b = $el.Current.BoundingRectangle
            if ($b.IsEmpty) { continue }
            if ($sx -ge $b.X -and $sx -lt ($b.X + $b.Width) -and $sy -ge $b.Y -and $sy -lt ($b.Y + $b.Height)) {
                $p = $null
                if ($el.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) {
                    $a = $b.Width * $b.Height
                    if ($a -lt $bestArea) { $bestArea = $a; $best = $el }
                }
            }
        } catch { }
    }
    if (-not $best) { "INVOKEPT ($X,$Y) -> no invokable element"; return }
    $desc = Format-El $best 0
    $ip = $best.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    $ip.Invoke()
    Start-Sleep -Milliseconds $SettleMs
    "INVOKEPT ($X,$Y) -> $desc"
}

function Click-Pt {
    # Posted mouse click at WINDOW-relative pixel coordinates (as in a Shot screenshot).
    param([int]$X, [int]$Y, [int]$SettleMs = 600, [IntPtr]$Hwnd = [IntPtr]::Zero, [switch]$Double, [switch]$Right)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $o = Get-WinOrigin $Hwnd
    $pt = New-Object RvW+POINT; $pt.X = $o[0] + $X; $pt.Y = $o[1] + $Y
    [void][RvW]::ScreenToClient($Hwnd, [ref]$pt)
    Click-Client -X $pt.X -Y $pt.Y -Hwnd $Hwnd -SettleMs $SettleMs -Double:$Double -Right:$Right
}

function Drag-Pt {
    param([int]$X1, [int]$Y1, [int]$X2, [int]$Y2, [IntPtr]$Hwnd = [IntPtr]::Zero, [int]$Steps = 14, [int]$SettleMs = 600)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $o = Get-WinOrigin $Hwnd
    $a = New-Object RvW+POINT; $a.X = $o[0] + $X1; $a.Y = $o[1] + $Y1; [void][RvW]::ScreenToClient($Hwnd, [ref]$a)
    $b = New-Object RvW+POINT; $b.X = $o[0] + $X2; $b.Y = $o[1] + $Y2; [void][RvW]::ScreenToClient($Hwnd, [ref]$b)
    Drag-Client -X1 $a.X -Y1 $a.Y -X2 $b.X -Y2 $b.Y -Hwnd $Hwnd -Steps $Steps -SettleMs $SettleMs
}

function Wheel-Pt {
    param([int]$X, [int]$Y, [int]$Notches = -3, [IntPtr]$Hwnd = [IntPtr]::Zero, [int]$SettleMs = 600)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $o = Get-WinOrigin $Hwnd
    $pt = New-Object RvW+POINT; $pt.X = $o[0] + $X; $pt.Y = $o[1] + $Y
    [void][RvW]::ScreenToClient($Hwnd, [ref]$pt)
    Wheel-Client -X $pt.X -Y $pt.Y -Notches $Notches -Hwnd $Hwnd -SettleMs $SettleMs
}

# ---- Real input (SendInput). Used only where UI Automation cannot reach: ----
# canvas drags and keyboard chords. Each burst saves and restores the cursor
# position and the previously focused window, and refuses to act unless the
# target point really belongs to the viewer window.

$script:RvSaved = $null

function Enter-Real {
    param([IntPtr]$Hwnd = [IntPtr]::Zero)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $pos = New-Object RvW+POINT
    [void][RvW]::GetCursorPos([ref]$pos)
    $script:RvSaved = @{ X = $pos.X; Y = $pos.Y; Fg = [RvW]::GetForegroundWindow() }
    $fg = [RvW]::GetForegroundWindow()
    if ($fg -ne $Hwnd) {
        try { (Get-UiRoot $Hwnd).SetFocus() } catch { }
        Start-Sleep -Milliseconds 150
        if ([RvW]::GetForegroundWindow() -ne $Hwnd) {
            $pidOut = 0
            $t1 = [RvW]::GetWindowThreadProcessId($fg, [ref]$pidOut)
            $t2 = [RvW]::GetCurrentThreadId()
            [void][RvW]::AttachThreadInput($t2, $t1, $true)
            [void][RvW]::BringWindowToTop($Hwnd)
            [void][RvW]::SetForegroundWindow($Hwnd)
            [void][RvW]::AttachThreadInput($t2, $t1, $false)
            Start-Sleep -Milliseconds 150
        }
    }
    ([RvW]::GetForegroundWindow() -eq $Hwnd)
}

function Exit-Real {
    if ($script:RvSaved) {
        [void][RvW]::SetCursorPos($script:RvSaved.X, $script:RvSaved.Y)
        if ($script:RvSaved.Fg -ne [IntPtr]::Zero -and [RvW]::IsWindow($script:RvSaved.Fg)) { [void][RvW]::SetForegroundWindow($script:RvSaved.Fg) }
        $script:RvSaved = $null
    }
}

function Test-PointIsUi {
    param([int]$Sx, [int]$Sy, [IntPtr]$Hwnd)
    $pt = New-Object RvW+POINT; $pt.X = $Sx; $pt.Y = $Sy
    $at = [RvW]::WindowFromPoint($pt)
    $top = [RvW]::GetAncestor($at, 2)
    ($top -eq $Hwnd -or $at -eq $Hwnd)
}

function Real-Click {
    param([int]$X, [int]$Y, [switch]$Double, [switch]$Right, [int]$SettleMs = 600, [IntPtr]$Hwnd = [IntPtr]::Zero)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $o = Get-WinOrigin $Hwnd; $sx = $o[0] + $X; $sy = $o[1] + $Y
    $fg = Enter-Real $Hwnd
    try {
        if (-not (Test-PointIsUi $sx $sy $Hwnd)) { "REALCLICK ($X,$Y) ABORTED: point is not on the viewer (foreground=$fg)"; return }
        [void][RvW]::SetCursorPos($sx, $sy); Start-Sleep -Milliseconds 120
        [RvW]::MouseButton($true, [bool]$Right); Start-Sleep -Milliseconds 60; [RvW]::MouseButton($false, [bool]$Right)
        if ($Double) { Start-Sleep -Milliseconds 90; [RvW]::MouseButton($true, [bool]$Right); Start-Sleep -Milliseconds 60; [RvW]::MouseButton($false, [bool]$Right) }
        Start-Sleep -Milliseconds $SettleMs
        "REALCLICK ($X,$Y) foreground=$fg"
    } finally { Exit-Real }
}

function Real-Drag {
    param([int]$X1, [int]$Y1, [int]$X2, [int]$Y2, [int]$Steps = 16, [int]$SettleMs = 600, [IntPtr]$Hwnd = [IntPtr]::Zero)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $o = Get-WinOrigin $Hwnd
    $fg = Enter-Real $Hwnd
    try {
        if (-not (Test-PointIsUi ($o[0] + $X1) ($o[1] + $Y1) $Hwnd)) { "REALDRAG ABORTED: start point is not on the viewer (foreground=$fg)"; return }
        [void][RvW]::SetCursorPos($o[0] + $X1, $o[1] + $Y1); Start-Sleep -Milliseconds 150
        [RvW]::MouseButton($true, $false); Start-Sleep -Milliseconds 120
        for ($i = 1; $i -le $Steps; $i++) {
            $x = [int]($X1 + ($X2 - $X1) * $i / $Steps); $y = [int]($Y1 + ($Y2 - $Y1) * $i / $Steps)
            [void][RvW]::SetCursorPos($o[0] + $x, $o[1] + $y); Start-Sleep -Milliseconds 35
        }
        Start-Sleep -Milliseconds 150
        [RvW]::MouseButton($false, $false)
        Start-Sleep -Milliseconds $SettleMs
        "REALDRAG ($X1,$Y1)->($X2,$Y2) foreground=$fg"
    } finally { Exit-Real }
}

function Real-Chord {
    # Press a key chord by virtual-key codes, e.g. Real-Chord 0x11,0x4F for Ctrl+O.
    param([Parameter(Mandatory = $true)][int[]]$Vk, [int]$SettleMs = 700, [IntPtr]$Hwnd = [IntPtr]::Zero)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $fg = Enter-Real $Hwnd
    try {
        if (-not $fg) { "REALCHORD ABORTED: viewer is not the foreground window"; return }
        foreach ($k in $Vk) { [RvW]::Key([uint16]$k, $true); Start-Sleep -Milliseconds 40 }
        for ($i = $Vk.Count - 1; $i -ge 0; $i--) { [RvW]::Key([uint16]$Vk[$i], $false); Start-Sleep -Milliseconds 40 }
        Start-Sleep -Milliseconds $SettleMs
        "REALCHORD $((($Vk | ForEach-Object { '0x{0:X2}' -f $_ }) -join '+')) foreground=$fg"
    } finally { Exit-Real }
}

function Real-Type {
    param([Parameter(Mandatory = $true)][string]$Text, [int]$SettleMs = 500, [IntPtr]$Hwnd = [IntPtr]::Zero)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $fg = Enter-Real $Hwnd
    try {
        if (-not $fg) { "REALTYPE ABORTED: viewer is not the foreground window"; return }
        foreach ($ch in $Text.ToCharArray()) { [RvW]::UnicodeChar($ch); Start-Sleep -Milliseconds 25 }
        Start-Sleep -Milliseconds $SettleMs
        "REALTYPE '$Text' foreground=$fg"
    } finally { Exit-Real }
}

function Real-ClickType {
    # Click a point to focus it, then type, without giving focus back in between.
    param([int]$X, [int]$Y, [Parameter(Mandatory = $true)][string]$Text, [switch]$SelectAllFirst, [switch]$Enter, [int]$SettleMs = 600, [IntPtr]$Hwnd = [IntPtr]::Zero)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $o = Get-WinOrigin $Hwnd; $sx = $o[0] + $X; $sy = $o[1] + $Y
    $fg = Enter-Real $Hwnd
    try {
        if (-not (Test-PointIsUi $sx $sy $Hwnd)) { "REALCLICKTYPE ($X,$Y) ABORTED: point is not on the viewer (foreground=$fg)"; return }
        [void][RvW]::SetCursorPos($sx, $sy); Start-Sleep -Milliseconds 120
        [RvW]::MouseButton($true, $false); Start-Sleep -Milliseconds 60; [RvW]::MouseButton($false, $false)
        Start-Sleep -Milliseconds 250
        if ($SelectAllFirst) { [RvW]::Key(0x11, $true); [RvW]::Key(0x41, $true); Start-Sleep -Milliseconds 40; [RvW]::Key(0x41, $false); [RvW]::Key(0x11, $false); Start-Sleep -Milliseconds 120 }
        foreach ($ch in $Text.ToCharArray()) { [RvW]::UnicodeChar($ch); Start-Sleep -Milliseconds 25 }
        if ($Enter) { Start-Sleep -Milliseconds 120; [RvW]::Key(0x0D, $true); Start-Sleep -Milliseconds 40; [RvW]::Key(0x0D, $false) }
        Start-Sleep -Milliseconds $SettleMs
        "REALCLICKTYPE ($X,$Y) '$Text' foreground=$fg"
    } finally { Exit-Real }
}

function Real-Seq {
    # Run several real-input steps in ONE focus session, so focus-sensitive
    # widgets (inline editors) are not disturbed between steps. Steps are strings:
    #   'click:X,Y'  'dclick:X,Y'  'key:0x0D'  'chord:0x11+0x53'  'text:hello'  'sleep:300'
    param([Parameter(Mandatory = $true)][string[]]$Steps, [int]$SettleMs = 600, [IntPtr]$Hwnd = [IntPtr]::Zero)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $o = Get-WinOrigin $Hwnd
    $fg = Enter-Real $Hwnd
    try {
        if (-not $fg) { 'REALSEQ ABORTED: viewer is not the foreground window'; return }
        foreach ($s in $Steps) {
            $kind, $arg = $s -split ':', 2
            switch ($kind) {
                'click' {
                    $xy = $arg -split ','; $sx = $o[0] + [int]$xy[0]; $sy = $o[1] + [int]$xy[1]
                    if (-not (Test-PointIsUi $sx $sy $Hwnd)) { "REALSEQ ABORTED at '$s': point is not on the viewer"; return }
                    [void][RvW]::SetCursorPos($sx, $sy); Start-Sleep -Milliseconds 120
                    [RvW]::MouseButton($true, $false); Start-Sleep -Milliseconds 60; [RvW]::MouseButton($false, $false); Start-Sleep -Milliseconds 250
                }
                'dclick' {
                    $xy = $arg -split ','; $sx = $o[0] + [int]$xy[0]; $sy = $o[1] + [int]$xy[1]
                    if (-not (Test-PointIsUi $sx $sy $Hwnd)) { "REALSEQ ABORTED at '$s': point is not on the viewer"; return }
                    [void][RvW]::SetCursorPos($sx, $sy); Start-Sleep -Milliseconds 120
                    [RvW]::MouseButton($true, $false); Start-Sleep -Milliseconds 50; [RvW]::MouseButton($false, $false); Start-Sleep -Milliseconds 80
                    [RvW]::MouseButton($true, $false); Start-Sleep -Milliseconds 50; [RvW]::MouseButton($false, $false); Start-Sleep -Milliseconds 250
                }
                'key' { $vk = [Convert]::ToInt32($arg, 16); [RvW]::Key([uint16]$vk, $true); Start-Sleep -Milliseconds 40; [RvW]::Key([uint16]$vk, $false); Start-Sleep -Milliseconds 200 }
                'chord' {
                    $vks = $arg -split '\+' | ForEach-Object { [Convert]::ToInt32($_, 16) }
                    foreach ($k in $vks) { [RvW]::Key([uint16]$k, $true); Start-Sleep -Milliseconds 40 }
                    for ($i = $vks.Count - 1; $i -ge 0; $i--) { [RvW]::Key([uint16]$vks[$i], $false); Start-Sleep -Milliseconds 40 }
                    Start-Sleep -Milliseconds 200
                }
                'text' { foreach ($ch in $arg.ToCharArray()) { [RvW]::UnicodeChar($ch); Start-Sleep -Milliseconds 25 }; Start-Sleep -Milliseconds 150 }
                'sleep' { Start-Sleep -Milliseconds ([int]$arg) }
            }
        }
        Start-Sleep -Milliseconds $SettleMs
        "REALSEQ $($Steps -join ' | ')"
    } finally { Exit-Real }
}

# ---- Native dialogs (file pickers, print, message boxes) owned by the viewer ----

function Get-Dialogs {
    # Top-level windows of the viewer other than its main egui window.
    $main = $null
    try { $main = Get-UiWin } catch { }
    foreach ($p in Get-UiProc) {
        foreach ($h in [RvW]::WindowsOf([uint32]$p.Id)) {
            if ($h -ne $main -and [RvW]::Class($h) -ne 'Window Class') { $h }
        }
    }
}

function Show-Dialogs {
    $d = @(Get-Dialogs)
    if (-not $d.Count) { 'DIALOGS none'; return }
    foreach ($h in $d) {
        "DIALOG hwnd=$h class='$([RvW]::Class($h))' title='$([RvW]::Title($h))'"
    }
}

function Dialog-Tree {
    param([IntPtr]$Hwnd, [string]$Match = 'Button|Edit|ComboBox|Text|ListItem|RadioButton|CheckBox', [int]$Max = 60)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($Hwnd)
    $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    $n = 0
    foreach ($el in $all) {
        try {
            $c = $el.Current
            $type = $c.ControlType.ProgrammaticName -replace '^ControlType\.', ''
            if ($type -notmatch $Match) { continue }
            $val = ''
            $vp = $null
            if ($el.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$vp)) { $val = " value='" + $vp.Current.Value + "'" }
            if (-not $c.Name -and -not $val) { continue }
            ('  {0} "{1}" id={2}{3} en={4}' -f $type, $c.Name, $c.AutomationId, $val, $c.IsEnabled)
            $n++; if ($n -ge $Max) { break }
        } catch { }
    }
    "DIALOGTREE shown=$n total=$($all.Count)"
}

function Dialog-Find {
    param([IntPtr]$Hwnd, [string]$Name = '', [string]$Id = '', [string]$Type = '')
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($Hwnd)
    $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    foreach ($el in $all) {
        try {
            $c = $el.Current
            $t = $c.ControlType.ProgrammaticName -replace '^ControlType\.', ''
            if ($Type -and $t -ne $Type) { continue }
            if ($Id -and $c.AutomationId -ne $Id) { continue }
            if ($Name -and $c.Name -ne $Name) { continue }
            return $el
        } catch { }
    }
    $null
}

function Dialog-Invoke {
    param([IntPtr]$Hwnd, [string]$Name = '', [string]$Id = '', [string]$Type = 'Button', [int]$SettleMs = 900)
    $el = Dialog-Find -Hwnd $Hwnd -Name $Name -Id $Id -Type $Type
    if (-not $el) { "DIALOGINVOKE name='$Name' id='$Id' -> NOT FOUND"; return }
    $p = $null
    if ($el.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { $p.Invoke(); $how = 'Invoke' }
    elseif ($el.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$p)) { $p.Select(); $how = 'Select' }
    else { $how = 'none' }
    Start-Sleep -Milliseconds $SettleMs
    "DIALOGINVOKE name='$Name' id='$Id' via=$how"
}

function Fill-FileDialog {
    # Type a path into a native Open/Save dialog and accept it.
    param([Parameter(Mandatory = $true)][string]$Path, [int]$SettleMs = 1500)
    $d = @(Get-Dialogs | Where-Object { [RvW]::Class($_) -eq '#32770' })
    if (-not $d.Count) { 'FILEDIALOG none open'; return }
    $h = $d[0]
    $title = [RvW]::Title($h)
    # Classic Win32 route: WM_SETTEXT into the file-name edit, then BM_CLICK on
    # the default button (control id 1). No folder contents are enumerated.
    $edit = [RvW]::FileNameEdit($h)
    if ($edit -eq [IntPtr]::Zero) { "FILEDIALOG '$title': no file-name edit found"; return }
    [void][RvW]::SendMessageText($edit, 0x000C, [IntPtr]::Zero, $Path)   # WM_SETTEXT
    Start-Sleep -Milliseconds 300
    $btn = [RvW]::ChildButton($h, 1)
    if ($btn -eq [IntPtr]::Zero) { "FILEDIALOG '$title': no default button"; return }
    $name = [RvW]::Title($btn)
    [void][RvW]::PostMessage($btn, 0x00F5, [IntPtr]::Zero, [IntPtr]::Zero)  # BM_CLICK
    Start-Sleep -Milliseconds $SettleMs
    "FILEDIALOG '$title' path='$Path' accepted via button '$name'"
}

function Close-Dialogs {
    foreach ($h in @(Get-Dialogs)) {
        $t = [RvW]::Title($h)
        [void][RvW]::PostMessage($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
        "DIALOGCLOSE hwnd=$h title='$t'"
    }
    Start-Sleep -Milliseconds 700
}

function Els {
    # List UIA elements, with rectangles converted to WINDOW-relative pixels.
    param([string]$Match = '', [IntPtr]$Hwnd = [IntPtr]::Zero, [int]$Max = 200)
    if ($Hwnd -eq [IntPtr]::Zero) { $Hwnd = Get-UiWin }
    $o = Get-WinOrigin $Hwnd
    $root = Get-UiRoot $Hwnd
    $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    $n = 0
    foreach ($el in $all) {
        try {
            $c = $el.Current; $b = $c.BoundingRectangle
            $type = $c.ControlType.ProgrammaticName -replace '^ControlType\.', ''
            $rs = if ($b.IsEmpty) { 'empty' } else { '{0:0},{1:0} {2:0}x{3:0}' -f ($b.X - $o[0]), ($b.Y - $o[1]), $b.Width, $b.Height }
            $val = ''
            $vp = $null
            if ($el.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$vp)) { $val = " value='" + $vp.Current.Value + "'" }
            $tp = $null
            if ($el.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$tp)) { $val += " toggle=" + $tp.Current.ToggleState }
            $line = '{0} "{1}"{2} en={3} [{4}]' -f $type, $c.Name, $val, $c.IsEnabled, $rs
            if (-not $Match -or $line -match $Match) { $line; $n++; if ($n -ge $Max) { break } }
        } catch { }
    }
    "ELS shown=$n total=$($all.Count)"
}

function Crop {
    # Save a cropped (optionally upscaled) region of an existing screenshot for close reading.
    param([Parameter(Mandatory = $true)][string]$From, [Parameter(Mandatory = $true)][string]$Name, [int]$X, [int]$Y, [int]$W, [int]$H, [double]$Scale = 1.0)
    $src = [System.Drawing.Image]::FromFile((Join-Path $script:RvDir ($From + '.png')))
    $dw = [int]($W * $Scale); $dh = [int]($H * $Scale)
    $dst = New-Object System.Drawing.Bitmap $dw, $dh
    $g = [System.Drawing.Graphics]::FromImage($dst)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.DrawImage($src, (New-Object System.Drawing.Rectangle 0, 0, $dw, $dh), (New-Object System.Drawing.Rectangle $X, $Y, $W, $H), [System.Drawing.GraphicsUnit]::Pixel)
    $path = Join-Path $script:RvDir ($Name + '.png')
    $dst.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $dst.Dispose(); $src.Dispose()
    "CROP $path ${dw}x${dh}"
}

function Stop-Ui {
    # Ask the viewer to close (WM_CLOSE); report whether it exited on its own.
    param([int]$WaitMs = 4000, [switch]$Force)
    $procs = Get-UiProc
    foreach ($p in $procs) {
        foreach ($h in [RvW]::WindowsOf([uint32]$p.Id)) { [void][RvW]::PostMessage($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
    }
    $deadline = (Get-Date).AddMilliseconds($WaitMs)
    while ((Get-Date) -lt $deadline -and (Get-UiProc).Count) { Start-Sleep -Milliseconds 200 }
    $left = Get-UiProc
    if ($left.Count) {
        if ($Force) { $left | Stop-Process -Force; Start-Sleep -Milliseconds 500; "STOP forced: $($left.Id -join ',') (did not exit on WM_CLOSE within ${WaitMs}ms)" }
        else { "STOP pending: still running pid=$($left.Id -join ',') windows:"; Get-AllUiWins }
    } else { "STOP clean: all installed viewer processes exited after WM_CLOSE" }
}

function Get-UiLog {
    $log = Join-Path $env:LOCALAPPDATA 'PDFXI\logs\pdfxi-ui.log'
    if (Test-Path $log) { "LOG $log size=$((Get-Item $log).Length) modified=$((Get-Item $log).LastWriteTime)"; Get-Content $log -Tail 12 } else { "LOG none ($log)" }
    $err = Join-Path $script:RvDir 'ui-stderr.txt'
    if ((Test-Path $err) -and (Get-Item $err).Length) { "STDERR:"; Get-Content $err -Tail 20 } else { 'STDERR empty' }
}
