//! The picker, which works like VS Code's: by default it finds a source by name (and later a topic), and a query
//! starting with `>` finds a command instead. Each command shows its keys, so the picker teaches the keybindings.

use archon_core::command::{COMMANDS, Command};
use archon_core::source::{SourceId, SourceKind, Status};
use archon_ros::Topic;
use archon_sources::Entry;
use egui::text::{CCursor, CCursorRange, LayoutJob};
use egui::{
    Align2, Color32, Context, FontId, Id, Key, Modifiers, Order, Rect, RichText, Sense, TextEdit, TextFormat, Ui, vec2,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::keymap::{Keymap, format_sequence};
use crate::layout::View;
use crate::logview::{kind_tag, status_color, status_text};

const VISIBLE_ROWS: usize = 12;
const COMMAND_PREFIX: char = '>';

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Choice {
    Command(Command),
    /// `beside` opens it in the other pane
    Source {
        id: SourceId,
        beside: bool,
    },
    /// All logs, or the topics table
    View {
        view: View,
        beside: bool,
    },
    /// Publish to a topic: open the form for it
    Publish {
        topic: String,
        ty: String,
    },
    /// A publish action from the profile: `run` sends it, else its form opens
    Action {
        index: usize,
        run: bool,
    },
    /// A saved layout to load
    Layout(String),
    /// One of a list of options, by its place in the list
    Option(usize),
    /// A topic on the graph without an echo yet
    Topic {
        name: String,
        ty: Option<String>,
        beside: bool,
    },
}

pub enum Outcome {
    Open,
    Closed,
    Chose(Choice),
}

/// A publish action, as the picker lists it
pub struct ActionItem {
    /// Its place among the publishers
    pub index: usize,
    pub name: String,
    pub topic: String,
    pub key: Option<String>,
}

/// One option of a list to pick from
pub struct OptionItem {
    pub label: String,
    pub detail: String,
    pub hint: String,
}

pub struct Picker {
    query: String,
    selected: usize,
    /// Choosing a topic to publish to, rather than going somewhere
    publishing: bool,
    /// Picking a saved layout, from these names
    layouts: Option<Vec<String>>,
    /// Picking one of these, with this prompt
    options: Option<(String, Vec<OptionItem>)>,
    /// Put the text cursor at the end on the first frame, after the `>` a command search starts with
    place_cursor: bool,
    /// Shift+Enter was pressed on something that isn't a topic
    publish_note: bool,
}

struct Item {
    choice: Choice,
    label: String,
    /// Shown dimmed after the label
    detail: String,
    /// Shown at the right: a command's keys, a source's number and status
    hint: String,
    /// A source's status, shown as a dot
    status: Option<Status>,
}

impl Picker {
    pub fn sources() -> Self {
        Self {
            query: String::new(),
            selected: 0,
            publishing: false,
            layouts: None,
            options: None,
            place_cursor: true,
            publish_note: false,
        }
    }

    pub fn commands() -> Self {
        Self {
            query: COMMAND_PREFIX.to_string(),
            selected: 0,
            publishing: false,
            layouts: None,
            options: None,
            place_cursor: true,
            publish_note: false,
        }
    }

    /// Picks a topic or a profile action to publish
    pub fn publish() -> Self {
        Self {
            query: String::new(),
            selected: 0,
            publishing: true,
            layouts: None,
            options: None,
            place_cursor: true,
            publish_note: false,
        }
    }

    /// Picks one of a list
    pub fn options(prompt: &str, items: Vec<OptionItem>) -> Self {
        Self {
            query: String::new(),
            selected: 0,
            publishing: false,
            layouts: None,
            options: Some((prompt.to_owned(), items)),
            place_cursor: true,
            publish_note: false,
        }
    }

    /// Picks one of these saved layouts
    pub fn layouts(names: Vec<String>) -> Self {
        Self {
            query: String::new(),
            selected: 0,
            publishing: false,
            layouts: Some(names),
            options: None,
            place_cursor: true,
            publish_note: false,
        }
    }

    pub fn is_publishing(&self) -> bool {
        self.publishing && !self.is_commands()
    }

    pub fn is_commands(&self) -> bool {
        self.query.starts_with(COMMAND_PREFIX)
    }

    fn search(&self) -> &str {
        self.query.strip_prefix(COMMAND_PREFIX).unwrap_or(&self.query).trim_start()
    }

    /// `recent` lists commands run from here, most recent first; with no query they come first
    pub fn show(
        &mut self,
        ctx: &Context,
        sources: &[Entry],
        topics: &[Topic],
        actions: &[ActionItem],
        keymap: &Keymap,
        recent: &[Command],
    ) -> Outcome {
        let mut choose = None;
        let keys = ctx.input_mut(|i| {
            // Shift+Ctrl+P before Ctrl+P, since a Ctrl+P binding also matches with Shift held
            let commands_key = i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::P);
            let sources_key = i.consume_key(Modifiers::CTRL, Key::P);
            let publish_key = i.consume_key(Modifiers::CTRL, Key::O);
            // Shift+Enter publishes to the topic picked, rather than echoing it
            let publish = i.consume_key(Modifiers::SHIFT, Key::Enter);
            let down = i.consume_key(Modifiers::NONE, Key::ArrowDown)
                | i.consume_key(Modifiers::CTRL, Key::N)
                | i.consume_key(Modifiers::CTRL, Key::J)
                | i.consume_key(Modifiers::NONE, Key::Tab);
            let up = i.consume_key(Modifiers::NONE, Key::ArrowUp)
                | i.consume_key(Modifiers::CTRL, Key::K)
                | i.consume_key(Modifiers::SHIFT, Key::Tab);
            let beside = i.consume_key(Modifiers::CTRL, Key::Enter);
            let enter = i.consume_key(Modifiers::NONE, Key::Enter);
            let escape = i.consume_key(Modifiers::NONE, Key::Escape);
            (commands_key, sources_key, publish_key, down, up, beside, enter, publish, escape)
        });
        let (commands_key, sources_key, publish_key, mut down, up, beside, enter, publish, escape) = keys;
        if escape {
            return Outcome::Closed;
        }
        // The key that opened a mode again moves down the list, as in VS Code; another mode's key switches to it
        let lists = self.options.is_none() && self.layouts.is_none();
        let mode = if self.is_commands() {
            Mode::Commands
        } else if self.publishing {
            Mode::Publish
        } else {
            Mode::Sources
        };
        let wanted = match (commands_key, sources_key, publish_key) {
            (true, _, _) => Some(Mode::Commands),
            (_, true, _) => Some(Mode::Sources),
            (_, _, true) => Some(Mode::Publish),
            _ => None,
        };
        match wanted {
            Some(wanted) if wanted == mode => down = true,
            Some(wanted) if lists => {
                self.publishing = wanted == Mode::Publish;
                self.switch(wanted == Mode::Commands);
            }
            _ => {}
        }

        let items = if self.is_commands() {
            command_items(keymap, recent, actions)
        } else if let Some((_, options)) = &self.options {
            options
                .iter()
                .enumerate()
                .map(|(i, o)| Item {
                    choice: Choice::Option(i),
                    label: o.label.clone(),
                    detail: o.detail.clone(),
                    hint: o.hint.clone(),
                    status: None,
                })
                .collect()
        } else if let Some(names) = &self.layouts {
            names
                .iter()
                .map(|name| Item {
                    choice: Choice::Layout(name.clone()),
                    label: name.clone(),
                    detail: String::new(),
                    hint: "layout".to_owned(),
                    status: None,
                })
                .collect()
        } else if self.publishing {
            publish_items(topics, actions)
        } else {
            source_items(sources, topics, keymap)
        };
        let ranked = rank(&items, self.search());
        if !ranked.is_empty() {
            if down {
                self.selected = (self.selected + 1) % ranked.len();
            }
            if up {
                self.selected = self.selected.checked_sub(1).unwrap_or(ranked.len() - 1);
            }
        }
        self.selected = self.selected.min(ranked.len().saturating_sub(1));
        if publish && let Some((i, _)) = ranked.get(self.selected) {
            match as_publish(&items[*i].choice, sources, topics) {
                Some(choice) => return Outcome::Chose(choice),
                None => self.publish_note = true,
            }
        }
        if enter || beside {
            return match ranked.get(self.selected) {
                Some((i, _)) => Outcome::Chose(with_beside(items[*i].choice.clone(), beside)),
                None => Outcome::Closed,
            };
        }

        let width = (ctx.content_rect().width() - 32.0).clamp(240.0, 640.0);
        egui::Area::new(Id::unique("picker"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 48.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).inner_margin(8.0).show(ui, |ui| {
                    ui.set_width(width);
                    let hint = if self.is_commands() {
                        "Run a command   (Backspace past > to find a source)"
                    } else if let Some((prompt, _)) = &self.options {
                        prompt.as_str()
                    } else if self.layouts.is_some() {
                        "Load a saved layout"
                    } else if self.publishing {
                        "Publish to any topic, or send an action from the profile   (Ctrl+P to go to one)"
                    } else {
                        "Go to a source   (type > for commands, Ctrl+O to publish)"
                    };
                    let mut output =
                        TextEdit::singleline(&mut self.query).hint_text(hint).desired_width(f32::INFINITY).show(ui);
                    output.response.response.request_focus();
                    if std::mem::take(&mut self.place_cursor) {
                        let end = CCursor::new(self.query.chars().count());
                        output.state.cursor.set_char_range(Some(CCursorRange::one(end)));
                        output.state.store(ui.ctx(), output.response.response.id);
                    }
                    if output.response.response.changed() {
                        self.selected = 0;
                    }
                    ui.add_space(4.0);

                    if self.publish_note {
                        ui.label(RichText::new("Shift+Enter publishes to a topic; that one isn't").small().weak());
                    }
                    if ranked.is_empty() {
                        let empty = match (self.is_commands(), sources.is_empty()) {
                            (false, true) => "This profile has no sources",
                            _ => "No matches",
                        };
                        ui.label(RichText::new(empty).weak());
                    }
                    let first = self.selected.saturating_sub(VISIBLE_ROWS - 1);
                    for (row, (i, indices)) in ranked.iter().enumerate().skip(first).take(VISIBLE_ROWS) {
                        let clicked = item_row(ui, &items[*i], indices, row == self.selected);
                        if let Some(ctrl) = clicked {
                            choose = Some(with_beside(items[*i].choice.clone(), ctrl));
                        }
                    }
                    if ranked.len() > VISIBLE_ROWS {
                        ui.label(RichText::new(format!("{} more", ranked.len() - VISIBLE_ROWS)).small().weak());
                    }
                });
            });
        choose.map_or(Outcome::Open, Outcome::Chose)
    }

    /// Starts a fresh search in the other mode, since a source name is no use as a command search
    fn switch(&mut self, to_commands: bool) {
        self.query = if to_commands { COMMAND_PREFIX.to_string() } else { String::new() };
        self.selected = 0;
        self.place_cursor = true;
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Sources,
    Commands,
    Publish,
}

