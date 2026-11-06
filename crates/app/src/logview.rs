//! A scrolling log: one source's output, or every source's merged in time order. Follows new output, filters with
//! highlighted matches, jumps between errors, and copies what's shown.

use std::time::{SystemTime, UNIX_EPOCH};

use archon_core::line::{Level, Line, LineBuffer, Origin};
use archon_core::source::{SourceId, SourceKind, Status};
use archon_sources::{Entry, Timeline};
use egui::text::LayoutJob;
use egui::{Color32, FontId, Label, RichText, ScrollArea, TextFormat, TextStyle, Ui, Visuals};

/// Lines a log view can show, by absolute row number
pub trait Rows {
    fn first(&self) -> u64;
    fn end(&self) -> u64;
    /// `None` for a row that's gone (cleared) or not shown here
    fn line(&self, row: u64) -> Option<Row<'_>>;
    /// Changes when rows already looked at may have gone, so a filtered list starts over
    fn generation(&self) -> u64;
    /// Whether rows between `first` and `end` can be missing, so the view must keep a list of the ones to show
    fn sparse(&self) -> bool;
}

pub struct Row<'a> {
    pub line: &'a Line,
    /// Which source wrote it, in the merged view
    pub source: Option<(SourceId, &'a str)>,
}

impl Rows for LineBuffer {
    fn first(&self) -> u64 {
        self.evicted()
    }
    fn end(&self) -> u64 {
        self.total()
    }
    fn line(&self, row: u64) -> Option<Row<'_>> {
        self.get_abs(row).map(|line| Row { line, source: None })
    }
    fn generation(&self) -> u64 {
        0
    }
    fn sparse(&self) -> bool {
        false
    }
}

/// Every source's lines in the order they arrived
pub struct Merged<'a> {
    pub timeline: &'a Timeline,
    pub entries: &'a [Entry],
}

impl Rows for Merged<'_> {
    fn first(&self) -> u64 {
        self.timeline.first()
    }
    fn end(&self) -> u64 {
        self.timeline.end()
    }
    fn line(&self, row: u64) -> Option<Row<'_>> {
        let (id, source_row) = self.timeline.get(row)?;
        let entry = self.entries.get(id).filter(|e| e.in_merged_logs() && !e.removed)?;
        entry.buffer.get_abs(source_row).map(|line| Row { line, source: Some((id, entry.name.as_str())) })
    }
    fn generation(&self) -> u64 {
        self.timeline.clears
    }
    fn sparse(&self) -> bool {
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TopicMode {
    #[default]
    Inspector,
    Stream,
    Bytes,
}

impl TopicMode {
    /// What `v` goes to next
    pub fn next(self) -> Self {
        match self {
            TopicMode::Inspector => TopicMode::Stream,
            TopicMode::Stream => TopicMode::Bytes,
            TopicMode::Bytes => TopicMode::Inspector,
        }
    }
}

pub enum Scroll {
    Lines(f32),
    Pages(f32),
    Top,
    Bottom,
    /// Put this row (an index into what's shown) a third of the way down
    Row(usize),
}

/// How a log is being viewed: each source has one, and so does the merged log
pub struct ViewState {
    /// Stay at the end as output arrives
    pub follow: bool,
    /// Only show lines containing this, ignoring case unless it has a capital
    pub filter: String,
    /// For a topic: the inspector, the stream, or the bytes
    pub mode: TopicMode,
    listed: Listed,
    scroll: Option<Scroll>,
    offset: f32,
    row_height: f32,
    /// The line an error jump landed on, by absolute number, drawn highlighted
    marked: Option<u64>,
}

/// The rows to show when not all of them are, kept up to date as output arrives instead of rescanning every frame
#[derive(Default)]
struct Listed {
    filter: String,
    generation: u64,
    rows: Vec<u64>,
    /// The absolute number of the first row not yet looked at
    next: u64,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            follow: true,
            filter: String::new(),
            mode: TopicMode::Inspector,
            listed: Listed::default(),
            scroll: None,
            offset: 0.0,
            row_height: 1.0,
            marked: None,
        }
    }
}

