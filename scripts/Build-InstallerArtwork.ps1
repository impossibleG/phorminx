[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$repository = Split-Path -Parent $PSScriptRoot
$icon = Join-Path $repository 'design\brand\png\app\phorminx-app-256.png'
$destinationDirectory = Join-Path $repository 'design\brand\png\installer'
[IO.Directory]::CreateDirectory($destinationDirectory) | Out-Null
$destination = Join-Path $destinationDirectory 'phorminx-wizard-600x1149.png'

$bitmap = [Drawing.Bitmap]::new(600, 1149)
$graphics = [Drawing.Graphics]::FromImage($bitmap)
$mark = $null
$titleFont = $null
$taglineFont = $null
$mutedBrush = $null
$titleBrush = $null
$goldBrush = $null
$rulePen = $null
try {
    $graphics.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.TextRenderingHint = [Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $graphics.Clear([Drawing.Color]::FromArgb(18, 22, 24))
    $goldBrush = [Drawing.SolidBrush]::new([Drawing.Color]::FromArgb(198, 151, 103))
    $titleBrush = [Drawing.SolidBrush]::new([Drawing.Color]::FromArgb(241, 238, 231))
    $mutedBrush = [Drawing.SolidBrush]::new([Drawing.Color]::FromArgb(161, 167, 168))
    $rulePen = [Drawing.Pen]::new([Drawing.Color]::FromArgb(70, 78, 80), 2)
    $graphics.FillRectangle($goldBrush, 58, 72, 5, 1004)
    $graphics.DrawLine($rulePen, 105, 73, 535, 73)
    $graphics.DrawLine($rulePen, 105, 1076, 535, 1076)
    $mark = [Drawing.Image]::FromFile($icon)
    $graphics.DrawImage($mark, [Drawing.Rectangle]::new(205, 320, 190, 190))
    $titleFont = [Drawing.Font]::new('Segoe UI', 47, [Drawing.FontStyle]::Regular, [Drawing.GraphicsUnit]::Pixel)
    $taglineFont = [Drawing.Font]::new('Segoe UI', 24, [Drawing.FontStyle]::Regular, [Drawing.GraphicsUnit]::Pixel)
    $center = [Drawing.StringFormat]::new()
    $center.Alignment = [Drawing.StringAlignment]::Center
    try {
        $graphics.DrawString('PHORMINX', $titleFont, $titleBrush, [Drawing.RectangleF]::new(92, 554, 440, 68), $center)
        $graphics.DrawLine($rulePen, 180, 655, 420, 655)
        $graphics.DrawString('VOICE, DISCIPLINED.', $taglineFont, $mutedBrush, [Drawing.RectangleF]::new(95, 688, 440, 42), $center)
    } finally { $center.Dispose() }
    $bitmap.Save($destination, [Drawing.Imaging.ImageFormat]::Png)
} finally {
    if ($null -ne $mark) { $mark.Dispose() }
    if ($null -ne $titleFont) { $titleFont.Dispose() }
    if ($null -ne $taglineFont) { $taglineFont.Dispose() }
    if ($null -ne $mutedBrush) { $mutedBrush.Dispose() }
    if ($null -ne $titleBrush) { $titleBrush.Dispose() }
    if ($null -ne $goldBrush) { $goldBrush.Dispose() }
    if ($null -ne $rulePen) { $rulePen.Dispose() }
    $graphics.Dispose()
    $bitmap.Dispose()
}
Write-Output $destination
