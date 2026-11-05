use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An sRGB colour, written as `"#rrggbb"` in JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// The Win32 `COLORREF` layout: `0x00BBGGRR`.
    pub const fn to_colorref(self) -> u32 {
        (self.b as u32) << 16 | (self.g as u32) << 8 | self.r as u32
    }

    pub const fn to_rgb_u32(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl FromStr for Color {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s.trim().trim_start_matches('#');
        let bad = || format!("invalid colour {s:?}, expected \"#rrggbb\"");
        let expanded: String = match hex.len() {
            3 => hex.chars().flat_map(|c| [c, c]).collect(),
            6 => hex.to_string(),
            _ => return Err(bad()),
        };
        let v = u32::from_str_radix(&expanded, 16).map_err(|_| bad())?;
        Ok(Self::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format() {
        assert_eq!("#7aa2f7".parse::<Color>().unwrap(), Color::rgb(0x7a, 0xa2, 0xf7));
        assert_eq!("#fff".parse::<Color>().unwrap(), Color::rgb(255, 255, 255));
        assert_eq!(Color::rgb(1, 2, 3).to_string(), "#010203");
        assert!("#12345".parse::<Color>().is_err());
        assert!("zzzzzz".parse::<Color>().is_err());
    }

    #[test]
    fn colorref_is_bgr() {
        assert_eq!(Color::rgb(0x11, 0x22, 0x33).to_colorref(), 0x00332211);
    }
}
