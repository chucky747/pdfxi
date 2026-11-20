# Regenerates `pdfxi.ico`, the icon embedded by `crates/pdfxi-app/build.rs`.
#
# Artwork is original: the app's red rounded-square document mark with a white
# page glyph. The generator is committed so the icon can be reproduced and
# audited; only `pdfxi.ico` is compiled into the binary. No Adobe asset is used.
#
# Usage: powershell -NoProfile -File make-icon.ps1
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$out = Join-Path $PSScriptRoot 'pdfxi.ico'
$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)

function New-Point([single]$x, [single]$y) {
    return [System.Drawing.PointF]::new($x, $y)
}

function New-RoundedPath([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $d = [single]($r * 2)
    $path.AddArc($x, $y, $d, $d, 180, 90)
    $path.AddArc(($x + $w - $d), $y, $d, $d, 270, 90)
    $path.AddArc(($x + $w - $d), ($y + $h - $d), $d, $d, 0, 90)
    $path.AddArc($x, ($y + $h - $d), $d, $d, 90, 90)
    $path.CloseFigure()
    return $path
}

function New-IconBitmap([int]$size, [int]$ss) {
    $n = [single]($size * $ss)
    $bmp = [System.Drawing.Bitmap]::new($size * $ss, $size * $ss, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    $inset = [single]($n * 0.02)
    $side = [single]($n - 2 * $inset)
    $radius = [single]($side * 0.22)
    $square = New-RoundedPath $inset $inset $side $side $radius
    $red = [System.Drawing.Color]::FromArgb(255, 200, 52, 43)
    $deep = [System.Drawing.Color]::FromArgb(255, 166, 38, 31)
    $brush = [System.Drawing.Drawing2D.LinearGradientBrush]::new((New-Point 0 0), (New-Point $n $n), $red, $deep)
    $g.FillPath($brush, $square)

    $pw = [single]($n * 0.42)
    $ph = [single]($n * 0.52)
    $px = [single](($n - $pw) / 2)
    $py = [single](($n - $ph) / 2)
    $fold = [single]($pw * 0.30)
    $pts = [System.Collections.Generic.List[System.Drawing.PointF]]::new()
    $pts.Add((New-Point $px $py))
    $pts.Add((New-Point ($px + $pw - $fold) $py))
    $pts.Add((New-Point ($px + $pw) ($py + $fold)))
    $pts.Add((New-Point ($px + $pw) ($py + $ph)))
    $pts.Add((New-Point $px ($py + $ph)))
    $page = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $page.AddPolygon($pts.ToArray())
    $white = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::White)
    $g.FillPath($white, $page)

    if ($size -ge 32) {
        $foldPts = [System.Collections.Generic.List[System.Drawing.PointF]]::new()
        $foldPts.Add((New-Point ($px + $pw - $fold) $py))
        $foldPts.Add((New-Point ($px + $pw - $fold) ($py + $fold)))
        $foldPts.Add((New-Point ($px + $pw) ($py + $fold)))
        $foldPath = [System.Drawing.Drawing2D.GraphicsPath]::new()
        $foldPath.AddPolygon($foldPts.ToArray())
        $shade = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 226, 226, 226))
        $g.FillPath($shade, $foldPath)
        $foldPath.Dispose()
        $shade.Dispose()

        $lineBrush = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 200, 52, 43))
        $lh = [single][Math]::Max(1.0, $n * 0.030)
        $lx = [single]($px + $pw * 0.18)
        $lw = [single]($pw * 0.64)
        $g.FillRectangle($lineBrush, $lx, [single]($py + $ph * 0.55), $lw, $lh)
        $g.FillRectangle($lineBrush, $lx, [single]($py + $ph * 0.72), [single]($lw * 0.78), $lh)
        $lineBrush.Dispose()
    }

    $white.Dispose()
    $brush.Dispose()
    $page.Dispose()
    $square.Dispose()
    $g.Dispose()

    if ($ss -eq 1) { return $bmp }
    $final = [System.Drawing.Bitmap]::new($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $fg = [System.Drawing.Graphics]::FromImage($final)
    $fg.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $fg.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $fg.DrawImage($bmp, 0, 0, $size, $size)
    $fg.Dispose()
    $bmp.Dispose()
    return $final
}
# Render every size and serialize as an ICO with 32bpp DIB entries. A DIB entry
# (rather than PNG) is accepted by windres, rc.exe, and Windows Explorer alike.
$images = @()
foreach ($s in $sizes) {
    $ss = if ($s -le 32) { 8 } elseif ($s -le 64) { 4 } else { 2 }
    $bmp = New-IconBitmap $s $ss
    $images += , @{ Size = $s; Bitmap = $bmp }
}

