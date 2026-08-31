# desktop_forensics.ps1 — WHY is the desktop black? One session-1 run that
# separates "nothing is drawn" from "the capture path is broken", with a
# POSITIVE CONTROL first.
#
# desktop_paint_capture.ps1 reports "Progman prints black" and its own doc reads
# that as "the window produces no content". That inference has never had a
# control: if the CAPTURE path is broken, EVERY window prints black and the
# conclusion is empty. So this paints a rectangle whose colour we choose onto
# the screen DC and reads it back before believing anything else.
#
#   1 CANARY round trip  — FillRect magenta on the screen DC, BitBlt it back.
#                          black  => the screen DC path is broken; stop.
#                          magenta=> capture works; a black screen IS black.
#   2 fullscreen BitBlt  — the composed primary, real nonzero count.
#   3 Progman/WorkerW/Shell_TrayWnd PrintWindow.
#   4 window census with DWMWA_CLOAKED — a cloaked shell is black for reasons
#     that have nothing to do with the GPU, and nothing has ever checked it.
#
# ⚠ MUST run in SESSION 1 (scheduled task, InteractiveToken). win_exec is
#   session 0: no compositor, no desktop, and a created window has no HWND.
# ⚠ NO WinForms window: a form shown without a message pump makes PrintWindow
#   block, which hung the first version of this script with zero output.
# ⚠ NO Bitmap.GetPixel loops: 40k calls is minutes. LockBits + one Marshal.Copy.
#
# Output: Z:\tmp\forensics\*.png + C:\ProgramData\Helios\desktop_forensics.txt
$ErrorActionPreference = 'Continue'
$log = 'C:\ProgramData\Helios\desktop_forensics.txt'
$out = 'Z:\tmp\forensics'
$sb = [System.Text.StringBuilder]::new()
function W([string]$s) { [void]$sb.AppendLine($s) }
function Flush { Set-Content -Path $log -Value $sb.ToString() -Encoding UTF8 }
W "=== desktop_forensics $(Get-Date -Format o) ==="
if (-not (Test-Path $out)) { New-Item -ItemType Directory -Path $out -Force | Out-Null }
Flush

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public class F {
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lp);
  [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr hdc);
  [DllImport("user32.dll")] public static extern int FillRect(IntPtr hdc, ref RECT r, IntPtr br);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateSolidBrush(int c);
  [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr o);
  [DllImport("gdi32.dll")] public static extern bool BitBlt(IntPtr d,int dx,int dy,int w,int h,IntPtr s,int sx,int sy,int rop);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int sz);
  [DllImport("dwmapi.dll")] public static extern int DwmIsCompositionEnabled(out bool e);
  [DllImport("dwmapi.dll")] public static extern int DwmFlush();
  public delegate bool EnumProc(IntPtr h, IntPtr lp);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L,T,R,B; }
}
"@

