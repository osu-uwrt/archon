//! TOML in colour: each table (`[environment]`, every `[[board]]`) takes the next hue, its header and keys in it,
//! so a file of many boards reads as bands; array brackets go round the hues by depth; values and comments keep
//! quieter colours of their own.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Visuals};

use crate::plot::SERIES_COLORS;

/// The colours, from the visuals of the theme it's shown in
struct Ink {
    text: Color32,
    weak: Color32,
    string: Color32,
    number: Color32,
}

pub fn highlight(text: &str, font: &FontId, visuals: &Visuals) -> LayoutJob {
    let ink = Ink {
        text: visuals.text_color(),
        weak: visuals.weak_text_color(),
        // Values lean warm and cool, but well short of the table hues, so the bands stay the loudest thing
        string: visuals.text_color().lerp_to_gamma(Color32::from_rgb(0x9c, 0xd6, 0x8f), 0.55),
        number: visuals.text_color().lerp_to_gamma(Color32::from_rgb(0xf0, 0xb4, 0x6a), 0.55),
    };
    let mut job = LayoutJob::default();
    let mut put = |job: &mut LayoutJob, piece: &str, color: Color32, italic: bool| {
        if !piece.is_empty() {
            job.append(piece, 0.0, TextFormat { font_id: font.clone(), color, italics: italic, ..Default::default() });
        }
    };
    // Before the first table, keys are the document's own
    let mut hue = ink.text;
    let mut tables = 0;
    for line in text.split_inclusive('\n') {
        let (body, newline) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        let trimmed = body.trim_start();
        let indent = &body[..body.len() - trimmed.len()];
        put(&mut job, indent, ink.text, false);
        if trimmed.starts_with('#') {
            put(&mut job, trimmed, ink.weak, true);
        } else if trimmed.starts_with('[') {
            hue = SERIES_COLORS[tables % SERIES_COLORS.len()];
            tables += 1;
            let (header, comment) = split_comment(trimmed);
            put(&mut job, header, hue, false);
            put(&mut job, comment, ink.weak, true);
        } else if let Some(at) = key_end(trimmed) {
            let (key, rest) = trimmed.split_at(at);
            put(&mut job, key, hue, false);
            put(&mut job, "=", ink.weak, false);
            value(&mut job, &rest[1..], &ink, &mut put);
        } else {
            put(&mut job, trimmed, ink.text, false);
        }
        put(&mut job, newline, ink.text, false);
    }
    job
}

/// Where a `key = value` line's `=` is, outside any quotes
fn key_end(line: &str) -> Option<usize> {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '=' if !quoted => return Some(i),
            _ => {}
        }
    }
    None
}

/// A header and the comment after it, if any
fn split_comment(line: &str) -> (&str, &str) {
    match line.find(" #") {
        Some(at) => line.split_at(at),
        None => (line, ""),
    }
}

/// A value: strings, numbers and booleans in their colours, brackets round the hues by depth
fn value(job: &mut LayoutJob, text: &str, ink: &Ink, put: &mut impl FnMut(&mut LayoutJob, &str, Color32, bool)) {
    let mut depth = 0usize;
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let len = match c {
            '"' | '\'' => {
                // To the closing quote, past escaped ones
                let mut end = rest.len();
                let mut escaped = false;
                for (i, d) in rest.char_indices().skip(1) {
                    if escaped {
                        escaped = false;
                    } else if d == '\\' && c == '"' {
                        escaped = true;
                    } else if d == c {
                        end = i + 1;
                        break;
                    }
                }
                put(job, &rest[..end], ink.string, false);
                end
            }
            '[' | '{' => {
                put(job, &rest[..1], SERIES_COLORS[depth % SERIES_COLORS.len()], false);
                depth += 1;
                1
            }
            ']' | '}' => {
                depth = depth.saturating_sub(1);
                put(job, &rest[..1], SERIES_COLORS[depth % SERIES_COLORS.len()], false);
                1
            }
            '#' => {
                put(job, rest, ink.weak, true);
                rest.len()
            }
            ',' | ' ' | '=' => {
                put(job, &rest[..1], ink.weak, false);
                1
            }
            _ => {
                // A bare word: a number, a boolean or a date, up to the next separator
                let end = rest.find([',', ']', '}', ' ', '#']).unwrap_or(rest.len()).max(c.len_utf8());
                let word = &rest[..end];
                let color = if word == "true"
                    || word == "false"
                    || word.starts_with(|d: char| d.is_ascii_digit() || d == '-' || d == '+')
                {
                    ink.number
                } else {
                    ink.text
                };
                put(job, word, color, false);
                end
            }
        };
        rest = &rest[len..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The colour each piece of text came out in
    fn pieces(text: &str) -> Vec<(String, Color32)> {
        let job = highlight(text, &FontId::monospace(12.0), &Visuals::dark());
        job.sections
            .iter()
            .map(|s| (job.text[s.byte_range.start.0..s.byte_range.end.0].to_owned(), s.format.color))
            .collect()
    }

    fn color_of(pieces: &[(String, Color32)], text: &str) -> Color32 {
        pieces.iter().find(|(t, _)| t.trim() == text).unwrap_or_else(|| panic!("no {text:?} in {pieces:?}")).1
    }

    #[test]
    fn each_table_takes_the_next_hue() {
        let p =
            pieces("# robot\n[environment]\nfile = \"pool\"\n\n[[board]]\nname = \"a\"\n\n[[board]]\nbus = \"b\"\n");
        assert_eq!(color_of(&p, "[environment]"), SERIES_COLORS[0]);
        assert_eq!(color_of(&p, "file"), SERIES_COLORS[0], "keys wear their table's hue");
        assert_eq!(color_of(&p, "name"), SERIES_COLORS[1]);
        assert_eq!(color_of(&p, "bus"), SERIES_COLORS[2]);
        assert_eq!(color_of(&p, "# robot"), Visuals::dark().weak_text_color());
        assert_ne!(color_of(&p, "\"pool\""), SERIES_COLORS[0], "values aren't in the band's hue");
        // Nothing is lost on the way
        let text: String = p.iter().map(|(t, _)| t.as_str()).collect();
        assert!(text.ends_with("bus = \"b\"\n"));
    }

    #[test]
    fn brackets_go_round_by_depth() {
        let p = pieces("x = [[1, 2], [3]]\n");
        let brackets: Vec<Color32> = p.iter().filter(|(t, _)| t == "[" || t == "]").map(|(_, c)| *c).collect();
        assert_eq!(brackets[0], SERIES_COLORS[0]);
        assert_eq!(brackets[1], SERIES_COLORS[1]);
        assert_eq!(brackets[2], SERIES_COLORS[1], "a closing bracket matches its opening one");
        assert_eq!(*brackets.last().unwrap(), SERIES_COLORS[0]);
        assert_eq!(color_of(&p, "1"), color_of(&p, "3"));
    }

    #[test]
    fn quotes_hold_equals_signs_and_escapes() {
        let p = pieces("cmd = \"a = \\\"b\\\"\" # note\n");
        assert_eq!(color_of(&p, "cmd"), Visuals::dark().text_color(), "before any table, keys are plain");
        assert!(p.iter().any(|(t, _)| t == "\"a = \\\"b\\\"\""), "{p:?}");
        assert_eq!(color_of(&p, "# note"), Visuals::dark().weak_text_color());
    }
}
