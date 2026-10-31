Add-Type -AssemblyName System.Drawing

# 相对定位：脚本位于 scripts/，图标目录在上一级的 apps/desktop/src-tauri/icons。
$root = Split-Path -Parent $PSScriptRoot
$iconDir = Join-Path $root 'apps\desktop\src-tauri\icons'
New-Item -ItemType Directory -Force -Path $iconDir | Out-Null

function New-BaseBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    $pad = [Math]::Max(1, [int]($size * 0.04))
    $rect = New-Object System.Drawing.Rectangle($pad, $pad, ($size - 2 * $pad), ($size - 2 * $pad))
    $radius = [Math]::Max(2, [int]($size * 0.22))
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $radius * 2
    $path.AddArc($rect.X, $rect.Y, $d, $d, 180, 90)
    $path.AddArc($rect.Right - $d, $rect.Y, $d, $d, 270, 90)
    $path.AddArc($rect.Right - $d, $rect.Bottom - $d, $d, $d, 0, 90)
    $path.AddArc($rect.X, $rect.Bottom - $d, $d, $d, 90, 90)
    $path.CloseFigure()

    $bg = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 18, 22, 31))
    $g.FillPath($bg, $path)
    $border = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(255, 44, 54, 70), [Math]::Max(1, $size / 96))
    $g.DrawPath($border, $path)

    # 播放三角 + 游戏方块：动画与游戏两条主线的抽象
    $accent = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 226, 87, 76))
    $triangle = @(
        (New-Object System.Drawing.PointF([float]($size * 0.34), [float]($size * 0.28))),
        (New-Object System.Drawing.PointF([float]($size * 0.34), [float]($size * 0.72))),
        (New-Object System.Drawing.PointF([float]($size * 0.72), [float]($size * 0.50)))
    )
    $g.FillPolygon($accent, $triangle)

    $g.Dispose()
    return $bmp
}

$sizes = @(16, 24, 32, 48, 64, 128, 256)
$pngs = @{}
foreach ($size in $sizes) {
    $bmp = New-BaseBitmap $size
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs[$size] = $ms.ToArray()
    $ms.Dispose()
    $bmp.Dispose()
}

$pngs[32] | Set-Content -Path (Join-Path $iconDir "32x32.png") -AsByteStream
$pngs[128] | Set-Content -Path (Join-Path $iconDir "128x128.png") -AsByteStream
$pngs[256] | Set-Content -Path (Join-Path $iconDir "128x128@2x.png") -AsByteStream
$pngs[256] | Set-Content -Path (Join-Path $iconDir "icon.png") -AsByteStream

# 组装多尺寸 ICO（每帧为 PNG 压缩数据，Vista 起支持）
$ordered = @(16, 24, 32, 48, 64, 128, 256)
$headerSize = 6
$entrySize = 16
$offset = $headerSize + $entrySize * $ordered.Count
$ico = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter($ico)
$writer.Write([UInt16]0)
$writer.Write([UInt16]1)
$writer.Write([UInt16]$ordered.Count)
foreach ($size in $ordered) {
    $data = $pngs[$size]
    $dim = if ($size -ge 256) { 0 } else { $size }
    $writer.Write([Byte]$dim)
    $writer.Write([Byte]$dim)
    $writer.Write([Byte]0)
    $writer.Write([Byte]0)
    $writer.Write([UInt16]1)
    $writer.Write([UInt16]32)
    $writer.Write([UInt32]$data.Length)
    $writer.Write([UInt32]$offset)
    $offset += $data.Length
}
foreach ($size in $ordered) {
    $writer.Write($pngs[$size])
}
$writer.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $iconDir "icon.ico"), $ico.ToArray())
$writer.Dispose()
$ico.Dispose()

Get-ChildItem $iconDir | Select-Object Name, Length
