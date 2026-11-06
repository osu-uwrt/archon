//! The scrolling output of one source.

use std::time::{SystemTime, UNIX_EPOCH};

use archon_core::line::{Level, LineBuffer, Origin};
use archon_core::source::{SourceKind, Status};
use archon_sources::Entry;
use egui::text::LayoutJob;
use egui::{Color32, FontId, Label, RichText, ScrollArea, Stroke, TextFormat, TextStyle, Ui, Visuals};

pub enum Scroll {
    Lines(f32),
    Pages(f32),
    Top,
    Bottom,
    /// Put this row (an index into what's shown) a third of the way down
    Row(usize),
}

/// How one source is being viewed. Each source has one, shared by the panes showing it.
pub struct ViewState {
    /// Stay at the end as output arrives
    pub follow: bool,
    /// Only show lines containing this, ignoring case unless it has a capital
    pub filter: String,
    filtered: Filtered,
    scroll: Option<Scroll>,
    offset: f32,
    row_height: f32,
    /// The line an error jump landed on, by absolute number, drawn highlighted
    marked: Option<u64>,
}

/// The lines matching a filter, kept up to date as output arrives instead of rescanning every frame
#[derive(Default)]
struct Filtered {
    filter: String,
    /// Absolute numbers (see [`LineBuffer::evicted`]) of the matching lines
    rows: Vec<u64>,
    /// The absolute number of the first line not yet checked
    next: u64,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            follow: true,
            filter: String::new(),
            filtered: Filtered::default(),
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

    /// The absolute numbers of the lines shown, in order
    fn shown(&mut self, buffer: &LineBuffer) -> Vec<u64> {
        if self.filter.is_empty() {
            (buffer.evicted()..buffer.total()).collect()
        } else {
            self.update_filter(buffer);
            self.filtered.rows.clone()
        }
    }

