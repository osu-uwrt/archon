//! The lines a source has written, kept in a bounded buffer.

use std::collections::VecDeque;
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Plain,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    /// Guesses a line's level from a level word near its start, which covers ROS 2 logs (`[ERROR] [123.4] [node]: ...`)
    /// as well as most firmware log formats.
    pub fn detect(text: &str) -> Level {
        const WORDS: &[(&str, Level)] = &[
            ("FATAL", Level::Error),
            ("ERROR", Level::Error),
            ("WARN", Level::Warn),
            ("INFO", Level::Info),
            ("DEBUG", Level::Debug),
        ];
        let head = &text[..text.floor_char_boundary(48)];
        WORDS
            .iter()
            .filter_map(|&(word, level)| head.find(word).map(|at| (at, level)))
            .min_by_key(|&(at, _)| at)
            .map_or(Level::Plain, |(_, level)| level)
    }
}

/// Who wrote a line: the source, or archon reporting on the source (started, exited, reconnected)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    Source,
    Archon,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub time: SystemTime,
    pub text: String,
    pub level: Level,
    pub origin: Origin,
}

pub struct LineBuffer {
    lines: VecDeque<Line>,
    capacity: usize,
    /// Whether the last line has no newline yet, so the next output continues it
    open: bool,
    /// Whether the last output ended in `\r`
    carriage_return: bool,
    /// How many lines have been evicted or cleared, so `evicted + index` names a line for as long as it's kept
    evicted: u64,
    /// Completed error and warning lines, ever, for unread badges
    errors: u64,
    warnings: u64,
}

impl LineBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            capacity: capacity.max(1),
            open: false,
            carriage_return: false,
            evicted: 0,
            errors: 0,
            warnings: 0,
        }
    }

    /// Appends a source's output. A lone `\r` returns to the start of the line, so progress bars overwrite in place.
    pub fn push_output(&mut self, text: &str) {
        let mut carriage_return = std::mem::take(&mut self.carriage_return);
        let mut pieces = text.split('\n').peekable();
        while let Some(mut piece) = pieces.next() {
            let ends_line = pieces.peek().is_some();
            if let Some(rest) = piece.strip_suffix('\r') {
                piece = rest;
                // A `\r` at the very end is either half of a `\r\n` or a return to the line start; the next read decides
                if !ends_line {
                    self.carriage_return = true;
                }
            }
            let overwrite = match piece.rfind('\r') {
                Some(at) => {
                    piece = &piece[at + 1..];
                    true
                }
                None => carriage_return && !piece.is_empty(),
            };
            carriage_return = false;

            if self.open {
                let line = self.lines.back_mut().expect("an open line exists");
                if overwrite {
                    line.text.clear();
                }
                line.text.push_str(piece);
                if line.level == Level::Plain {
                    line.level = Level::detect(&line.text);
                }
            } else if ends_line || !piece.is_empty() {
                self.push(Line {
                    time: SystemTime::now(),
                    text: piece.to_owned(),
                    level: Level::detect(piece),
                    origin: Origin::Source,
                });
            }
            if ends_line && let Some(line) = self.lines.back() {
                self.tally(line.level);
            }
            self.open = !ends_line && !self.lines.is_empty() && (self.open || !piece.is_empty());
        }
    }

    /// Adds a line from archon itself, on a line of its own
    pub fn push_note(&mut self, text: impl Into<String>, level: Level) {
        self.open = false;
        self.carriage_return = false;
        self.tally(level);
        self.push(Line { time: SystemTime::now(), text: text.into(), level, origin: Origin::Archon });
    }

    fn push(&mut self, line: Line) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
            self.evicted += 1;
        }
        self.lines.push_back(line);
    }

    pub fn clear(&mut self) {
        self.evicted += self.lines.len() as u64;
        self.lines.clear();
        self.open = false;
        self.carriage_return = false;
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Line> {
        self.lines.get(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter()
    }

    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Every line ever added, which unlike `len` only grows, even across a clear
    pub fn total(&self) -> u64 {
        self.evicted + self.lines.len() as u64
    }

    pub fn errors(&self) -> u64 {
        self.errors
    }

    pub fn warnings(&self) -> u64 {
        self.warnings
    }

    fn tally(&mut self, level: Level) {
        match level {
            Level::Error => self.errors += 1,
            Level::Warn => self.warnings += 1,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(buffer: &LineBuffer) -> Vec<&str> {
        buffer.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn joins_lines_split_across_reads() {
        let mut b = LineBuffer::new(10);
        b.push_output("hel");
        b.push_output("lo\nwor");
        b.push_output("ld\r\n");
        assert_eq!(texts(&b), ["hello", "world"]);
        b.push_output("next");
        assert_eq!(texts(&b), ["hello", "world", "next"]);
    }

    #[test]
    fn empty_lines_are_kept() {
        let mut b = LineBuffer::new(10);
        b.push_output("a\n\nb\n");
        assert_eq!(texts(&b), ["a", "", "b"]);
    }

    #[test]
    fn carriage_return_overwrites() {
        let mut b = LineBuffer::new(10);
        b.push_output("10%");
        b.push_output("\r50%");
        b.push_output("\r100%\n");
        assert_eq!(texts(&b), ["100%"]);
    }

    #[test]
    fn carriage_return_at_end_of_read_overwrites() {
        let mut b = LineBuffer::new(10);
        b.push_output("10%\r");
        b.push_output("50%\r");
        b.push_output("100%\n");
        assert_eq!(texts(&b), ["100%"]);
    }

    #[test]
    fn crlf_split_across_reads() {
        let mut b = LineBuffer::new(10);
        b.push_output("a\r");
        b.push_output("\nb\n");
        assert_eq!(texts(&b), ["a", "b"]);
    }

    #[test]
    fn note_closes_open_line() {
        let mut b = LineBuffer::new(10);
        b.push_output("partial");
        b.push_note("exited", Level::Info);
        b.push_output("more\n");
        assert_eq!(texts(&b), ["partial", "exited", "more"]);
    }

    #[test]
    fn counts_completed_errors_and_warnings() {
        let mut b = LineBuffer::new(10);
        b.push_output("[ERR");
        assert_eq!(b.errors(), 0, "not counted while the line is open");
        b.push_output("OR] x\n[WARN] y\nplain\n");
        b.push_note("exited with code 1", Level::Warn);
        b.clear();
        assert_eq!((b.errors(), b.warnings()), (1, 2), "counts survive a clear");
        assert_eq!(b.total(), 4);
    }

    #[test]
    fn evicts_oldest() {
        let mut b = LineBuffer::new(2);
        b.push_output("1\n2\n3\n");
        assert_eq!(texts(&b), ["2", "3"]);
        assert_eq!(b.evicted(), 1);
    }

    #[test]
    fn detects_ros_levels() {
        assert_eq!(Level::detect("[WARN] [1699.1] [thrusters]: no heartbeat"), Level::Warn);
        assert_eq!(Level::detect("[INFO] [1699.1] [node]: ERROR count 0"), Level::Info);
        assert_eq!(Level::detect("just text"), Level::Plain);
    }

    #[test]
    fn level_detected_once_open_line_completes() {
        let mut b = LineBuffer::new(10);
        b.push_output("[ER");
        b.push_output("ROR] bad\n");
        assert_eq!(b.get(0).unwrap().level, Level::Error);
    }
}
