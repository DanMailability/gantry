//! Default dark color theme and xterm 256-color resolution.

use crate::snapshot::Rgb;

/// Terminal colors used when the program does not override them.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Default text color.
    pub foreground: Rgb,
    /// Default background color.
    pub background: Rgb,
    /// Cursor color (light-blue accent).
    pub cursor: Rgb,
    /// Background of selected cells.
    pub selection: Rgb,
    /// The 16 ANSI colors: 0–7 normal, 8–15 bright.
    pub ansi: [Rgb; 16],
}

/// Dark theme with a light-blue accent.
pub const DARK: Palette = Palette {
    foreground: Rgb::hex(0xd8dde8),
    background: Rgb::hex(0x141821),
    cursor: Rgb::hex(0x9ecbff),
    selection: Rgb::hex(0x2f4a6e),
    ansi: [
        Rgb::hex(0x1b1f27),
        Rgb::hex(0xe06c75),
        Rgb::hex(0x98c379),
        Rgb::hex(0xe5c07b),
        Rgb::hex(0x61afef),
        Rgb::hex(0xc678dd),
        Rgb::hex(0x56b6c2),
        Rgb::hex(0xabb2bf),
        Rgb::hex(0x5c6370),
        Rgb::hex(0xff7b86),
        Rgb::hex(0xb5e890),
        Rgb::hex(0xffd68a),
        Rgb::hex(0x7cc4ff),
        Rgb::hex(0xde9bff),
        Rgb::hex(0x7fd8e2),
        Rgb::hex(0xffffff),
    ],
};

impl Palette {
    /// Resolve an xterm 256-color index: 0–15 ANSI, 16–231 color cube, 232–255 grayscale.
    pub fn indexed(&self, index: u8) -> Rgb {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
                let i = index - 16;
                Rgb {
                    r: LEVELS[(i / 36) as usize],
                    g: LEVELS[((i / 6) % 6) as usize],
                    b: LEVELS[(i % 6) as usize],
                }
            }
            232..=255 => {
                let v = 8 + 10 * (index - 232);
                Rgb { r: v, g: v, b: v }
            }
        }
    }
}
