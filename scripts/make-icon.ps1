<#
.SYNOPSIS
  Draws assets\tile-rs.ico (and tile-rs.png) - the same three-tile spiral as the tray icon.
#>
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Set-Location (Split-Path -Parent $PSScriptRoot)
New-Item -ItemType Directory -Force assets | Out-Null

function New-TileBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)
    $s = $size / 32.0
    $tiles = @(
        @{ x = 2.0;  y = 2.0;  w = 13.0; h = 28.0; shade = 1.00 },
        @{ x = 17.0; y = 2.0;  w = 13.0; h = 13.0; shade = 0.85 },
        @{ x = 17.0; y = 17.0; w = 13.0; h = 13.0; shade = 0.65 }
    )
    foreach ($t in $tiles) {
        $c = [System.Drawing.Color]::FromArgb(255, [int](0x8d * $t.shade), [int](0xbc * $t.shade), [int](0xff * $t.shade))
        $br = New-Object System.Drawing.SolidBrush $c
        $rad = 3.5 * $s
        $rect = New-Object System.Drawing.RectangleF ($t.x * $s), ($t.y * $s), ($t.w * $s), ($t.h * $s)
        $path = New-Object System.Drawing.Drawing2D.GraphicsPath
        $path.AddArc($rect.X, $rect.Y, $rad, $rad, 180, 90)
        $path.AddArc($rect.Right - $rad, $rect.Y, $rad, $rad, 270, 90)
        $path.AddArc($rect.Right - $rad, $rect.Bottom - $rad, $rad, $rad, 0, 90)
        $path.AddArc($rect.X, $rect.Bottom - $rad, $rad, $rad, 90, 90)
        $path.CloseFigure()
        $g.FillPath($br, $path)
        $br.Dispose()
        $path.Dispose()
    }
    $g.Dispose()
    return $bmp
}

$sizes = @(16, 32, 48, 256)
$pngs = @{}
foreach ($s in $sizes) {
    $bmp = New-TileBitmap $s
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs[$s] = $ms.ToArray()
    $ms.Dispose()
    $bmp.Dispose()
}
[IO.File]::WriteAllBytes((Join-Path (Get-Location) 'assets\tile-rs.png'), $pngs[256])

$ms = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter($ms)
$bw.Write([UInt16]0); $bw.Write([UInt16]1); $bw.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
foreach ($s in $sizes) {
    $dim = if ($s -ge 256) { 0 } else { $s }
    $bw.Write([Byte]$dim); $bw.Write([Byte]$dim); $bw.Write([Byte]0); $bw.Write([Byte]0)
    $bw.Write([UInt16]1); $bw.Write([UInt16]32)
    $bw.Write([UInt32]$pngs[$s].Length); $bw.Write([UInt32]$offset)
    $offset += $pngs[$s].Length
}
foreach ($s in $sizes) { $bw.Write($pngs[$s]) }
$bw.Flush()
[IO.File]::WriteAllBytes((Join-Path (Get-Location) 'assets\tile-rs.ico'), $ms.ToArray())
$bw.Dispose()
$ms.Dispose()
Write-Host 'assets\tile-rs.ico and assets\tile-rs.png written' -ForegroundColor Green