impl ViewState {
    pub fn scroll(&mut self, scroll: Scroll) {
        self.follow = matches!(scroll, Scroll::Bottom);
        if self.follow {
            self.marked = None;
        }
        self.scroll = Some(scroll);
    }

    pub fn toggle_follow(&mut self) {
        if self.follow {
            self.follow = false;
        } else {
            self.scroll(Scroll::Bottom);
        }
    }

    /// Whether rows go through the list: when filtering, or when the rows have gaps
    fn listing(&self, rows: &dyn Rows) -> bool {
        !self.filter.is_empty() || rows.sparse()
    }

    fn update_list(&mut self, rows: &dyn Rows) {
        let listed = &mut self.listed;
        let (first, end) = (rows.first(), rows.end());
        if listed.filter != self.filter || listed.generation != rows.generation() {
            listed.filter.clone_from(&self.filter);
            listed.generation = rows.generation();
            listed.rows.clear();
            listed.next = first;
        }
        // Look at the last row again, since it may have grown since
        let start = listed.next.saturating_sub(1).max(first);
        listed.rows.retain(|&row| row >= first && row < start);
        for row in start..end {
            if rows
                .line(row)
                .is_some_and(|r| find(&r.line.text, &self.filter).next().is_some() || self.filter.is_empty())
            {
                listed.rows.push(row);
            }
        }
        listed.next = end;
    }

    /// The absolute numbers of the rows shown, in order
    fn shown(&mut self, rows: &dyn Rows) -> Vec<u64> {
        if self.listing(rows) {
            self.update_list(rows);
            self.listed.rows.clone()
        } else {
            (rows.first()..rows.end()).collect()
        }
    }

    /// Moves to the next (or previous) error or warning after the last one jumped to, or after the top of the view.
    /// Returns false when there isn't one.
    pub fn jump_to_problem(&mut self, rows: &dyn Rows, forward: bool) -> bool {
        let shown = self.shown(rows);
        let top = (self.offset / self.row_height).floor() as usize;
        let from = self.marked.and_then(|m| shown.binary_search(&m).ok()).unwrap_or(top);
        let is_problem =
            |row: &u64| rows.line(*row).is_some_and(|r| matches!(r.line.level, Level::Error | Level::Warn));
        let found = if forward {
            shown.iter().enumerate().skip(from + usize::from(self.marked.is_some())).find(|(_, row)| is_problem(row))
        } else {
            shown.iter().enumerate().take(from).rev().find(|(_, row)| is_problem(row))
        };
        let Some((index, &row)) = found else { return false };
        self.marked = Some(row);
        self.scroll(Scroll::Row(index));
        true
    }

    /// The lines shown, as text, for copying. Merged lines carry their source's name.
    pub fn shown_text(&mut self, rows: &dyn Rows) -> (String, usize) {
        let shown = self.shown(rows);
        let lines: Vec<String> = shown
            .iter()
            .filter_map(|&row| rows.line(row))
            .map(|r| match r.source {
                Some((_, name)) => format!("{name}: {}", r.line.text),
                None => r.line.text.clone(),
            })
            .collect();
        let count = lines.len();
        (lines.join("\n"), count)
    }
}

/// Where `needle` occurs in `text`, ignoring ASCII case unless the needle has a capital
pub fn find<'a>(text: &'a str, needle: &'a str) -> impl Iterator<Item = std::ops::Range<usize>> + 'a {
    let ignore_case = !needle.chars().any(char::is_uppercase);
    let (hay, pin) = (text.as_bytes(), needle.as_bytes());
    let mut at = 0;
    std::iter::from_fn(move || {
        if pin.is_empty() {
            return None;
        }
        while at + pin.len() <= hay.len() {
            let window = &hay[at..at + pin.len()];
            let hit = if ignore_case { window.eq_ignore_ascii_case(pin) } else { window == pin };
            // Only stop on a char boundary, so the range can slice the text
            if hit && text.is_char_boundary(at) && text.is_char_boundary(at + pin.len()) {
                let range = at..at + pin.len();
                at += pin.len();
                return Some(range);
            }
            at += 1;
        }
        None
    })
}

