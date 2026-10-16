Add-Type -AssemblyName System.Drawing

$size = [float]256
$bmp = New-Object System.Drawing.Bitmap 256, 256
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)

# rounded square with vertical gradient
$pad = [float]8
$w = [float]240
$h = [float]240
$r = [float]56
$p = New-Object System.Drawing.Drawing2D.GraphicsPath
$p.AddArc($pad, $pad, $r, $r, 180, 90)
$p.AddArc($pad + $w - $r, $pad, $r, $r, 270, 90)
$p.AddArc($pad + $w - $r, $pad + $h - $r, $r, $r, 0, 90)
$p.AddArc($pad, $pad + $h - $r, $r, $r, 90, 90)
$p.CloseFigure()

$c1 = [System.Drawing.Color]::FromArgb(255, 138, 180, 248)
$c2 = [System.Drawing.Color]::FromArgb(255, 26, 115, 232)
$rect = New-Object System.Drawing.Rectangle(8, 8, 240, 240)
$grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush($rect, $c1, $c2, 90)
$g.FillPath($grad, $p)

# globe
$cx = [float]128; $cy = [float]128; $R = [float]82
$clipPath = New-Object System.Drawing.Drawing2D.GraphicsPath
$clipPath.AddEllipse(($cx - $R), ($cy - $R), (2 * $R), (2 * $R))
$g.SetClip($clipPath)
$pen = New-Object System.Drawing.Pen([System.Drawing.Color]::White, [float]8)
$g.DrawEllipse($pen, ($cx - $R), ($cy - $R), (2 * $R), (2 * $R))
$mw = 73.8
$g.DrawEllipse($pen, ($cx - $mw/2), ($cy - $R), $mw, (2 * $R))
$g.DrawLine($pen, ($cx - $R), $cy, ($cx + $R), $cy)
$g.DrawArc($pen, ($cx - $R), ($cy - 41), (2 * $R), 82, 0, 180)
$g.ResetClip()

$ms = New-Object System.IO.MemoryStream
$bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
$png = $ms.ToArray()

# ICO container: ICONDIR + one ICONDIRENTRY + PNG payload
$ico = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter($ico)
$bw.Write([UInt16]0)
$bw.Write([UInt16]1)
$bw.Write([UInt16]1)
$bw.Write([Byte]0)
$bw.Write([Byte]0)
$bw.Write([Byte]0)
$bw.Write([Byte]0)
$bw.Write([UInt16]1)
$bw.Write([UInt16]32)
$bw.Write([UInt32]$png.Length)
$bw.Write([UInt32]22)
$bw.Write($png)
$bw.Flush()

$dir = "D:\NanoBrowser\src-tauri\icons"
[System.IO.File]::WriteAllBytes("$dir\icon.ico", $ico.ToArray())
[System.IO.File]::WriteAllBytes("$dir\icon.png", $png)

$g.Dispose(); $bmp.Dispose(); $bw.Dispose()
Write-Host "icon.ico written ok"
