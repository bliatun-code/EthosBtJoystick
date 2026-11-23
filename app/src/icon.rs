//! The program icon.
//!
//! The picture lives in `assets/EthosBtJoystick.ico` (drawn by
//! `tools/make-icon.ps1`). The same file is the exe's icon resource (see
//! `build.rs`) and the installer's icon; here it supplies the window and tray
//! icons, so that all of them show the same picture.

const ICO: &[u8] = include_bytes!("../assets/EthosBtJoystick.ico");

/// One size from the icon file as straight-alpha RGBA, top row first.
/// `None` if the file has no bitmap of that size.
pub fn rgba(size: u32) -> Option<Vec<u8>> {
    parse(ICO, size)
}

fn parse(ico: &[u8], size: u32) -> Option<Vec<u8>> {
    let u16_at = |i: usize| Some(u16::from_le_bytes(ico.get(i..i + 2)?.try_into().ok()?));
    let u32_at = |i: usize| Some(u32::from_le_bytes(ico.get(i..i + 4)?.try_into().ok()?));

    let count = u16_at(4)? as usize;
    for entry in (0..count).map(|i| 6 + 16 * i) {
        // a width of 0 in the directory means 256
        let width = match *ico.get(entry)? {
            0 => 256,
            width => width as u32,
        };
        if width != size {
            continue;
        }
        // Only the classic entries: a 40 byte bitmap header and 32 bit pixels.
        // (The 256 pixel picture is stored as a PNG.)
        let offset = u32_at(entry + 12)? as usize;
        if u32_at(offset)? != 40 || u16_at(offset + 14)? != 32 {
            return None;
        }
        let n = size as usize;
        let pixels = ico.get(offset + 40..offset + 40 + n * n * 4)?;

        // stored bottom row first, as BGRA
        let mut rgba = Vec::with_capacity(n * n * 4);
        for row in (0..n).rev() {
            for pixel in pixels[row * n * 4..(row + 1) * n * 4].chunks_exact(4) {
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
            }
        }
        return Some(rgba);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLUE: [u8; 4] = [0x3B, 0x8E, 0xEA, 0xFF];
    const WHITE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

    fn pixel(rgba: &[u8], size: usize, x: usize, y: usize) -> [u8; 4] {
        rgba[(y * size + x) * 4..(y * size + x) * 4 + 4].try_into().unwrap()
    }

    /// The sizes the program asks for exist, are the right way up and have
    /// the colours in the right order.
    #[test]
    fn window_and_tray_sizes() {
        for size in [32usize, 64] {
            let rgba = rgba(size as u32).unwrap_or_else(|| panic!("no {size} pixel bitmap in the icon file"));
            assert_eq!(rgba.len(), size * size * 4);

            // the corners are transparent
            assert_eq!(pixel(&rgba, size, 0, 0)[3], 0);
            // the stick is up and to the right of the centre, and the opposite
            // side is plain blue - so the picture is not mirrored or upside down
            assert_eq!(pixel(&rgba, size, size * 19 / 32, size * 13 / 32), WHITE);
            assert_eq!(pixel(&rgba, size, size * 13 / 32, size * 19 / 32), BLUE);
        }
    }

    #[test]
    fn sizes_that_cannot_be_used() {
        assert!(rgba(256).is_none(), "the 256 pixel picture is a PNG");
        assert!(rgba(33).is_none());
        assert!(parse(&[], 32).is_none());
        assert!(parse(&ICO[..200], 32).is_none(), "truncated file");
    }
}
