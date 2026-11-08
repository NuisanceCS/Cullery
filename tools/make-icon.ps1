# Makes Cullery's icon files from the artwork (assets\icon-artwork.png, 1024 x 1024):
#   assets\icon.png     the artwork with a soft light rim inside the circle's edge,
#                       for a rounded, glassy look
#   assets\icon-256.png the window icon eframe shows before Windows' own takes over
#   assets\cullery.ico  every size Windows uses (16-256 px), built into cullery.exe
#
#   powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1
param(
    [double]$RimWidth = 26,     # px on the 1024 canvas
    [double]$RimTop = 0.60,     # rim opacity at the top of the circle
    [double]$RimBottom = 0.30   # ... and at the bottom
)

$root = Split-Path $PSScriptRoot
$assets = Join-Path $root 'assets'
Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing @'
using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;

public static class CulleryIcon {
    static byte[] Pixels(Bitmap b, out BitmapData d) {
        d = b.LockBits(new Rectangle(0, 0, b.Width, b.Height), ImageLockMode.ReadWrite, PixelFormat.Format32bppArgb);
        var px = new byte[d.Stride * b.Height];
        Marshal.Copy(d.Scan0, px, 0, px.Length);
        return px;
    }

    // Adds the light rim just inside the circle's dark outline.
    public static Bitmap AddRim(Bitmap src, double width, double top, double bottom) {
        var b = new Bitmap(src.Width, src.Height, PixelFormat.Format32bppArgb);
        using (var g = Graphics.FromImage(b)) g.DrawImage(src, 0, 0, src.Width, src.Height);
        BitmapData d;
        var px = Pixels(b, out d);
        int w = b.Width, h = b.Height, cx = w / 2, cy = h / 2;
        // The circle's outer edge: the last opaque pixel right of center.
        int outer = cx;
        for (int x = w - 1; x > cx; x--) { if (px[cy * d.Stride + x * 4 + 3] > 128) { outer = x - cx; break; } }
        // Its dark outline: walk inward while pixels are dark.
        int ring = outer, limit = Math.Max(0, outer - 40);
        while (ring > limit) {
            int i = cy * d.Stride + (cx + ring) * 4;
            if (Math.Max(px[i], Math.Max(px[i + 1], px[i + 2])) > 80) break;
            ring--;
        }
        if (ring == limit) ring = outer; // artwork that's dark right up to its edge
        double inner = ring + 0.5;
        for (int y = 0; y < h; y++) {
            for (int x = 0; x < w; x++) {
                double dx = x + 0.5 - cx, dy = y + 0.5 - cy;
                double r = Math.Sqrt(dx * dx + dy * dy);
                double t = (inner - r) / width;           // 0 at the outline, 1 at the rim's inner edge
                if (t < -0.02 || t > 1) continue;
                double edge = Math.Min(1, Math.Max(0, (inner - r) + 0.5)); // soft against the outline
                double fade = (1 - Math.Max(0, t)); fade *= fade;
                double up = (1 - dy / r) / 2;               // 1 at the top, 0 at the bottom
                double a = (bottom + (top - bottom) * up) * fade * edge;
                int i = y * d.Stride + x * 4;
                for (int k = 0; k < 3; k++) px[i + k] = (byte)Math.Round(px[i + k] + (255 - px[i + k]) * a);
            }
        }
        Marshal.Copy(px, 0, d.Scan0, px.Length);
        b.UnlockBits(d);
        return b;
    }

    public static Bitmap Resize(Image src, int size) {
        var b = new Bitmap(size, size, PixelFormat.Format32bppArgb);
        using (var g = Graphics.FromImage(b)) {
            g.InterpolationMode = InterpolationMode.HighQualityBicubic;
            g.SmoothingMode = SmoothingMode.HighQuality;
            g.PixelOffsetMode = PixelOffsetMode.HighQuality;
            g.CompositingQuality = CompositingQuality.HighQuality;
            using (var a = new ImageAttributes()) {
                a.SetWrapMode(WrapMode.TileFlipXY); // no dark fringe at the canvas edge
                g.DrawImage(src, new Rectangle(0, 0, size, size), 0, 0, src.Width, src.Height, GraphicsUnit.Pixel, a);
            }
        }
        return b;
    }

    // One size as a classic icon bitmap: 32-bit BGRA rows bottom-up, then a 1-bit
    // mask (set where fully transparent).
    static byte[] Dib(Bitmap b) {
        int s = b.Width;
        BitmapData d;
        var px = Pixels(b, out d);
        b.UnlockBits(d);
        int maskStride = ((s + 31) / 32) * 4;
        using (var ms = new MemoryStream()) using (var f = new BinaryWriter(ms)) {
            f.Write(40); f.Write(s); f.Write(s * 2); f.Write((short)1); f.Write((short)32);
            f.Write(0); f.Write(s * s * 4 + maskStride * s); f.Write(0); f.Write(0); f.Write(0); f.Write(0);
            for (int y = s - 1; y >= 0; y--) f.Write(px, y * d.Stride, s * 4);
            for (int y = s - 1; y >= 0; y--) {
                var row = new byte[maskStride];
                for (int x = 0; x < s; x++) if (px[y * d.Stride + x * 4 + 3] == 0) row[x / 8] |= (byte)(0x80 >> (x % 8));
                f.Write(row);
            }
            return ms.ToArray();
        }
    }

    // A .ico with every size: bitmaps up to 128 px, PNG for 256 (the standard layout).
    public static void WriteIco(Image src, int[] sizes, string path) {
        var pngs = new byte[sizes.Length][];
        for (int n = 0; n < sizes.Length; n++)
            using (var b = Resize(src, sizes[n])) using (var ms = new MemoryStream()) {
                if (sizes[n] >= 256) { b.Save(ms, ImageFormat.Png); pngs[n] = ms.ToArray(); } else pngs[n] = Dib(b);
            }
        using (var f = new BinaryWriter(File.Create(path))) {
            f.Write((short)0); f.Write((short)1); f.Write((short)sizes.Length);
            int offset = 6 + 16 * sizes.Length;
            for (int n = 0; n < sizes.Length; n++) {
                f.Write((byte)(sizes[n] >= 256 ? 0 : sizes[n])); f.Write((byte)(sizes[n] >= 256 ? 0 : sizes[n]));
                f.Write((byte)0); f.Write((byte)0); f.Write((short)1); f.Write((short)32);
                f.Write(pngs[n].Length); f.Write(offset);
                offset += pngs[n].Length;
            }
            foreach (var p in pngs) f.Write(p);
        }
    }
}
'@

$art = [System.Drawing.Bitmap]::FromFile((Join-Path $assets 'icon-artwork.png'))
$icon = [CulleryIcon]::AddRim($art, $RimWidth, $RimTop, $RimBottom)
$art.Dispose()
$icon.Save((Join-Path $assets 'icon.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$b256 = [CulleryIcon]::Resize($icon, 256)
$b256.Save((Join-Path $assets 'icon-256.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$b256.Dispose()
[CulleryIcon]::WriteIco($icon, @(16, 20, 24, 32, 40, 48, 64, 96, 128, 256), (Join-Path $assets 'cullery.ico'))
$icon.Dispose()
Get-ChildItem $assets | Select-Object Name, Length