pub fn status_text(status: &Status) -> String {
    match status {
        Status::Stopped => "stopped".to_owned(),
        Status::Running => "running".to_owned(),
        Status::Waiting(why) => format!("waiting: {why}"),
        Status::Exited(Some(code)) => format!("exited {code}"),
        Status::Exited(None) => "killed".to_owned(),
        Status::Failed(why) => format!("failed: {why}"),
    }
}

pub fn status_color(status: &Status, visuals: &Visuals) -> Color32 {
    match status {
        Status::Running => Color32::from_rgb(80, 200, 120),
        Status::Waiting(_) => visuals.warn_fg_color,
        Status::Exited(Some(0)) | Status::Stopped => visuals.weak_text_color(),
        Status::Exited(_) | Status::Failed(_) => visuals.error_fg_color,
    }
}

pub fn kind_tag(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Process => "proc",
        SourceKind::Serial => "serial",
        SourceKind::Topic => "topic",
        SourceKind::Can => "can",
    }
}

/// The default fonts have no circle glyph, so it's drawn
pub fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// A colour per source, so its lines are easy to pick out in the merged log
pub fn source_color(id: SourceId) -> Color32 {
    const COLORS: [Color32; 6] = [
        Color32::from_rgb(110, 170, 225),
        Color32::from_rgb(190, 145, 225),
        Color32::from_rgb(110, 195, 160),
        Color32::from_rgb(225, 165, 110),
        Color32::from_rgb(220, 125, 160),
        Color32::from_rgb(170, 190, 100),
    ];
    COLORS[id % COLORS.len()]
}

fn level_color(level: Level, visuals: &Visuals) -> Color32 {
    match level {
        Level::Error => visuals.error_fg_color,
        Level::Warn => visuals.warn_fg_color,
        Level::Debug => visuals.weak_text_color(),
        Level::Info | Level::Plain => visuals.text_color(),
    }
}

/// Wall-clock time of day, `HH:MM:SS.mmm`
fn clock(time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since_epoch.as_secs() as libc::time_t;
    // SAFETY: localtime_r only writes the tm it's given
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        tm
    };
    format!("{:02}:{:02}:{:02}.{:03}", tm.tm_hour, tm.tm_min, tm.tm_sec, since_epoch.subsec_millis())
}

/// A local date and time for file names, `20261106-021530`
pub fn file_stamp(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as libc::time_t;
    // SAFETY: localtime_r only writes the tm it's given
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        tm
    };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// The right end of a log's header: line counts, the filter, and whether it follows
fn header_state(ui: &mut Ui, view: &ViewState, total: usize) {
    let visuals = ui.visuals().clone();
    let (mode, color) =
        if view.follow { ("FOLLOW", visuals.weak_text_color()) } else { ("PAUSED", visuals.warn_fg_color) };
    ui.label(RichText::new(mode).small().color(color));
    if view.filter.is_empty() {
        ui.label(RichText::new(format!("{total} lines")).small().weak());
    } else {
        let shown = view.listed.rows.len();
        ui.label(RichText::new(format!("/{}  {shown} of {total}", view.filter)).small());
    }
}