/// What publishing to a picked topic or topic echo opens: its form, when its type is known
fn as_publish(choice: &Choice, sources: &[Entry], topics: &[Topic]) -> Option<Choice> {
    let topic = match choice {
        Choice::Topic { name, .. } => name.clone(),
        Choice::Source { id, .. } if sources[*id].kind == SourceKind::Topic => sources[*id].name.clone(),
        Choice::Publish { .. } | Choice::Action { .. } => return Some(choice.clone()),
        _ => return None,
    };
    let ty = topics.iter().find(|t| t.name == topic)?.types.first()?.clone();
    Some(Choice::Publish { topic, ty })
}

fn with_beside(choice: Choice, beside: bool) -> Choice {
    match choice {
        Choice::Source { id, .. } => Choice::Source { id, beside },
        Choice::Topic { name, ty, .. } => Choice::Topic { name, ty, beside },
        Choice::View { view, .. } => Choice::View { view, beside },
        command => command,
    }
}

/// The views, the sources, then the topics that aren't open as sources yet
fn source_items(sources: &[Entry], topics: &[Topic], keymap: &Keymap) -> Vec<Item> {
    let key = |command| keymap.keys_for(command).first().map(|k| format_sequence(k)).unwrap_or_default();
    let views = [
        (View::AllLogs, "All logs", "every source merged in time order", Command::OpenAllLogs),
        (View::Topics, "Topics", "the table of every topic, with rates", Command::OpenTopics),
    ]
    .map(|(view, label, detail, command)| Item {
        choice: Choice::View { view, beside: false },
        label: label.to_owned(),
        detail: detail.to_owned(),
        hint: key(command),
        status: None,
    });
    let open = |topic: &Topic| sources.iter().any(|s| s.kind == SourceKind::Topic && s.name == topic.name);
    let topics = topics.iter().filter(|t| !open(t)).map(|topic| Item {
        choice: Choice::Topic { name: topic.name.clone(), ty: topic.types.first().cloned(), beside: false },
        label: topic.name.clone(),
        detail: topic.types.join(", "),
        hint: match topic.publishers {
            0 => "no publishers  echo".to_owned(),
            n => format!("{n} pub  echo"),
        },
        status: None,
    });
    let sources = sources
        .iter()
        .enumerate()
        .filter(|(_, entry)| !entry.removed)
        .map(|(id, entry)| Item {
            choice: Choice::Source { id, beside: false },
            label: entry.name.clone(),
            detail: format!("{}  {}", kind_tag(entry.kind), entry.description),
            hint: match id {
                0..9 => format!("{}  {}", status_text(&entry.status), id + 1),
                _ => status_text(&entry.status),
            },
            status: Some(entry.status.clone()),
        })
        .chain(topics);
    views.into_iter().chain(sources).collect()
}

