//! Theme colours: parsing, contrast, and deriving the full palette from a few choices.

use std::collections::BTreeMap;
use std::fmt;

use crate::theme::Base;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(hex: u32) -> Color {
        Color { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8, a: 255 }
    }

    /// Parses `#rgb`, `#rrggbb` or `#rrggbbaa`.
    pub fn parse(s: &str) -> Option<Color> {
        let h = s.strip_prefix('#')?;
        if !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let v = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok();
        match h.len() {
            3 => Some(Color { r: v(0, 1)? * 17, g: v(1, 1)? * 17, b: v(2, 1)? * 17, a: 255 }),
            6 => Some(Color { r: v(0, 2)?, g: v(2, 2)?, b: v(4, 2)?, a: 255 }),
            8 => Some(Color { r: v(0, 2)?, g: v(2, 2)?, b: v(4, 2)?, a: v(6, 2)? }),
            _ => None,
        }
    }

    pub fn with_alpha(self, a: f64) -> Color {
        Color { a: (a.clamp(0.0, 1.0) * 255.0).round() as u8, ..self }
    }

    /// WCAG relative luminance (alpha ignored).
    pub fn luminance(self) -> f64 {
        let lin = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.039_28 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }

    /// WCAG contrast ratio, 1 to 21.
    pub fn contrast(self, other: Color) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    pub fn to_argb(self) -> u32 {
        u32::from(self.a) << 24 | u32::from(self.r) << 16 | u32::from(self.g) << 8 | u32::from(self.b)
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)?;
        if self.a != 255 {
            write!(f, "{:02x}", self.a)?;
        }
        Ok(())
    }
}

/// Every colour token a theme exposes, in the order they appear in `Theme`.
pub const TOKENS: [&str; 15] = [
    "bg",
    "panel",
    "raised",
    "border",
    "border-hover",
    "text",
    "dim",
    "faint",
    "accent",
    "accent-ink",
    "accent-soft",
    "accent-glow",
    "ok",
    "bad",
    "warn",
];

pub const DEFAULT_ACCENT: Color = Color::rgb(0xc6ff3d);

fn neutrals(base: Base) -> [(&'static str, Color); 8] {
    match base {
        // trap.foarte.top's palette.
        Base::Dark => [
            ("bg", Color::rgb(0x0b0b0c)),
            ("panel", Color::rgb(0x141416)),
            ("raised", Color::rgb(0x1b1b1f)),
            ("border", Color::rgb(0x26262b)),
            ("border-hover", Color::rgb(0x3a3a42)),
            ("text", Color::rgb(0xf3f3ee)),
            ("dim", Color::rgb(0xa6a69e)),
            ("faint", Color::rgb(0x6e6e68)),
        ],
        Base::Light => [
            ("bg", Color::rgb(0xf4f4ef)),
            ("panel", Color::rgb(0xffffff)),
            ("raised", Color::rgb(0xebebe5)),
            ("border", Color::rgb(0xd6d6cf)),
            ("border-hover", Color::rgb(0xb4b4ac)),
            ("text", Color::rgb(0x0b0b0c)),
            ("dim", Color::rgb(0x4d4d48)),
            ("faint", Color::rgb(0x7a7a73)),
        ],
    }
}

/// Builds the full palette: neutrals from `base`, accent variants from `accent`, then
/// `overrides` (already validated) on top.
pub fn derive(base: Base, accent: Option<Color>, overrides: &BTreeMap<String, Color>) -> BTreeMap<&'static str, Color> {
    let mut p: BTreeMap<&'static str, Color> = neutrals(base).into_iter().collect();
    let accent = accent.unwrap_or(DEFAULT_ACCENT);
    let (bg, text) = (p["bg"], p["text"]);
    let (dark, light) = if bg.luminance() < text.luminance() { (bg, text) } else { (text, bg) };
    let ink = if accent.contrast(dark) >= accent.contrast(light) { dark } else { light };
    p.insert("accent", accent);
    p.insert("accent-ink", ink);
    p.insert("accent-soft", accent.with_alpha(0.10));
    p.insert("accent-glow", accent.with_alpha(0.35));
    p.insert("ok", accent);
    p.insert("bad", Color::rgb(0xff5b4f));
    p.insert("warn", Color::rgb(0xffc53d));
    for (k, v) in overrides {
        if let Some(&token) = TOKENS.iter().find(|t| **t == k.as_str()) {
            p.insert(token, *v);
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_forms() {
        assert_eq!(Color::parse("#c6ff3d"), Some(Color::rgb(0xc6ff3d)));
        assert_eq!(Color::parse("#fff"), Some(Color::rgb(0xffffff)));
        assert_eq!(Color::parse("#00000080").map(|c| c.a), Some(0x80));
        for bad in ["c6ff3d", "#c6ff3", "#gggggg", "#", "#+1+2+3"] {
            assert_eq!(Color::parse(bad), None, "{bad}");
        }
        assert_eq!(Color::rgb(0xc6ff3d).with_alpha(0.1).to_string(), "#c6ff3d1a");
    }

    #[test]
    fn ink_follows_contrast() {
        let none = BTreeMap::new();
        let lime = derive(Base::Dark, None, &none);
        assert_eq!(lime["accent-ink"], Color::rgb(0x0b0b0c), "lime takes dark ink");
        let navy = derive(Base::Dark, Color::parse("#1a237e"), &none);
        assert_eq!(navy["accent-ink"], Color::rgb(0xf3f3ee), "navy takes light ink");
        let light = derive(Base::Light, Color::parse("#1a237e"), &none);
        assert_eq!(light["accent-ink"], Color::rgb(0xf4f4ef), "light base: ink is its bg");
    }

    #[test]
    fn overrides_win_and_every_token_exists() {
        let mut o = BTreeMap::new();
        o.insert("accent-ink".to_string(), Color::rgb(0xff0000));
        o.insert("bg".to_string(), Color::rgb(0x123456));
        let p = derive(Base::Dark, None, &o);
        assert_eq!(p["accent-ink"], Color::rgb(0xff0000));
        assert_eq!(p["bg"], Color::rgb(0x123456));
        for t in TOKENS {
            assert!(p.contains_key(t), "{t}");
        }
        assert_eq!(p.len(), TOKENS.len());
    }
}