pub fn source_header(ui: &mut Ui, entry: &Entry, view: &ViewState) {
    let visuals = ui.visuals().clone();
    ui.horizontal(|ui| {
        status_dot(ui, status_color(&entry.status, &visuals));
        ui.label(RichText::new(&entry.name).strong());
        ui.label(RichText::new(status_text(&entry.status)).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            header_state(ui, view, entry.buffer.len());
            // Last, so it gets whatever width is left and is cut short rather than pushing into the other pane
            ui.add(Label::new(RichText::new(&entry.description).weak().monospace()).truncate())
                .on_hover_text(&entry.description);
        });
    });
}

pub fn merged_header(ui: &mut Ui, view: &ViewState, total: usize) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("All logs").strong());
        ui.label(RichText::new("every source, in the order lines arrived").weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| header_state(ui, view, total));
    });
}

/// The lines themselves. `pane` keeps each pane's scrolling apart.
pub fn lines(ui: &mut Ui, pane: usize, rows: &dyn Rows, view: &mut ViewState, timestamps: bool) {
    let listing = view.listing(rows);
    if listing {
        view.update_list(rows);
    }
    let count = if listing { view.listed.rows.len() } else { (rows.end() - rows.first()) as usize };

    ui.spacing_mut().item_spacing.y = 0.0;
    let font = TextStyle::Monospace.resolve(ui.style());
    let row_height = ui.fonts_mut(|f| f.row_height(&font));
    view.row_height = row_height;
    let viewport = ui.available_height();

    let mut area = ScrollArea::both().id_salt(("log", pane)).auto_shrink(false).stick_to_bottom(view.follow);
    let scripted = view.scroll.take().map(|scroll| match scroll {
        Scroll::Lines(n) => view.offset + n * row_height,
        Scroll::Pages(n) => view.offset + n * (viewport - 2.0 * row_height).max(row_height),
        Scroll::Top => 0.0,
        Scroll::Bottom => count as f32 * row_height,
        Scroll::Row(row) => row as f32 * row_height - viewport / 3.0,
    });
    if let Some(offset) = scripted {
        area = area.vertical_scroll_offset(offset.max(0.0));
    }

    let visuals = ui.visuals().clone();
    let name_width = if rows.sparse() { 12 } else { 0 };
    let output = area.show_rows(ui, row_height, count, |ui, range| {
        for i in range {
            let row = if listing { view.listed.rows[i] } else { rows.first() + i as u64 };
            let Some(found) = rows.line(row) else {
                ui.label("");
                continue;
            };
            let job = layout(&found, timestamps, &view.filter, name_width, &font, &visuals);
            let response = ui.add(Label::new(job).extend());
            if view.marked == Some(row) {
                let rect = response.rect.with_max_x(ui.max_rect().right());
                ui.painter().rect_filled(rect, 0.0, visuals.selection.bg_fill.gamma_multiply(0.35));
            }
        }
    });

    // Scrolling by hand to the end resumes following, and scrolling away pauses it
    let offset = output.state.clamped_offset().y;
    let at_bottom = offset + output.inner_rect.height() >= output.content_size.y - row_height * 0.5;
    if scripted.is_none() && (offset - view.offset).abs() > 0.5 {
        view.follow = at_bottom;
    }
    view.offset = offset;
}

