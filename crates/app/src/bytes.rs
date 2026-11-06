//! A topic's recent messages as they arrived: a list, and the selected one's bytes in hex beside what they decode to.
//! For when a firmware message doesn't come out the way it should.

use std::time::Instant;

use archon_msg::{Registry, TypeName, cdr};
use archon_ros::{RawMessage, TopicState};
use archon_sources::Entry;
use egui::{RichText, ScrollArea, Sense, Ui, vec2};

/// How far `g g` goes back: every message kept
pub const KEEP: usize = archon_ros::RAW_KEEP;

#[derive(Default)]
pub struct BytesState {
    /// The message picked by its number; `None` follows the newest
    pub selected: Option<u64>,
    reveal: bool,
}

impl BytesState {
    pub fn move_selection(&mut self, topic: &TopicState, by: isize) {
        let Some(newest) = topic.raw.back() else { return };
        let oldest = topic.raw.front().map_or(1, |m| m.number);
        let at = self.selected.unwrap_or(newest.number) as i64 + by as i64;
        let at = at.clamp(oldest as i64, newest.number as i64) as u64;
        // Back at the newest goes back to following
        self.selected = (at != newest.number).then_some(at);
        self.reveal = true;
    }

    pub fn follow(&mut self) {
        self.selected = None;
        self.reveal = true;
    }

    pub fn selected<'a>(&self, topic: &'a TopicState) -> Option<&'a RawMessage> {
        match self.selected {
            Some(n) => topic.raw.iter().find(|m| m.number == n),
            None => topic.raw.back(),
        }
    }
}

/// Bytes as a classic hex dump: offset, sixteen bytes in hex, and the printable ones as text
pub fn hex_dump(bytes: &[u8]) -> String {
    bytes
        .chunks(16)
        .enumerate()
        .map(|(row, chunk)| {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            let (left, right) = hex.split_at(hex.len().min(8));
            let text: String =
                chunk.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' }).collect();
            format!("{:04x}  {:<23}  {:<23}  |{text}|", row * 16, left.join(" "), right.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn show(ui: &mut Ui, pane: usize, entry: &Entry, topic: &TopicState, state: &mut BytesState, registry: &Registry) {
    let visuals = ui.visuals().clone();
    ui.horizontal(|ui| {
        ui.label(RichText::new(&entry.name).strong());
        ui.label(RichText::new("bytes").weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("v inspector · j/k pick · G newest · y copy hex").small().weak());
            let mode = if state.selected.is_some() { "PICKED" } else { "NEWEST" };
            ui.label(RichText::new(mode).small().weak());
        });
    });
    ui.separator();
    if topic.raw.is_empty() {
        ui.label(RichText::new("No messages yet").weak());
        return;
    }

    // The list: newest at the bottom, like a log
    let now = Instant::now();
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
    let list_height = (ui.available_height() * 0.3).max(row_height * 4.0);
    let chosen = state.selected(topic).map(|m| m.number);
    let mut area =
        ScrollArea::vertical().id_salt(("bytes-list", pane)).max_height(list_height).auto_shrink([false, true]);
    if state.selected.is_none() {
        area = area.stick_to_bottom(true);
    }
    if std::mem::take(&mut state.reveal)
        && let Some(n) = chosen
    {
        let index = topic.raw.iter().position(|m| m.number == n).unwrap_or(0);
        area = area.vertical_scroll_offset((index as f32 * row_height - list_height / 2.0).max(0.0));
    }
    area.show_rows(ui, row_height, topic.raw.len(), |ui, range| {
        for message in topic.raw.range(range) {
            let ago = now.saturating_duration_since(message.at).as_secs_f32();
            let status = match &message.error {
                Some(_) => RichText::new("doesn't decode").color(visuals.error_fg_color),
                None => RichText::new("ok").weak(),
            };
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::click());
            if Some(message.number) == chosen {
                ui.painter().rect_filled(rect, 2.0, visuals.selection.bg_fill.gamma_multiply(0.5));
            }
            if response.clicked() {
                state.selected = Some(message.number);
            }
            let mut child = ui.new_child(
                egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.label(
                RichText::new(format!("#{:<6} {ago:>7.2} s ago  {:>6} bytes", message.number, message.size))
                    .monospace(),
            );
            child.label(status);
        }
    });
    ui.separator();

    let Some(message) = state.selected(topic) else { return };
    let decoded = match (&message.error, &topic.ty) {
        (Some(why), _) => Err(why.clone()),
        (None, Some(ty)) => TypeName::parse(ty)
            .ok_or_else(|| format!("{ty} isn't a message type"))
            .and_then(|name| cdr::decode(registry, &name, &message.bytes).map_err(|e| format!("{e:#}")))
            .map(|value| value.to_text()),
        (None, None) => Err("the type isn't known yet".to_owned()),
    };
    let cut = if message.bytes.len() < message.size {
        format!(" (first {} shown)", message.bytes.len())
    } else {
        String::new()
    };
    ui.label(RichText::new(format!("#{}: {} bytes{cut}", message.number, message.size)).strong());
    ui.columns(2, |columns| {
        ScrollArea::both().id_salt(("bytes-hex", pane)).auto_shrink(false).show(&mut columns[0], |ui| {
            ui.add(egui::Label::new(RichText::new(hex_dump(&message.bytes)).monospace()).extend());
        });
        ScrollArea::both().id_salt(("bytes-decoded", pane)).auto_shrink(false).show(
            &mut columns[1],
            |ui| match &decoded {
                Ok(text) => _ = ui.add(egui::Label::new(RichText::new(text).monospace()).extend()),
                Err(why) => {
                    ui.label(RichText::new("Doesn't decode as its type").color(visuals.error_fg_color).strong());
                    ui.label(RichText::new(why).color(visuals.error_fg_color));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("The bytes on the left are what arrived; compare them with the .msg definition.")
                            .small()
                            .weak(),
                    );
                }
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dumps_hex_with_text() {
        let dump = hex_dump(b"\x00\x01\x00\x00\x03\x00\x00\x00hi\x00");
        assert_eq!(dump, "0000  00 01 00 00 03 00 00 00  68 69 00                 |........hi.|");
        assert_eq!(hex_dump(&[0xab; 17]).lines().count(), 2);
    }

    #[test]
    fn picks_and_follows() {
        let mut topic = TopicState::default();
        for _ in 0..5 {
            topic.record(&[1, 2, 3], None);
        }
        let mut state = BytesState::default();
        assert_eq!(state.selected(&topic).unwrap().number, 5);
        state.move_selection(&topic, -2);
        assert_eq!(state.selected, Some(3));
        state.move_selection(&topic, -10);
        assert_eq!(state.selected, Some(1), "stops at the oldest");
        state.move_selection(&topic, 10);
        assert_eq!(state.selected, None, "back at the newest, it follows again");
    }
}