$ms = [System.IO.MemoryStream]::new()
$bw = [System.IO.BinaryWriter]::new($ms)
$bw.Write([uint16]0)
$bw.Write([uint16]1)
$bw.Write([uint16]$images.Count)
$offset = 6 + 16 * $images.Count
$payloads = @()
foreach ($img in $images) {
    $bmp = $img.Bitmap
    $s = $img.Size
    $rect = [System.Drawing.Rectangle]::new(0, 0, $s, $s)
    $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $stride = $data.Stride
    $raw = New-Object byte[] ($stride * $s)
    [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $raw, 0, $raw.Length)
    $bmp.UnlockBits($data)

    $maskStride = [int]([Math]::Floor(($s + 31) / 32) * 4)
    $pixelBytes = $s * $s * 4
    $maskBytes = $maskStride * $s
    $entry = [System.IO.MemoryStream]::new()
    $ew = [System.IO.BinaryWriter]::new($entry)
    $ew.Write([uint32]40)
    $ew.Write([int32]$s)
    $ew.Write([int32]($s * 2))
    $ew.Write([uint16]1)
    $ew.Write([uint16]32)
    $ew.Write([uint32]0)
    $ew.Write([uint32]$pixelBytes)
    $ew.Write([int32]0); $ew.Write([int32]0)
    $ew.Write([uint32]0); $ew.Write([uint32]0)
    for ($y = $s - 1; $y -ge 0; $y--) {
        for ($x = 0; $x -lt $s; $x++) {
            $o = $y * $stride + $x * 4
            $ew.Write($raw[$o + 2]); $ew.Write($raw[$o + 1]); $ew.Write($raw[$o]); $ew.Write($raw[$o + 3])
        }
    }
    $ew.Write((New-Object byte[] $maskBytes))
    $ew.Flush()
    $payloads += , $entry.ToArray()

    $dim = if ($s -ge 256) { 0 } else { $s }
    $bw.Write([byte]$dim)
    $bw.Write([byte]$dim)
    $bw.Write([byte]0)
    $bw.Write([byte]0)
    $bw.Write([uint16]1)
    $bw.Write([uint16]32)
    $bw.Write([uint32]$payloads[-1].Length)
    $bw.Write([uint32]$offset)
    $offset += $payloads[-1].Length
}
foreach ($p in $payloads) { $bw.Write($p) }
$bw.Flush()
[System.IO.File]::WriteAllBytes($out, $ms.ToArray())
$bw.Dispose(); $ms.Dispose()

# PNG preview for review only; not shipped and not embedded.
$preview = $images | Where-Object { $_.Size -eq 256 } | Select-Object -First 1
$preview.Bitmap.Save((Join-Path $PSScriptRoot 'icon-preview-256.png'), [System.Drawing.Imaging.ImageFormat]::Png)
foreach ($img in $images) { $img.Bitmap.Dispose() }

"ICON $out"
"SIZE $((Get-Item $out).Length)"
"SHA256 $((Get-FileHash $out -Algorithm SHA256).Hash.ToLowerInvariant())"