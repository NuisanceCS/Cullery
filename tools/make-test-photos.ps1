# Creates a folder of fake camera shots for trying Cullery without an SD card:
# full-size (6000x4000) JPGs plus dummy .CR3 files with matching names.
# A few shots are JPG-only or RAW-only to test pairing.
#   powershell -ExecutionPolicy Bypass -File tools\make-test-photos.ps1 [-Count 40] [-Out test-photos]
param(
    [int]$Count = 40,
    [string]$Out = (Join-Path $PSScriptRoot '..\test-photos')
)

Add-Type -AssemblyName System.Drawing
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path

$codec = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq 'image/jpeg' }
$params = New-Object System.Drawing.Imaging.EncoderParameters 1
$params.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter ([System.Drawing.Imaging.Encoder]::Quality), 88L
$rand = New-Object System.Random 7
$rawBytes = New-Object byte[] (2MB)

for ($i = 0; $i -lt $Count; $i++) {
    $n = 4821 + $i
    $name = "IMG_$n"
    $rawOnly = ($i % 13 -eq 6)
    $jpgOnly = ($i % 17 -eq 11)
    $portrait = ($i % 9 -eq 4)

    if (-not $rawOnly) {
        $w = 6000; $h = 4000
        if ($portrait) { $w = 4000; $h = 6000 }
        $bmp = New-Object System.Drawing.Bitmap $w, $h
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $hue = ($i * 37) % 360
        $c1 = [System.Drawing.Color]::FromArgb(255, (80 + ($hue % 120)), (60 + (($hue * 2) % 110)), (70 + (($hue * 3) % 120)))
        $c2 = [System.Drawing.Color]::FromArgb(255, 20, 22, 28)
        $rect = New-Object System.Drawing.Rectangle 0, 0, $w, $h
        $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, $c1, $c2, 60.0
        $g.FillRectangle($brush, $rect)
        for ($k = 0; $k -lt 12; $k++) {
            $sz = $rand.Next(300, 1400)
            $cb = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(70, $rand.Next(255), $rand.Next(255), $rand.Next(255)))
            $g.FillEllipse($cb, $rand.Next($w), $rand.Next($h), $sz, $sz)
            $cb.Dispose()
        }
        $font = New-Object System.Drawing.Font 'Segoe UI', 420, ([System.Drawing.FontStyle]::Bold)
        $g.DrawString("$n", $font, [System.Drawing.Brushes]::White, 300, 300)
        $g.Dispose(); $brush.Dispose(); $font.Dispose()
        $bmp.Save((Join-Path $Out "$name.JPG"), $codec, $params)
        $bmp.Dispose()
    }
    if (-not $jpgOnly) {
        $rand.NextBytes($rawBytes)
        [System.IO.File]::WriteAllBytes((Join-Path $Out "$name.CR3"), $rawBytes)
    }
}
Write-Output "Made $Count shots in $Out"