/// The profile's actions, then every topic on the graph
fn publish_items(topics: &[Topic], actions: &[ActionItem]) -> Vec<Item> {
    let actions = actions.iter().map(|action| Item {
        choice: Choice::Action { index: action.index, run: false },
        label: action.name.clone(),
        detail: action.topic.clone(),
        hint: action.key.clone().unwrap_or_else(|| "action".to_owned()),
        status: None,
    });
    let topics = topics.iter().filter_map(|topic| {
        Some(Item {
            choice: Choice::Publish { topic: topic.name.clone(), ty: topic.types.first()?.clone() },
            label: topic.name.clone(),
            detail: topic.types.join(", "),
            hint: format!("{} sub", topic.subscribers),
            status: None,
        })
    });
    actions.chain(topics).collect()
}

fn command_items(keymap: &Keymap, recent: &[Command], actions: &[ActionItem]) -> Vec<Item> {
    let palette = COMMANDS.iter().filter(|info| info.in_palette);
    let infos: Vec<_> =
        recent.iter().map(|c| c.info()).chain(palette.filter(|i| !recent.contains(&i.command))).collect();
    infos
        .into_iter()
        .map(|info| Item {
            choice: Choice::Command(info.command),
            label: info.title.to_owned(),
            detail: if recent.contains(&info.command) { "recent".to_owned() } else { String::new() },
            hint: keymap.keys_for(info.command).first().map(|keys| format_sequence(keys)).unwrap_or_default(),
            status: None,
        })
        .chain(actions.iter().map(|action| Item {
            choice: Choice::Action { index: action.index, run: true },
            label: format!("Send {}", action.name),
            detail: action.topic.clone(),
            hint: action.key.clone().unwrap_or_default(),
            status: None,
        }))
        .collect()
}

