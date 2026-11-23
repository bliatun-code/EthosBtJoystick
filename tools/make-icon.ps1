# Draws the program icon (the blue rounded square with the white gimbal ring
# and stick) and writes it as a Windows .ico file with several sizes. Every
# size is drawn on its own, with smooth edges, instead of scaling one picture.
#
# The result, app\assets\EthosBtJoystick.ico, is committed. It is the exe's
# icon (app\build.rs), the window and tray icon (app\src\icon.rs, which needs
# the 32 and 64 pixel sizes) and the installer's icon. Run this script again
# only to change the picture.
#
#   powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1
param(
    [string]$Out = '',
    [int[]]$Sizes = @(16, 20, 24, 32, 40, 48, 64, 256)
)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if (-not $Out) { $Out = Join-Path $root 'app\assets\EthosBtJoystick.ico' }
$Out = [System.IO.Path]::GetFullPath($Out)
[void][System.IO.Directory]::CreateDirectory((Split-Path -Parent $Out))

Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;

public static class EthosIcon
{
    // The shapes, on a 32 unit square with the origin in the middle.
    const double Half = 10.0, Corner = 5.5;                  // rounded square
    const double RingRadius = 9.5, RingHalfWidth = 1.2;      // gimbal ring
    const double KnobX = 3.0, KnobY = -3.0, KnobRadius = 4.2; // stick
    const int Blue = 0x3B8EEA;
    const int Samples = 8;                                   // per pixel and axis

    // Straight (not premultiplied) BGRA, top row first.
    public static byte[] Render(int size)
    {
        byte[] bgra = new byte[size * size * 4];
        double unit = 32.0 / size;
        // keep the ring at least about 1.7 pixels wide, or it fades away at 16 and 20 pixels
        double ringHalfWidth = Math.Max(RingHalfWidth, 0.85 * unit);
        for (int y = 0; y < size; y++)
        for (int x = 0; x < size; x++)
        {
            int inside = 0, white = 0;
            for (int sy = 0; sy < Samples; sy++)
            for (int sx = 0; sx < Samples; sx++)
            {
                double fx = (x + (sx + 0.5) / Samples) * unit - 16.0;
                double fy = (y + (sy + 0.5) / Samples) * unit - 16.0;
                double dx = Math.Max(Math.Abs(fx) - Half, 0.0);
                double dy = Math.Max(Math.Abs(fy) - Half, 0.0);
                if (Math.Sqrt(dx * dx + dy * dy) > Corner) continue;
                inside++;

                bool ring = Math.Abs(Math.Sqrt(fx * fx + fy * fy) - RingRadius) <= ringHalfWidth;
                double kx = fx - KnobX, ky = fy - KnobY;
                bool knob = Math.Sqrt(kx * kx + ky * ky) <= KnobRadius;
                if (ring || knob) white++;
            }
            if (inside == 0) continue;

            double w = (double)white / inside;
            int i = (y * size + x) * 4;
            bgra[i] = Mix(Blue & 0xFF, w);
            bgra[i + 1] = Mix((Blue >> 8) & 0xFF, w);
            bgra[i + 2] = Mix((Blue >> 16) & 0xFF, w);
            bgra[i + 3] = (byte)Math.Round(255.0 * inside / (Samples * Samples));
        }
        return bgra;
    }

    static byte Mix(int blue, double white)
    {
        return (byte)Math.Round(blue + (255 - blue) * white);
    }

    static byte[] Png(int size)
    {
        byte[] bgra = Render(size);
        using (Bitmap bitmap = new Bitmap(size, size, PixelFormat.Format32bppArgb))
        {
            BitmapData data = bitmap.LockBits(new Rectangle(0, 0, size, size), ImageLockMode.WriteOnly, PixelFormat.Format32bppArgb);
            Marshal.Copy(bgra, 0, data.Scan0, bgra.Length);
            bitmap.UnlockBits(data);
            using (MemoryStream stream = new MemoryStream())
            {
                bitmap.Save(stream, ImageFormat.Png);
                return stream.ToArray();
            }
        }
    }

    // The classic icon picture: a bitmap header, the pixels bottom row first,
    // then a 1 bit mask that marks the fully transparent pixels.
    static byte[] Dib(int size)
    {
        byte[] bgra = Render(size);
        int maskStride = ((size + 31) / 32) * 4;
        using (MemoryStream stream = new MemoryStream())
        using (BinaryWriter w = new BinaryWriter(stream))
        {
            w.Write(40); w.Write(size); w.Write(size * 2); w.Write((short)1); w.Write((short)32);
            w.Write(0); w.Write(size * size * 4 + maskStride * size);
            w.Write(0); w.Write(0); w.Write(0); w.Write(0);
            for (int y = size - 1; y >= 0; y--) w.Write(bgra, y * size * 4, size * 4);
            for (int y = size - 1; y >= 0; y--)
            {
                byte[] row = new byte[maskStride];
                for (int x = 0; x < size; x++)
                    if (bgra[(y * size + x) * 4 + 3] == 0) row[x / 8] |= (byte)(0x80 >> (x % 8));
                w.Write(row);
            }
            w.Flush();
            return stream.ToArray();
        }
    }

    // 256 pixels is stored as PNG (as Windows expects), smaller sizes as bitmaps.
    public static void WriteIco(string path, int[] sizes)
    {
        List<byte[]> images = new List<byte[]>();
        foreach (int size in sizes) images.Add(size >= 256 ? Png(size) : Dib(size));

        using (FileStream file = File.Create(path))
        using (BinaryWriter w = new BinaryWriter(file))
        {
            w.Write((short)0); w.Write((short)1); w.Write((short)sizes.Length);
            int offset = 6 + 16 * sizes.Length;
            for (int i = 0; i < sizes.Length; i++)
            {
                byte dimension = (byte)(sizes[i] >= 256 ? 0 : sizes[i]);
                w.Write(dimension); w.Write(dimension); w.Write((byte)0); w.Write((byte)0);
                w.Write((short)1); w.Write((short)32);
                w.Write(images[i].Length); w.Write(offset);
                offset += images[i].Length;
            }
            foreach (byte[] image in images) w.Write(image);
        }
    }
}
'@

foreach ($size in $Sizes) {
    if ($size -lt 16 -or $size -gt 256) { throw "Icon size $size is outside 16..256" }
}
[EthosIcon]::WriteIco($Out, ($Sizes | Sort-Object))
"Wrote $Out ($((Get-Item $Out).Length) bytes, sizes: $(($Sizes | Sort-Object) -join ', '))"
