//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! `layout` / `tint` against the ten standard samples of card-visual appendix A. The images are
//! test resources (tests/data/tint, see SOURCES); nothing is fetched.

mod common;

use std::io::Cursor;

use common::read;
use tellomi_links::{Layout, Level, Rgb, layout, tint};

/// RGBA8 of a PNG, whatever its colour type (palette, RGB, grey, 16-bit, interlaced).
fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().expect("png header");
    let mut buf = vec![0; reader.output_buffer_size().expect("size")];
    let info = reader.next_frame(&mut buf).expect("png frame");
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::GrayscaleAlpha => data
            .chunks(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        other => panic!("unexpected colour type after ALPHA|EXPAND: {other:?}"),
    };
    (info.width, info.height, rgba)
}

/// The largest image of an ICO: embedded PNG, or a 32-bit BMP (BGRA, bottom-up).
fn decode_ico(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let u16le = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32le = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
    assert_eq!((u16le(0), u16le(2)), (0, 1), "ICONDIR");
    let entry = (0..usize::from(u16le(4)))
        .map(|i| 6 + i * 16)
        .max_by_key(|&e| (u32::from(bytes[e]).max(1), u16le(e + 6)))
        .expect("an entry");
    let size = usize::try_from(u32le(entry + 8)).expect("size");
    let offset = usize::try_from(u32le(entry + 12)).expect("offset");
    let data = &bytes[offset..offset + size];
    if data.starts_with(b"\x89PNG") {
        return decode_png(data);
    }
    assert_eq!(u32le(offset), 40, "BITMAPINFOHEADER");
    let width = u32le(offset + 4);
    let height = u32le(offset + 8) / 2; // XOR + AND masks
    assert_eq!(u16le(offset + 14), 32, "only 32-bit icons are needed here");
    let (w, h) = (
        usize::try_from(width).unwrap(),
        usize::try_from(height).unwrap(),
    );
    let pixels = &data[40..40 + w * h * 4];
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        let src = &pixels[(h - 1 - y) * w * 4..][..w * 4];
        for x in 0..w {
            let p = &src[x * 4..x * 4 + 4];
            rgba[(y * w + x) * 4..][..4].copy_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    (width, height, rgba)
}

fn decode(path: &str) -> (u32, u32, Vec<u8>) {
    let bytes = read(&format!("tint/{path}"));
    if bytes.starts_with(b"\x89PNG") {
        decode_png(&bytes)
    } else {
        decode_ico(&bytes)
    }
}

fn hue(c: Rgb) -> f64 {
    c.hsl().0
}

struct Sample {
    file: &'static str,
    layout: Layout,
    /// `None` = not tinted; `Some((lo, hi))` = tinted with a hue in this range (degrees).
    hue: Option<(f64, f64)>,
    /// Title colour in light mode when tinted.
    text: Option<Rgb>,
}

#[test]
fn ten_standard_samples() {
    let samples = [
        // Meituan's 100×100 og:image: yellow ≈ #FBC800, black text.
        Sample {
            file: "meituan-og.png",
            layout: Layout::Icon,
            hue: Some((40.0, 56.0)),
            text: Some(Rgb::BLACK),
        },
        // Taobao apple-touch-icon: orange.
        Sample {
            file: "taobao-touch.png",
            layout: Layout::Icon,
            hue: Some((15.0, 40.0)),
            text: None,
        },
        // Bilibili 32×32 favicon on a transparent ground: cyan ≈ #00A1D6.
        Sample {
            file: "bilibili-favicon.ico",
            layout: Layout::Icon,
            hue: Some((185.0, 205.0)),
            text: None,
        },
        // JD: white wins over red → not tinted.
        Sample {
            file: "jd-favicon.ico",
            layout: Layout::Icon,
            hue: None,
            text: None,
        },
        Sample {
            file: "qq-favicon.ico",
            layout: Layout::Icon,
            hue: None,
            text: None,
        },
        Sample {
            file: "163-ipad-icon.png",
            layout: Layout::Icon,
            hue: None,
            text: None,
        },
        // Tellomi's own icon is black: neutral.
        Sample {
            file: "tellomi-touch.png",
            layout: Layout::Icon,
            hue: None,
            text: None,
        },
        // Cloudflare 1400×800: large card, orange title bar, black text.
        Sample {
            file: "cloudflare-preview.png",
            layout: Layout::LargeImage,
            hue: Some((10.0, 40.0)),
            text: Some(Rgb::BLACK),
        },
        Sample {
            file: "apple-og.png",
            layout: Layout::LargeImage,
            hue: None,
            text: None,
        },
        Sample {
            file: "youtube-1200.png",
            layout: Layout::LargeImage,
            hue: None,
            text: None,
        },
    ];
    let mut report = Vec::new();
    for s in &samples {
        let (w, h, rgba) = decode(s.file);
        let got_layout = layout(w, h, "", Level::Generic);
        assert_eq!(got_layout, s.layout, "{} ({w}×{h})", s.file);
        let t = tint(got_layout, w, h, &rgba);
        report.push(format!(
            "{:24} {w:>4}×{h:<4} {:?} tinted={} source={} light={}",
            s.file,
            got_layout,
            t.tinted,
            t.source.map(Rgb::hex).unwrap_or_default(),
            t.light
                .map(|c| format!("{}/{}", c.background.hex(), c.text.hex()))
                .unwrap_or_default()
        ));
        match s.hue {
            None => assert!(!t.tinted, "{}: should stay neutral, got {:?}", s.file, t),
            Some((lo, hi)) => {
                assert!(t.tinted, "{}: should be tinted, got {:?}", s.file, t);
                let h = hue(t.source.expect("source"));
                assert!(
                    (lo..=hi).contains(&h),
                    "{}: hue {h:.1} outside {lo}–{hi}",
                    s.file
                );
                let light = t.light.expect("light");
                assert!(light.background.contrast(light.text) >= 4.5, "{}", s.file);
                let dark = t.dark.expect("dark");
                assert_eq!(dark.text, Rgb::WHITE);
                assert!(dark.background.contrast(Rgb::WHITE) >= 4.5, "{}", s.file);
                if let Some(text) = s.text {
                    assert_eq!(light.text, text, "{}", s.file);
                }
            }
        }
    }
    println!("{}", report.join("\n"));
}

#[test]
fn first_party_and_brand_layouts() {
    let (w, h, rgba) = decode("cloudflare-preview.png");
    assert_eq!(
        layout(w, h, "tellomi.official", Level::FirstParty),
        Layout::FirstParty
    );
    // First-party cards are never tinted, whatever image they carry.
    assert!(!tint(Layout::FirstParty, w, h, &rgba).tinted);
    // A brand shell uses its bundled icon's size, and is an icon card even with a big image.
    assert_eq!(layout(w, h, "product", Level::Brand), Layout::Icon);
}
