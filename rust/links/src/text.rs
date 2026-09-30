//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Sender-written text as it may be shown (ADR-0063 §6.1, "显示欺骗").
//!
//! Titles and attr values come from whoever sent the message, so they can carry characters that
//! change how a line is laid out: zero-width characters that hide or split words, and bidi
//! controls that reorder the text around them. The rules are the ones the ADR fixes:
//!
//! - U+200B, U+2060 and U+FEFF are stripped wherever they are.
//! - U+200C / U+200D (ZWNJ / ZWJ) and U+200E / U+200F (LRM / RLM) stay inside the text: stripping
//!   ZWJ would split emoji sequences (families, flags), stripping ZWNJ would break the joining of
//!   Persian, Hindi and other scripts (the app ships fa / ug / ar). At the edge of a text they
//!   mean nothing and go with the whitespace.
//! - The bidi controls (U+202A–U+202E, U+2066–U+2069) stay but are balanced the way Signal's
//!   `filterStringForDisplay` does on iOS: a pop without a start gets one prepended, a start without
//!   a pop gets one appended, so the controls cannot reach past this text.
//! - A text with nothing visible left is empty.
//!
//! One implementation here means the three clients show the same text for the same input.

/// Stripped wherever they are.
fn is_stripped(c: char) -> bool {
    matches!(c, '\u{200B}' | '\u{2060}' | '\u{FEFF}')
}

/// Means nothing at the edge of a text: whitespace and every invisible character.
fn is_edge_invisible(c: char) -> bool {
    c.is_whitespace() || is_stripped(c) || matches!(c, '\u{200C}'..='\u{200F}' | '\u{00AD}')
}

const LRE: char = '\u{202A}';
const RLE: char = '\u{202B}';
const PDF: char = '\u{202C}';
const LRO: char = '\u{202D}';
const RLO: char = '\u{202E}';
const LRI: char = '\u{2066}';
const RLI: char = '\u{2067}';
const FSI: char = '\u{2068}';
const PDI: char = '\u{2069}';

fn is_bidi_control(c: char) -> bool {
    matches!(c, LRE | RLE | PDF | LRO | RLO | LRI | RLI | FSI | PDI)
}

/// Make every bidi start and pop in `text` pair up (counts only, like Signal iOS).
fn balance_bidi(text: &str) -> String {
    let (mut isolate_starts, mut isolate_pops, mut format_starts, mut format_pops) = (0, 0, 0, 0);
    for c in text.chars() {
        match c {
            LRI | RLI | FSI => isolate_starts += 1,
            PDI => isolate_pops += 1,
            LRE | RLE | LRO | RLO => format_starts += 1,
            PDF => format_pops += 1,
            _ => {}
        }
    }
    if isolate_starts == isolate_pops && format_starts == format_pops {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 8);
    while isolate_pops > isolate_starts {
        out.push(FSI);
        isolate_starts += 1;
    }
    while format_pops > format_starts {
        out.push(LRE);
        format_starts += 1;
    }
    out.push_str(text);
    while format_starts > format_pops {
        out.push(PDF);
        format_pops += 1;
    }
    while isolate_starts > isolate_pops {
        out.push(PDI);
        isolate_pops += 1;
    }
    out
}

/// `text` as it may be displayed; empty when nothing visible is left.
pub(crate) fn display_text(text: &str) -> String {
    let stripped: String = text.chars().filter(|&c| !is_stripped(c)).collect();
    let trimmed = stripped.trim_matches(is_edge_invisible);
    if !trimmed
        .chars()
        .any(|c| !is_edge_invisible(c) && !is_bidi_control(c))
    {
        return String::new();
    }
    balance_bidi(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_unchanged() {
        for s in [
            "《柯洁围棋入门课》",
            "Hello, world",
            "a b  c",
            "😀 emoji",
            "",
        ] {
            assert_eq!(display_text(s), s);
        }
    }

    #[test]
    fn strips_zero_width_characters_everywhere() {
        assert_eq!(display_text("a\u{200B}b\u{2060}c\u{FEFF}d"), "abcd");
        assert_eq!(display_text("\u{FEFF}Title\u{200B}"), "Title");
    }

    #[test]
    fn keeps_joiners_and_direction_marks_inside_the_text() {
        // Family emoji: woman + ZWJ + man + ZWJ + girl.
        let family = "👩\u{200D}👨\u{200D}👧";
        assert_eq!(display_text(family), family);
        // Persian "می‌خواهم": ZWNJ between morphemes.
        let persian = "می\u{200C}خواهم";
        assert_eq!(display_text(persian), persian);
        let marks = "a\u{200E}b\u{200F}c";
        assert_eq!(display_text(marks), marks);
    }

    #[test]
    fn trims_whitespace_and_invisibles_at_the_edge() {
        assert_eq!(display_text(" \u{200B}\u{200D}  hi \t\n\u{200C}"), "hi");
        assert_eq!(display_text("\u{200E}hi\u{200F}"), "hi");
        assert_eq!(display_text("\u{3000}hi\u{00A0}"), "hi");
        assert_eq!(display_text("\u{00AD}hi\u{00AD}"), "hi");
    }

    #[test]
    fn balances_bidi_controls_like_signal_ios() {
        assert_eq!(display_text("\u{202E}abc"), "\u{202E}abc\u{202C}");
        assert_eq!(display_text("abc\u{202C}"), "\u{202A}abc\u{202C}");
        assert_eq!(display_text("\u{2067}x"), "\u{2067}x\u{2069}");
        assert_eq!(display_text("x\u{2069}"), "\u{2068}x\u{2069}");
        // A start and a pop of each kind, in either order, are already balanced.
        for balanced in [
            "\u{202A}abc\u{202C}",
            "\u{2066}abc\u{2069}",
            "a\u{202C}b\u{202A}c",
        ] {
            assert_eq!(display_text(balanced), balanced);
        }
        assert_eq!(
            display_text("\u{202E}\u{2067}ab"),
            "\u{202E}\u{2067}ab\u{202C}\u{2069}"
        );
    }

    #[test]
    fn nothing_visible_is_empty() {
        assert_eq!(display_text("\u{200B} \u{2060}\u{FEFF}"), "");
        assert_eq!(display_text("\u{200C}\u{200D}\u{200E}"), "");
        assert_eq!(display_text("\u{202E}\u{202C}"), "");
        assert_eq!(display_text("\u{2066}\u{2069} "), "");
        assert_eq!(display_text("   "), "");
    }

    #[test]
    fn is_idempotent() {
        for s in [
            "\u{202E}abc",
            "a\u{200B}b",
            "abc\u{2069}\u{202C}",
            " \u{200D}x\u{202B} ",
            "👩\u{200D}👨\u{200D}👧",
        ] {
            let once = display_text(s);
            assert_eq!(display_text(&once), once, "{s:?}");
        }
    }
}