    /// Moves to the next (or previous) error or warning after the last one jumped to, or after the top of the view.
    /// Returns false when there isn't one.
    pub fn jump_to_problem(&mut self, buffer: &LineBuffer, forward: bool) -> bool {
        let shown = self.shown(buffer);
        let top = (self.offset / self.row_height).floor() as usize;
        let from = self.marked.and_then(|m| shown.binary_search(&m).ok()).unwrap_or(top);
        let is_problem = |row: &u64| {
            let line = buffer.get((row - buffer.evicted()) as usize).expect("shown rows are in the buffer");
            matches!(line.level, Level::Error | Level::Warn)
        };
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

    /// The lines shown, as text, for copying
    pub fn shown_text(&mut self, buffer: &LineBuffer) -> (String, usize) {
        let shown = self.shown(buffer);
        let text = shown
            .iter()
            .map(|row| {
                buffer.get((row - buffer.evicted()) as usize).expect("shown rows are in the buffer").text.as_str()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (text, shown.len())
    }

    pub fn toggle_follow(&mut self) {
        if self.follow {
            self.follow = false;
        } else {
            self.scroll(Scroll::Bottom);
        }
    }

    fn update_filter(&mut self, buffer: &LineBuffer) {
        let filtered = &mut self.filtered;
        let first = buffer.evicted();
        let end = first + buffer.len() as u64;
        if filtered.filter != self.filter {
            filtered.filter.clone_from(&self.filter);
            filtered.rows.clear();
            filtered.next = first;
        }
        // Check the last line again, since it may have grown since
        let start = filtered.next.saturating_sub(1).max(first);
        filtered.rows.retain(|&row| row >= first && row < start);

        for row in start..end {
            let text = &buffer.get((row - first) as usize).expect("row is in the buffer").text;
            if find(text, &self.filter).next().is_some() {
                filtered.rows.push(row);
            }
        }
        filtered.next = end;
    }
}

/// Where `needle` occurs in `text`, ignoring ASCII case unless the needle has a capital
fn find<'a>(text: &'a str, needle: &'a str) -> impl Iterator<Item = std::ops::Range<usize>> + 'a {
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
    }
}

/// The default fonts have no circle glyph, so it's drawn
pub fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
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

/// Shows the source's header and output. `highlight` marks the active pane when there are two. Returns whether the
/// pane was clicked.
pub fn show(ui: &mut Ui, pane: usize, entry: &Entry, view: &mut ViewState, timestamps: bool, highlight: bool) -> bool {
    let visuals = ui.visuals().clone();
    let stroke = if highlight { Stroke::new(1.0, visuals.selection.stroke.color) } else { Stroke::NONE };
    let frame = egui::Frame::NONE.stroke(stroke).inner_margin(4.0);
    let response = frame.show(ui, |ui| {
        // Long lines scroll sideways inside the pane instead of drawing over its neighbour
        ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
        header(ui, entry, view);
        ui.separator();
        lines(ui, pane, entry, view, timestamps);
    });
    let rect = response.response.rect;
    ui.rect_contains_pointer(rect) && ui.input(|i| i.pointer.any_pressed())
}

fn header(ui: &mut Ui, entry: &Entry, view: &ViewState) {
    let visuals = ui.visuals().clone();
    ui.horizontal(|ui| {
        status_dot(ui, status_color(&entry.status, &visuals));
        ui.label(RichText::new(&entry.name).strong());
        ui.label(RichText::new(status_text(&entry.status)).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (mode, color) =
                if view.follow { ("FOLLOW", visuals.weak_text_color()) } else { ("PAUSED", visuals.warn_fg_color) };
            ui.label(RichText::new(mode).small().color(color));
            if view.filter.is_empty() {
                ui.label(RichText::new(format!("{} lines", entry.buffer.len())).small().weak());
            } else {
                let shown = view.filtered.rows.len();
                ui.label(RichText::new(format!("/{}  {shown} of {}", view.filter, entry.buffer.len())).small());
            }
            // Last, so it gets whatever width is left and is cut short rather than pushing into the other pane
            ui.add(Label::new(RichText::new(&entry.description).weak().monospace()).truncate())
                .on_hover_text(&entry.description);
        });
    });
}

fn lines(ui: &mut Ui, pane: usize, entry: &Entry, view: &mut ViewState, timestamps: bool) {
    let buffer = &entry.buffer;
    let filtering = !view.filter.is_empty();
    if filtering {
        view.update_filter(buffer);
    }
    let count = if filtering { view.filtered.rows.len() } else { buffer.len() };

    ui.spacing_mut().item_spacing.y = 0.0;
    let font = TextStyle::Monospace.resolve(ui.style());
    let row_height = ui.fonts_mut(|f| f.row_height(&font));
    view.row_height = row_height;
    let viewport = ui.available_height();

    let mut area = ScrollArea::both().id_salt(("pane", pane)).auto_shrink(false).stick_to_bottom(view.follow);
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
    let output = area.show_rows(ui, row_height, count, |ui, range| {
        for i in range {
            let index = if filtering { (view.filtered.rows[i] - buffer.evicted()) as usize } else { i };
            let Some(line) = buffer.get(index) else { continue };
            let job = layout(line, timestamps, &view.filter, &font, &visuals);
            let response = ui.add(Label::new(job).extend());
            if view.marked == Some(buffer.evicted() + index as u64) {
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
    line: &archon_core::line::Line,
    timestamps: bool,
    filter: &str,
    font: &FontId,
    visuals: &Visuals,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let format = |color: Color32| TextFormat { font_id: font.clone(), color, ..Default::default() };
    if timestamps {
        job.append(&clock(line.time), 0.0, format(visuals.weak_text_color()));
        job.append("  ", 0.0, format(visuals.weak_text_color()));
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

    fn filtered(view: &ViewState, buffer: &LineBuffer) -> Vec<String> {
        view.filtered
            .rows
            .iter()
            .map(|&row| buffer.get((row - buffer.evicted()) as usize).unwrap().text.clone())
            .collect()
    }

    #[test]
    fn filter_follows_new_output_and_eviction() {
        let mut buffer = LineBuffer::new(3);
        let mut view = ViewState { filter: "imu".to_owned(), ..Default::default() };
        buffer.push_output("IMU ok\nthruster\n");
        view.update_filter(&buffer);
        assert_eq!(filtered(&view, &buffer), ["IMU ok"]);

        // The open last line grows into a match
        buffer.push_output("im");
        view.update_filter(&buffer);
        buffer.push_output("u late\n");
        view.update_filter(&buffer);
        assert_eq!(filtered(&view, &buffer), ["IMU ok", "imu late"]);

        // "IMU ok" falls out of the 3-line buffer
        buffer.push_output("imu again\n");
        view.update_filter(&buffer);
        assert_eq!(filtered(&view, &buffer), ["imu late", "imu again"]);
    }

    #[test]
    fn finds_every_match() {
        let hits: Vec<_> = find("IMU imu Imu", "imu").collect();
        assert_eq!(hits, [0..3, 4..7, 8..11]);
        assert_eq!(find("IMU imu", "IMU").collect::<Vec<_>>().len(), 1);
        assert_eq!(find("IMU imu", "IMU").next(), Some(0..3));
        assert_eq!(find("°C °C", "°c").count(), 2, "ASCII letters ignore case next to other characters");
        assert_eq!(find("°C", "°C").next(), Some(0..3));
    }

    #[test]
    fn jumps_between_problems() {
        let mut buffer = LineBuffer::new(100);
        buffer.push_output("ok\n[ERROR] a\nok\n[WARN] b\nok\n");
        let mut view = ViewState::default();
        let jump = |view: &mut ViewState, forward| view.jump_to_problem(&buffer, forward).then(|| view.marked.unwrap());
        assert_eq!(jump(&mut view, true), Some(1));
        assert_eq!(jump(&mut view, true), Some(3));
        assert_eq!(jump(&mut view, true), None, "no more");
        assert_eq!(view.marked, Some(3), "stays on the last one");
        assert_eq!(jump(&mut view, false), Some(1));
        assert!(!view.follow, "a jump stops following");
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
        view.update_filter(&buffer);
        assert_eq!(filtered(&view, &buffer), ["IMU"]);
    }

    #[test]
    fn filter_survives_clear() {
        let mut buffer = LineBuffer::new(10);
        let mut view = ViewState { filter: "a".to_owned(), ..Default::default() };
        buffer.push_output("a1\na2\n");
        view.update_filter(&buffer);
        buffer.clear();
        buffer.push_output("a3\n");
        view.update_filter(&buffer);
        assert_eq!(filtered(&view, &buffer), ["a3"]);
    }
}
