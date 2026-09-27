//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The two pure functions behind the card's look (ADR-0063 §4.10; card-visual §3.2 / §3.3), so
//! the composer preview and all three receivers agree to the pixel.
//!
//! * [`layout`]: which of the four card shapes, from the image's pixel size and the kind only.
//! * [`tint`]: background and text colour from the card's own image, the way iOS does it
//!   (card-visual §2.3): no brand colour table, no `theme-color`, nothing the sender says.

use serde::{Serialize, Serializer};

use crate::card::Level;
use crate::limits::FIRST_PARTY_KIND_PREFIX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// tell.cc / official: Tellomi's own neutral card with an action button (card-visual §5.2).
    FirstParty,
    /// Image on top, full card width; title bar underneath.
    LargeImage,
    /// Text on the left, a 44 pt square icon on the right, whole card tinted.
    Icon,
    /// Title + domain + a generic link glyph; grey.
    NoImage,
}

/// Short side ≥ 300 px **and** long side ≥ 600 px makes a large-image card; anything smaller is an
/// icon (card-visual §3.2, after Apple TN3156's "under 150 px may be treated as an icon").
pub const LARGE_MIN_SHORT_PX: u32 = 300;
pub const LARGE_MIN_LONG_PX: u32 = 600;

/// `image_w` / `image_h` are `Preview.image`'s pixel size (0 × 0 when there is none); for a brand
/// shell pass the bundled icon's size, or 0 × 0 when it has none.
pub fn layout(image_w: u32, image_h: u32, kind: &str, level: Level) -> Layout {
    if level == Level::FirstParty || kind.starts_with(FIRST_PARTY_KIND_PREFIX) {
        return Layout::FirstParty;
    }
    let has_image = image_w > 0 && image_h > 0;
    if level == Level::PlainLink || !has_image {
        return Layout::NoImage;
    }
    if level == Level::Brand {
        return Layout::Icon;
    }
    let (short, long) = (image_w.min(image_h), image_w.max(image_h));
    if short >= LARGE_MIN_SHORT_PX && long >= LARGE_MIN_LONG_PX {
        Layout::LargeImage
    } else {
        Layout::Icon
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };
    pub const WHITE: Rgb = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };

    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    fn channel(c: u8) -> f64 {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// WCAG 2 relative luminance.
    pub fn luminance(self) -> f64 {
        0.2126 * Self::channel(self.r)
            + 0.7152 * Self::channel(self.g)
            + 0.0722 * Self::channel(self.b)
    }

    pub fn contrast(self, other: Rgb) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// (hue in degrees, saturation, lightness), HSL.
    pub fn hsl(self) -> (f64, f64, f64) {
        let (r, g, b) = (
            f64::from(self.r) / 255.0,
            f64::from(self.g) / 255.0,
            f64::from(self.b) / 255.0,
        );
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let l = (max + min) / 2.0;
        if (max - min).abs() < f64::EPSILON {
            return (0.0, 0.0, l);
        }
        let d = max - min;
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        let h = if (max - r).abs() < f64::EPSILON {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if (max - g).abs() < f64::EPSILON {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        (h * 60.0, s, l)
    }

    pub fn from_hsl(h: f64, s: f64, l: f64) -> Rgb {
        let hue = |p: f64, q: f64, mut t: f64| {
            if t < 0.0 {
                t += 1.0;
            }
            if t > 1.0 {
                t -= 1.0;
            }
            if t < 1.0 / 6.0 {
                p + (q - p) * 6.0 * t
            } else if t < 0.5 {
                q
            } else if t < 2.0 / 3.0 {
                p + (q - p) * (2.0 / 3.0 - t) * 6.0
            } else {
                p
            }
        };
        let (r, g, b) = if s == 0.0 {
            (l, l, l)
        } else {
            let q = if l < 0.5 {
                l * (1.0 + s)
            } else {
                l + s - l * s
            };
            let p = 2.0 * l - q;
            let h = h / 360.0;
            (
                hue(p, q, h + 1.0 / 3.0),
                hue(p, q, h),
                hue(p, q, h - 1.0 / 3.0),
            )
        };
        Rgb {
            r: to_u8(r * 255.0),
            g: to_u8(g * 255.0),
            b: to_u8(b * 255.0),
        }
    }
}

fn to_u8(v: f64) -> u8 {
    // clamped to 0..=255 first, so the cast cannot truncate
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = v.round().clamp(0.0, 255.0) as u8;
    byte
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Colors {
    pub background: Rgb,
    pub text: Rgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Tint {
    /// `false`: use the default neutral card colours.
    pub tinted: bool,
    /// The colour the rule picked from the image, tinted or not (for tests and debugging).
    pub source: Option<Rgb>,
    pub light: Option<Colors>,
    pub dark: Option<Colors>,
}

impl Tint {
    const NONE: Tint = Tint {
        tinted: false,
        source: None,
        light: None,
        dark: None,
    };
}

/// WCAG AA for the title (card-visual §3.3 step 3).
pub const MIN_CONTRAST: f64 = 4.5;
/// Dark mode keeps the hue but never goes lighter than this (card-visual §3.3 step 4).
pub const DARK_MAX_LIGHTNESS: f64 = 0.30;
const SIDE: usize = 32;

/// Area-average an RGBA image to 32 × 32 (premultiplied, so transparent pixels do not bleed
/// black). Smaller images are sampled up by nearest neighbour.
fn downsample(width: usize, height: usize, rgba: &[u8]) -> Vec<[f64; 4]> {
    let mut out = Vec::with_capacity(SIDE * SIDE);
    for ty in 0..SIDE {
        let y0 = ty * height / SIDE;
        let y1 = ((ty + 1) * height / SIDE).max(y0 + 1);
        for tx in 0..SIDE {
            let x0 = tx * width / SIDE;
            let x1 = ((tx + 1) * width / SIDE).max(x0 + 1);
            let mut acc = [0.0f64; 4];
            let mut n = 0.0;
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = &rgba[(y * width + x) * 4..][..4];
                    let a = f64::from(p[3]) / 255.0;
                    acc[0] += f64::from(p[0]) * a;
                    acc[1] += f64::from(p[1]) * a;
                    acc[2] += f64::from(p[2]) * a;
                    acc[3] += a;
                    n += 1.0;
                }
            }
            let alpha = acc[3] / n;
            if acc[3] > 0.0 {
                out.push([
                    acc[0] / acc[3],
                    acc[1] / acc[3],
                    acc[2] / acc[3],
                    alpha * 255.0,
                ]);
            } else {
                out.push([0.0, 0.0, 0.0, 0.0]);
            }
        }
    }
    out
}

/// Icon cards: drop pixels with alpha < 128, bucket by the top 3 bits of each channel, take the
/// fullest bucket and average it (card-visual §3.3 step 1).
fn dominant_icon_colour(pixels: &[[f64; 4]]) -> Option<Rgb> {
    let mut buckets: Vec<(usize, [f64; 3])> = vec![(0, [0.0; 3]); 512];
    for p in pixels.iter().filter(|p| p[3] >= 128.0) {
        let q = |c: f64| usize::from(to_u8(c) >> 5);
        let idx = (q(p[0]) << 6) | (q(p[1]) << 3) | q(p[2]);
        buckets[idx].0 += 1;
        for (sum, v) in buckets[idx].1.iter_mut().zip(p) {
            *sum += v;
        }
    }
    // Most pixels wins; ties go to the lower bucket index so the answer is deterministic.
    let (count, sum) = buckets
        .iter()
        .enumerate()
        .max_by(|(ia, a), (ib, b)| a.0.cmp(&b.0).then(ib.cmp(ia)))
        .map(|(_, b)| *b)?;
    if count == 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = count as f64;
    Some(Rgb {
        r: to_u8(sum[0] / n),
        g: to_u8(sum[1] / n),
        b: to_u8(sum[2] / n),
    })
}

/// Large-image cards: the title bar continues the image, so it takes the average of the bottom
/// 10 % of the picture (card-visual §3.3 step 2).
fn bottom_strip_colour(width: usize, height: usize, rgba: &[u8]) -> Option<Rgb> {
    let rows = (height / 10).max(1);
    let mut acc = [0.0f64; 3];
    let mut weight = 0.0;
    for y in height - rows..height {
        for x in 0..width {
            let p = &rgba[(y * width + x) * 4..][..4];
            let a = f64::from(p[3]) / 255.0;
            for (sum, v) in acc.iter_mut().zip(p) {
                *sum += f64::from(*v) * a;
            }
            weight += a;
        }
    }
    (weight > 0.0).then(|| Rgb {
        r: to_u8(acc[0] / weight),
        g: to_u8(acc[1] / weight),
        b: to_u8(acc[2] / weight),
    })
}

/// White, grey and black are not colours worth tinting a card with (card-visual §3.3 step 1).
pub fn is_neutral(c: Rgb) -> bool {
    let (_, s, l) = c.hsl();
    s < 0.25 || l > 0.92 || l < 0.08
}

fn light_colors(c: Rgb) -> Colors {
    let text = if c.contrast(Rgb::BLACK) >= c.contrast(Rgb::WHITE) {
        Rgb::BLACK
    } else {
        Rgb::WHITE
    };
    let (h, s, mut l) = c.hsl();
    let mut bg = c;
    // Same hue, nudged lighter under black text or darker under white, until the title passes AA.
    while bg.contrast(text) < MIN_CONTRAST {
        l = if text == Rgb::BLACK {
            (l + 0.01).min(1.0)
        } else {
            (l - 0.01).max(0.0)
        };
        bg = Rgb::from_hsl(h, s, l);
        if l <= 0.0 || l >= 1.0 {
            break;
        }
    }
    Colors {
        background: bg,
        text,
    }
}

fn dark_colors(c: Rgb) -> Colors {
    let (h, s, l) = c.hsl();
    let mut l = l.min(DARK_MAX_LIGHTNESS);
    let mut bg = Rgb::from_hsl(h, s, l);
    while bg.contrast(Rgb::WHITE) < MIN_CONTRAST && l > 0.0 {
        l = (l - 0.01).max(0.0);
        bg = Rgb::from_hsl(h, s, l);
    }
    Colors {
        background: bg,
        text: Rgb::WHITE,
    }
}

/// Card colours from the card's own image, decoded to RGBA (any size; it is reduced here so every
/// platform samples the same way). Call it only for third-party cards — not first-party, not
/// payment, not inside a message request (card-visual §3.3, owner 2026-09-26).
pub fn tint(layout: Layout, width: u32, height: u32, rgba: &[u8]) -> Tint {
    let (Ok(w), Ok(h)) = (usize::try_from(width), usize::try_from(height)) else {
        return Tint::NONE;
    };
    if w == 0 || h == 0 || rgba.len() != w.saturating_mul(h).saturating_mul(4) {
        return Tint::NONE;
    }
    let source = match layout {
        Layout::Icon => dominant_icon_colour(&downsample(w, h, rgba)),
        Layout::LargeImage => bottom_strip_colour(w, h, rgba),
        Layout::FirstParty | Layout::NoImage => None,
    };
    let Some(source) = source else {
        return Tint::NONE;
    };
    if is_neutral(source) {
        return Tint {
            source: Some(source),
            ..Tint::NONE
        };
    }
    Tint {
        tinted: true,
        source: Some(source),
        light: Some(light_colors(source)),
        dark: Some(dark_colors(source)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        assert_eq!(
            layout(1200, 630, "video", Level::Structured),
            Layout::LargeImage
        );
        assert_eq!(layout(1200, 299, "video", Level::Structured), Layout::Icon);
        assert_eq!(layout(100, 100, "", Level::Generic), Layout::Icon);
        assert_eq!(layout(0, 0, "", Level::Generic), Layout::NoImage);
        assert_eq!(layout(1200, 630, "product", Level::Brand), Layout::Icon);
        assert_eq!(layout(0, 0, "product", Level::Brand), Layout::NoImage);
        assert_eq!(
            layout(1200, 630, "tellomi.group", Level::FirstParty),
            Layout::FirstParty
        );
        assert_eq!(layout(1200, 630, "", Level::PlainLink), Layout::NoImage);
    }

    #[test]
    fn colour_math() {
        assert!((Rgb::BLACK.contrast(Rgb::WHITE) - 21.0).abs() < 1e-9);
        let c = Rgb {
            r: 0xFB,
            g: 0xC8,
            b: 0x00,
        };
        let back = {
            let (h, s, l) = c.hsl();
            Rgb::from_hsl(h, s, l)
        };
        assert_eq!(back, c);
        assert!(is_neutral(Rgb::WHITE) && is_neutral(Rgb::BLACK));
        assert!(is_neutral(Rgb {
            r: 128,
            g: 128,
            b: 130
        }));
        assert!(!is_neutral(c));
    }

    #[test]
    fn contrast_is_always_met() {
        for c in [
            Rgb {
                r: 0xFE,
                g: 0x75,
                b: 0x00,
            },
            Rgb {
                r: 0x00,
                g: 0xA1,
                b: 0xD6,
            },
            Rgb {
                r: 0x80,
                g: 0x40,
                b: 0xC0,
            },
        ] {
            let light = light_colors(c);
            assert!(
                light.background.contrast(light.text) >= MIN_CONTRAST,
                "{}",
                c.hex()
            );
            let dark = dark_colors(c);
            assert!(
                dark.background.contrast(dark.text) >= MIN_CONTRAST,
                "{}",
                c.hex()
            );
        }
    }

    #[test]
    fn bad_input_is_untinted() {
        assert_eq!(tint(Layout::Icon, 2, 2, &[0; 3]), Tint::NONE);
        assert_eq!(
            tint(Layout::FirstParty, 1, 1, &[255, 0, 0, 255]),
            Tint::NONE
        );
        let red = [255u8, 0, 0, 255].repeat(4);
        assert!(tint(Layout::Icon, 2, 2, &red).tinted);
        let clear = [255u8, 0, 0, 0].repeat(4);
        assert!(!tint(Layout::Icon, 2, 2, &clear).tinted);
    }
}
