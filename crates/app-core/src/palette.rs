//! Default dark color theme and xterm 256-color resolution.

use protocol::message::PaletteColors;

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

/// Light theme: dark text on a pale background, ANSI colours darkened to read on it.
pub const LIGHT: Palette = Palette {
    foreground: Rgb::hex(0x1c2333),
    background: Rgb::hex(0xf8f9fc),
    cursor: Rgb::hex(0x3d86d9),
    selection: Rgb::hex(0xc9dcf3),
    ansi: [
        Rgb::hex(0x1c2333),
        Rgb::hex(0xc0392b),
        Rgb::hex(0x2e7d32),
        Rgb::hex(0x9a6b00),
        Rgb::hex(0x1f63b5),
        Rgb::hex(0x8e3fb0),
        Rgb::hex(0x0f7c86),
        Rgb::hex(0xd6dce7),
        Rgb::hex(0x566073),
        Rgb::hex(0xd0453a),
        Rgb::hex(0x3a8f3f),
        Rgb::hex(0xa87700),
        Rgb::hex(0x2a73c9),
        Rgb::hex(0x9c4ec0),
        Rgb::hex(0x16899a),
        Rgb::hex(0xffffff),
    ],
};

impl Default for Palette {
    fn default() -> Self {
        DARK
    }
}

impl From<Palette> for PaletteColors {
    fn from(p: Palette) -> Self {
        let pack = |c: Rgb| u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b);
        PaletteColors { foreground: pack(p.foreground), background: pack(p.background), cursor: pack(p.cursor), selection: pack(p.selection), ansi: p.ansi.map(pack) }
    }
}

impl From<PaletteColors> for Palette {
    fn from(c: PaletteColors) -> Self {
        Palette { foreground: Rgb::hex(c.foreground), background: Rgb::hex(c.background), cursor: Rgb::hex(c.cursor), selection: Rgb::hex(c.selection), ansi: c.ansi.map(Rgb::hex) }
    }
}

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