/// The items matching `query`, best first, with the positions of the matched characters for highlighting. An empty
/// query keeps the given order.
fn rank(items: &[Item], query: &str) -> Vec<(usize, Vec<u32>)> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize, Vec<u32>)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let mut indices = Vec::new();
            let score = pattern.indices(Utf32Str::new(&item.label, &mut buf), &mut matcher, &mut indices)?;
            indices.sort_unstable();
            indices.dedup();
            Some((score, i, indices))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i, indices)| (i, indices)).collect()
}

/// Draws one row across the whole width. Returns `Some(ctrl_held)` when clicked.
fn item_row(ui: &mut Ui, item: &Item, matched: &[u32], selected: bool) -> Option<bool> {
    let visuals = ui.visuals().clone();
    let height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, 4.0, visuals.selection.bg_fill);
    } else if response.hovered() {
        painter.rect_filled(rect, 4.0, visuals.widgets.hovered.weak_bg_fill);
    }

    let mut x = rect.left() + 8.0;
    if let Some(status) = &item.status {
        painter.circle_filled(egui::pos2(x + 4.0, rect.center().y), 4.0, status_color(status, &visuals));
        x += 16.0;
    }
    let body = egui::TextStyle::Body.resolve(ui.style());
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let strong = if selected { visuals.selection.stroke.color } else { visuals.strong_text_color() };
    let label = highlighted(&item.label, matched, &body, visuals.text_color(), strong);
    let label = ui.fonts_mut(|f| f.layout_job(label));
    let hint = painter.text(
        rect.right_center() - vec2(8.0, 0.0),
        Align2::RIGHT_CENTER,
        &item.hint,
        mono.clone(),
        visuals.weak_text_color(),
    );
    painter.galley(egui::pos2(x, rect.center().y - label.size().y / 2.0), label.clone(), visuals.text_color());

    // The detail gets what's left between the label and the hint, and is cut short to fit
    let detail_left = x + label.size().x + 12.0;
    let detail_room = Rect::from_x_y_ranges(detail_left..=hint.left() - 12.0, rect.y_range());
    if detail_room.width() > 24.0 && !item.detail.is_empty() {
        let mut job = LayoutJob::simple_singleline(item.detail.clone(), mono, visuals.weak_text_color());
        job.wrap.max_width = detail_room.width();
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        let galley = ui.fonts_mut(|f| f.layout_job(job));
        painter.galley(
            egui::pos2(detail_left, rect.center().y - galley.size().y / 2.0),
            galley,
            visuals.weak_text_color(),
        );
    }
    response.clicked().then(|| ui.input(|i| i.modifiers.command))
}