fn layout(
    row: &Row<'_>,
    timestamps: bool,
    filter: &str,
    name_width: usize,
    font: &FontId,
    visuals: &Visuals,
) -> LayoutJob {
    let line = row.line;
    let mut job = LayoutJob::default();
    let format = |color: Color32| TextFormat { font_id: font.clone(), color, ..Default::default() };
    if timestamps {
        job.append(&clock(line.time), 0.0, format(visuals.weak_text_color()));
        job.append("  ", 0.0, format(visuals.weak_text_color()));
    }
    if let Some((id, name)) = row.source {
        let name: String = name.chars().take(name_width).collect();
        job.append(&format!("{name:<name_width$}  "), 0.0, format(source_color(id)));
    }
    match line.origin {
        Origin::Source => {
            // Matches of the filter get a background, so the eye finds them in long lines
            let normal = format(level_color(line.level, visuals));
            let hit = TextFormat {
                background: visuals.selection.bg_fill,
                color: visuals.strong_text_color(),
                ..normal.clone()
            };
            let mut done = 0;
            for range in find(&line.text, filter) {
                job.append(&line.text[done..range.start], 0.0, normal.clone());
                job.append(&line.text[range.clone()], 0.0, hit.clone());
                done = range.end;
            }
            job.append(&line.text[done..], 0.0, normal);
        }
        Origin::Archon => {
            let color = match line.level {
                Level::Error | Level::Warn => level_color(line.level, visuals),
                _ => visuals.hyperlink_color,
            };
            let mut archon = format(color);
            archon.italics = true;
            job.append("» ", 0.0, archon.clone());
            job.append(&line.text, 0.0, archon);
        }
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(view: &mut ViewState, rows: &dyn Rows) -> Vec<String> {
        view.shown(rows).iter().filter_map(|&r| rows.line(r)).map(|r| r.line.text.clone()).collect()
    }

    #[test]
    fn filter_follows_new_output_and_eviction() {
        let mut buffer = LineBuffer::new(3);
        let mut view = ViewState { filter: "imu".to_owned(), ..Default::default() };
        buffer.push_output("IMU ok\nthruster\n");
        assert_eq!(listed(&mut view, &buffer), ["IMU ok"]);

        // The open last line grows into a match
        buffer.push_output("im");
        listed(&mut view, &buffer);
        buffer.push_output("u late\n");
        assert_eq!(listed(&mut view, &buffer), ["IMU ok", "imu late"]);

        // "IMU ok" falls out of the 3-line buffer
        buffer.push_output("imu again\n");
        assert_eq!(listed(&mut view, &buffer), ["imu late", "imu again"]);
    }

    #[test]
    fn finds_every_match() {
        let hits: Vec<_> = find("IMU imu Imu", "imu").collect();
        assert_eq!(hits, [0..3, 4..7, 8..11]);
        assert_eq!(find("IMU imu", "IMU").count(), 1);
        assert_eq!(find("°C °C", "°c").count(), 2, "ASCII letters ignore case next to other characters");
        assert_eq!(find("°C", "°C").next(), Some(0..3));
    }

    #[test]
    fn jumps_between_problems() {
        let mut buffer = LineBuffer::new(100);
        buffer.push_output("ok\n[ERROR] a\nok\n[WARN] b\nok\n");
        let mut view = ViewState::default();
        let mut jump = |forward| view.jump_to_problem(&buffer, forward).then(|| view.marked.unwrap());
        assert_eq!(jump(true), Some(1));
        assert_eq!(jump(true), Some(3));
        assert_eq!(jump(true), None, "no more");
        assert_eq!(jump(false), Some(1));
    }

    #[test]
    fn copies_what_is_shown() {
        let mut buffer = LineBuffer::new(100);
        buffer.push_output("imu 1\nesc\nimu 2\n");
        let mut view = ViewState { filter: "imu".to_owned(), ..Default::default() };
        assert_eq!(view.shown_text(&buffer), ("imu 1\nimu 2".to_owned(), 2));
    }

    #[test]
    fn capital_in_filter_matches_case() {
        let mut buffer = LineBuffer::new(10);
        buffer.push_output("IMU\nimu\n");
        let mut view = ViewState { filter: "IMU".to_owned(), ..Default::default() };
        assert_eq!(listed(&mut view, &buffer), ["IMU"]);
    }

    #[test]
    fn filter_survives_clear() {
        let mut buffer = LineBuffer::new(10);
        let mut view = ViewState { filter: "a".to_owned(), ..Default::default() };
        buffer.push_output("a1\na2\n");
        listed(&mut view, &buffer);
        buffer.clear();
        buffer.push_output("a3\n");
        assert_eq!(listed(&mut view, &buffer), ["a3"]);
    }
}