# LockBits + one Marshal.Copy. GetPixel over a full screen takes minutes.
function Stats([System.Drawing.Bitmap]$b) {
  $rect = New-Object System.Drawing.Rectangle 0, 0, $b.Width, $b.Height
  $d = $b.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
                   [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $bytes = New-Object byte[] ($d.Stride * $b.Height)
  [System.Runtime.InteropServices.Marshal]::Copy($d.Scan0, $bytes, 0, $bytes.Length)
  $b.UnlockBits($d)
  $nz = 0; $first = ''
  for ($i = 0; $i -lt $bytes.Length; $i += 4) {
    if ($bytes[$i] -ne 0 -or $bytes[$i+1] -ne 0 -or $bytes[$i+2] -ne 0) {
      $nz++
      if (-not $first) {
        $px = $i / 4
        $first = ("first@({0},{1})=#{2:X2}{3:X2}{4:X2}" -f ($px % $b.Width), [int]($px / $b.Width),
                  $bytes[$i+2], $bytes[$i+1], $bytes[$i])
      }
    }
  }
  return @{ nz = $nz; n = ($bytes.Length / 4); first = $first }
}

function Grab([int]$x, [int]$y, [int]$w, [int]$h, [string]$name) {
  $screen = [F]::GetDC([IntPtr]::Zero)
  $b = New-Object System.Drawing.Bitmap($w, $h)
  $g = [System.Drawing.Graphics]::FromImage($b)
  $hdc = $g.GetHdc()
  $ok = [F]::BitBlt($hdc, 0, 0, $w, $h, $screen, $x, $y, 0x00CC0020)
  $g.ReleaseHdc($hdc); $g.Dispose()
  [void][F]::ReleaseDC([IntPtr]::Zero, $screen)
  $b.Save("$out\$name.png")
  $s = Stats $b
  $b.Dispose()
  return @{ ok = $ok; s = $s }
}

$comp = $false; [void][F]::DwmIsCompositionEnabled([ref]$comp)
W "DwmIsCompositionEnabled = $comp"
Flush

# ── 1  the canary: paint a colour we chose, read it straight back ───────────
$screen = [F]::GetDC([IntPtr]::Zero)
$brush = [F]::CreateSolidBrush(0x00FF00FF)          # COLORREF 0x00BBGGRR = magenta
$r = New-Object F+RECT; $r.L = 120; $r.T = 120; $r.R = 520; $r.B = 420
W "1a screen DC = 0x$('{0:x}' -f $screen.ToInt64())"; Flush
$fr = [F]::FillRect($screen, [ref]$r, $brush)
W "1b FillRect returned $fr"; Flush
[void][F]::DeleteObject($brush)
[void][F]::ReleaseDC([IntPtr]::Zero, $screen)
W "1c DC released"; Flush
# ⛔ DwmFlush BLOCKS until the next composition. If DWM is not completing
# frames it never returns, which hung the previous version here with the log
# stopping at exactly this point. Timed, and its result is a finding either way.
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$dwmr = [F]::DwmFlush()
$sw.Stop()
W "1d DwmFlush hr=0x$('{0:x8}' -f $dwmr) in $($sw.ElapsedMilliseconds) ms"; Flush
Start-Sleep -Milliseconds 500
W "1e grabbing canary rect"; Flush
$c = Grab 120 120 400 300 "1_canary_roundtrip"
W "1 CANARY FillRect->BitBlt  fillrect=$fr blt=$($c.ok) nonzero=$($c.s.nz)/$($c.s.n) $($c.s.first)"
Flush

# ── 2  the whole composed primary ───────────────────────────────────────────
$vw = [System.Windows.Forms.SystemInformation]::VirtualScreen 2>$null
if (-not $vw) { Add-Type -AssemblyName System.Windows.Forms; $vw = [System.Windows.Forms.SystemInformation]::VirtualScreen }
W "VirtualScreen = $vw"
W "2a grabbing fullscreen $($vw.Width)x$($vw.Height)"; Flush
$f = Grab $vw.X $vw.Y $vw.Width $vw.Height "2_fullscreen"
W "2 fullscreen BitBlt        blt=$($f.ok) nonzero=$($f.s.nz)/$($f.s.n) $($f.s.first)"
Flush

# ── 4  census first: PrintWindow needs the handles ──────────────────────────
$script:wins = @()
$cb = [F+EnumProc]{ param($h, $lp)
  if (-not [F]::IsWindowVisible($h)) { return $true }
  $cls = [System.Text.StringBuilder]::new(256); [void][F]::GetClassName($h, $cls, 256)
  $txt = [System.Text.StringBuilder]::new(256); [void][F]::GetWindowTextW($h, $txt, 256)
  $rr = New-Object F+RECT; [void][F]::GetWindowRect($h, [ref]$rr)
  $ck = 0; [void][F]::DwmGetWindowAttribute($h, 14, [ref]$ck, 4)   # DWMWA_CLOAKED
  $script:wins += [pscustomobject]@{ h=$h; cls=$cls.ToString(); txt=$txt.ToString()
                                     w=($rr.R-$rr.L); ht=($rr.B-$rr.T); x=$rr.L; y=$rr.T; cloaked=$ck }
  return $true }
[void][F]::EnumWindows($cb, [IntPtr]::Zero)
W "--- visible top-level windows: $($script:wins.Count) ---"
foreach ($x in $script:wins) {
  W ("  0x{0:x8} {1,-26} {2,5}x{3,-5} @{4},{5} cloaked={6} `"{7}`"" -f `
      $x.h.ToInt64(), $x.cls, $x.w, $x.ht, $x.x, $x.y, $x.cloaked, $x.txt)
}
Flush

# ── 3  the desktop's own windows ────────────────────────────────────────────
foreach ($want in @('Progman','WorkerW','Shell_TrayWnd')) {
  $t = $script:wins | Where-Object { $_.cls -eq $want -and $_.w -gt 0 -and $_.ht -gt 0 } | Select-Object -First 1
  if (-not $t) { W "3 $want : no visible window"; continue }
  $b = New-Object System.Drawing.Bitmap($t.w, $t.ht)
  $g = [System.Drawing.Graphics]::FromImage($b)
  $hdc = $g.GetHdc()
  $o = [F]::PrintWindow($t.h, $hdc, 2)
  $gle = [System.Runtime.InteropServices.Marshal]::GetLastWin32Error()
  $g.ReleaseHdc($hdc); $g.Dispose()
  $b.Save("$out\3_$want.png")
  $ss = Stats $b
  W "3 $want PrintWindow ok=$o gle=$gle nonzero=$($ss.nz)/$($ss.n) $($ss.first)"
  $b.Dispose()
  Flush
}

W "=== end $(Get-Date -Format o) ==="
Flush