fn highlighted(text: &str, matched: &[u32], font: &FontId, color: Color32, strong: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (i, c) in text.chars().enumerate() {
        let hit = matched.binary_search(&(i as u32)).is_ok();
        let format = TextFormat {
            font_id: font.clone(),
            color: if hit { strong } else { color },
            underline: if hit { egui::Stroke::new(1.0, strong) } else { egui::Stroke::NONE },
            ..Default::default()
        };
        job.append(c.encode_utf8(&mut [0; 4]), 0.0, format);
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(labels: &[&str]) -> Vec<Item> {
        labels
            .iter()
            .map(|label| Item {
                choice: Choice::Command(Command::Quit),
                label: (*label).to_owned(),
                detail: String::new(),
                hint: String::new(),
                status: None,
            })
            .collect()
    }

    #[test]
    fn ranks_fuzzy_matches_first() {
        let items = items(&["Restart source", "Toggle timestamps", "Stop all sources"]);
        assert_eq!(rank(&items, "rst")[0].0, 0);
        assert_eq!(rank(&items, "stamps").iter().map(|r| r.0).collect::<Vec<_>>(), [1]);
        assert_eq!(rank(&items, "").len(), 3);
    }

    #[test]
    fn reports_matched_characters() {
        let items = items(&["agent"]);
        assert_eq!(rank(&items, "agt")[0].1, [0, 1, 4]);
    }

    #[test]
    fn mode_follows_the_prefix() {
        let mut picker = Picker::sources();
        assert!(!picker.is_commands());
        picker.switch(true);
        assert_eq!(picker.query, ">");
        picker.query.push_str(" restart");
        assert_eq!(picker.search(), "restart");
        picker.switch(false);
        assert_eq!(picker.query, "");
    }

    #[test]
    fn recent_commands_come_first_once() {
        let commands = command_items(&Keymap::defaults(), &[Command::Restart], &[]);
        assert_eq!(commands[0].choice, Choice::Command(Command::Restart));
        assert_eq!(commands.iter().filter(|c| c.choice == Choice::Command(Command::Restart)).count(), 1);
    }
}
