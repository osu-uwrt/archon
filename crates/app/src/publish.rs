//! Publishing: a form for one message of any type, sent once (Ctrl+S) or repeatedly at a rate (Ctrl+R). The
//! profile's `[[publish]]` actions are publishers made ready ahead of time, and the form can save one into the profile.

use std::time::{Duration, Instant};

use archon_msg::edit::{default_value, encode, set_field};
use archon_msg::{Registry, TypeName, Value};
use egui::{Key, RichText, ScrollArea, TextEdit, Ui};

use crate::inspector::value_text;

pub struct Publisher {
    pub name: String,
    pub topic: String,
    pub ty: String,
    /// The message being built, or why there can't be one
    pub value: Result<Value, String>,
    pub rate_text: String,
    /// A profile action with a rate: running it starts or stops sending, rather than sending once
    pub repeats: bool,
    pub repeating: bool,
    pub sent: u64,
    pub last_sent: Option<Instant>,
    pub cursor: usize,
    /// The field being typed into: its path and text
    editing: Option<(String, String)>,
    /// The last edit that didn't fit its field: the path and why
    field_error: Option<(String, String)>,
    /// Whether it's a `[[publish]]` in the profile
    pub in_profile: bool,
    pub key: Option<String>,
    /// Taken off the bench; it keeps its place so the others keep their numbers
    pub removed: bool,
}

/// One line of the form
pub struct Row {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub value: Value,
    /// Messages and arrays of messages aren't typed into; their fields are
    pub editable: bool,
}

pub enum Action {
    Send,
    ToggleRepeat,
    SaveToProfile,
}

impl Publisher {
    pub fn new(registry: &mut Registry, name: &str, topic: &str, ty: &str) -> Self {
        let value = TypeName::parse(ty).ok_or_else(|| format!("{ty} isn't a message type")).and_then(|type_name| {
            registry.load(&type_name).map_err(|e| format!("{e:#}"))?;
            default_value(registry, &type_name).map_err(|e| format!("{e:#}"))
        });
        Self {
            name: name.to_owned(),
            topic: topic.to_owned(),
            ty: ty.to_owned(),
            value,
            rate_text: "10".to_owned(),
            repeats: false,
            repeating: false,
            sent: 0,
            last_sent: None,
            cursor: 0,
            editing: None,
            field_error: None,
            in_profile: false,
            key: None,
            removed: false,
        }
    }

    /// Puts every field back to its default
    pub fn reset(&mut self, registry: &mut Registry) {
        let fresh = Self::new(registry, &self.name, &self.topic, &self.ty);
        self.value = fresh.value;
        self.editing = None;
        self.field_error = None;
    }

    fn type_name(&self) -> TypeName {
        TypeName::parse(&self.ty).expect("checked in new")
    }

    /// Sets one field from text, as typed in the form
    pub fn set(&mut self, registry: &Registry, path: &str, text: &str) -> Result<(), String> {
        let type_name = self.type_name();
        let value = self.value.as_mut().map_err(|e| e.clone())?;
        set_field(registry, &type_name, value, path, text).map_err(|e| format!("{e:#}"))
    }

    /// Sets the fields in a profile's `value` table
    pub fn apply_table(&mut self, registry: &Registry, table: &toml::Table) -> Result<(), String> {
        let mut fields = Vec::new();
        flatten_toml("", &toml::Value::Table(table.clone()), &mut fields);
        fields.iter().try_for_each(|(path, text)| self.set(registry, path, text))
    }

    pub fn encode(&self, registry: &Registry) -> Result<Vec<u8>, String> {
        let value = self.value.as_ref().map_err(|e| e.clone())?;
        encode(registry, &self.type_name(), value).map_err(|e| format!("{e:#}"))
    }

    pub fn rate(&self) -> Option<f64> {
        self.rate_text.trim().parse::<f64>().ok().filter(|r| *r > 0.0 && r.is_finite())
    }

    /// Whether a repeating publisher is due to send again
    pub fn due(&self) -> Option<Duration> {
        let period = Duration::from_secs_f64(1.0 / self.rate()?);
        let since = self.last_sent.map_or(period, |t| t.elapsed());
        Some(period.saturating_sub(since))
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Ok(Value::Struct(fields)) = &self.value {
            for (name, v) in fields {
                walk(name, name, 0, v, &mut rows);
            }
        }
        rows
    }

    pub fn move_cursor(&mut self, by: isize) {
        let rows = self.rows().len();
        if rows > 0 {
            self.cursor = self.cursor.saturating_add_signed(by).min(rows - 1);
        }
    }

    /// Starts typing into the field at the cursor, or flips it if it's a bool
    pub fn edit_at_cursor(&mut self, registry: &Registry) {
        let rows = self.rows();
        let Some(row) = rows.get(self.cursor).filter(|r| r.editable) else { return };
        match row.value {
            Value::Bool(b) => {
                let _ = self.set(registry, &row.path, if b { "false" } else { "true" });
            }
            _ => self.editing = Some((row.path.clone(), plain(&row.value))),
        }
    }

    /// The profile entry for this publisher, to append to the profile file
    pub fn profile_entry(&self) -> Result<String, String> {
        #[derive(serde::Serialize)]
        struct Entry<'a> {
            name: &'a str,
            topic: &'a str,
            #[serde(rename = "type")]
            ty: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            rate: Option<f64>,
            value: toml::Value,
        }
        #[derive(serde::Serialize)]
        struct File<'a> {
            publish: [Entry<'a>; 1],
        }
        let value = self.value.as_ref().map_err(|e| e.clone())?;
        let rate = self.repeating.then(|| self.rate()).flatten();
        let entry = Entry { name: &self.name, topic: &self.topic, ty: &self.ty, rate, value: to_toml(value) };
        toml::to_string(&File { publish: [entry] }).map_err(|e| e.to_string())
    }
}

fn walk(path: &str, name: &str, depth: usize, value: &Value, out: &mut Vec<Row>) {
    let editable = match value {
        Value::Struct(_) => false,
        Value::Array(items) => !items.iter().any(|v| matches!(v, Value::Struct(_))),
        _ => true,
    };
    out.push(Row { path: path.to_owned(), name: name.to_owned(), depth, value: value.clone(), editable });
    match value {
        Value::Struct(fields) => {
            for (n, v) in fields {
                walk(&format!("{path}.{n}"), n, depth + 1, v, out);
            }
        }
        Value::Array(items) if !editable => {
            for (i, v) in items.iter().enumerate() {
                walk(&format!("{path}[{i}]"), &format!("[{i}]"), depth + 1, v, out);
            }
        }
        _ => {}
    }
}

/// A value as it's typed: strings without quotes, arrays as `[1, 2]`
fn plain(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bytes(bytes) => format!("[{}]", bytes.iter().map(u8::to_string).collect::<Vec<_>>().join(", ")),
        other => value_text(other, None),
    }
}

/// Paths and texts for every value in a TOML table shaped like a message
fn flatten_toml(prefix: &str, value: &toml::Value, out: &mut Vec<(String, String)>) {
    let join = |name: &str| if prefix.is_empty() { name.to_owned() } else { format!("{prefix}.{name}") };
    match value {
        toml::Value::Table(table) => table.iter().for_each(|(k, v)| flatten_toml(&join(k), v, out)),
        toml::Value::Array(items) if items.iter().any(|v| v.is_table()) => {
            for (i, item) in items.iter().enumerate() {
                flatten_toml(&format!("{prefix}[{i}]"), item, out);
            }
        }
        toml::Value::Array(items) => {
            let texts: Vec<String> = items.iter().map(scalar_text).collect();
            out.push((prefix.to_owned(), format!("[{}]", texts.join(", "))));
        }
        scalar => out.push((prefix.to_owned(), scalar_text(scalar))),
    }
}

fn scalar_text(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn to_toml(value: &Value) -> toml::Value {
    match value {
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Int(i) => toml::Value::Integer(*i),
        Value::UInt(u) => toml::Value::Integer(*u as i64),
        Value::Float32(f) => toml::Value::Float(f64::from(*f)),
        Value::Float64(f) => toml::Value::Float(*f),
        Value::String(s) => toml::Value::String(s.clone()),
        Value::Bytes(b) => toml::Value::Array(b.iter().map(|x| toml::Value::Integer(i64::from(*x))).collect()),
        Value::Array(items) => toml::Value::Array(items.iter().map(to_toml).collect()),
        Value::Struct(fields) => toml::Value::Table(fields.iter().map(|(n, v)| (n.clone(), to_toml(v))).collect()),
    }
}

/// `keys` names the keys for the hints: send, repeat
pub fn show(
    ui: &mut Ui,
    pane: usize,
    publisher: &mut Publisher,
    registry: &Registry,
    keys: (&str, &str),
) -> Option<Action> {
    let visuals = ui.visuals().clone();
    let mut action = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Publish {}", publisher.name)).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let last = match publisher.last_sent {
                Some(t) => format!("sent {} · last {:.1} s ago", publisher.sent, t.elapsed().as_secs_f32()),
                None => "not sent yet".to_owned(),
            };
            ui.label(RichText::new(last).small().weak());
            let what = format!("{} {}", publisher.topic, publisher.ty);
            ui.add(egui::Label::new(RichText::new(what).weak().monospace()).truncate());
        });
    });
    ui.separator();

    // The buttons, at the top so a long message doesn't push them away
    ui.horizontal(|ui| {
        if ui.button(format!("Send  {}", keys.0)).clicked() {
            action = Some(Action::Send);
        }
        ui.add_space(8.0);
        let repeat = if publisher.repeating { "Stop" } else { "Repeat" };
        if ui.button(format!("{repeat}  {}", keys.1)).clicked() {
            action = Some(Action::ToggleRepeat);
        }
        ui.label("at");
        let rate_ok = publisher.rate().is_some();
        let edit = TextEdit::singleline(&mut publisher.rate_text).desired_width(48.0);
        let edit = if rate_ok { edit } else { edit.text_color(visuals.error_fg_color) };
        ui.add(edit);
        ui.label("Hz");
        if publisher.repeating {
            ui.label(RichText::new("sending").color(visuals.warn_fg_color));
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if publisher.in_profile {
                let key = publisher.key.as_deref().map(|k| format!(" · key {k}")).unwrap_or_default();
                ui.label(RichText::new(format!("in the profile{key}")).small().weak());
            } else if ui.button("Save to profile").clicked() {
                action = Some(Action::SaveToProfile);
            }
        });
    });
    ui.separator();

    let rows = match &publisher.value {
        Ok(_) => publisher.rows(),
        Err(why) => {
            ui.label(RichText::new(why).color(visuals.error_fg_color));
            return action;
        }
    };
    publisher.cursor = publisher.cursor.min(rows.len().saturating_sub(1));
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let name_width =
        rows.iter().map(|r| r.depth as f32 * 14.0 + r.name.len() as f32 * font.size * 0.62).fold(0.0, f32::max) + 24.0;
    let name_width = name_width.min(ui.available_width() * 0.5);

    let mut commit: Option<(String, String)> = None;
    let mut cancel = false;
    let mut start: Option<(usize, String, Value)> = None;
    ScrollArea::vertical().id_salt(("publish", pane)).auto_shrink(false).show(ui, |ui| {
        for (i, row) in rows.iter().enumerate() {
            let selected = i == publisher.cursor;
            let frame = egui::Frame::NONE
                .fill(if selected {
                    visuals.selection.bg_fill.gamma_multiply(0.35)
                } else {
                    egui::Color32::TRANSPARENT
                })
                .inner_margin(egui::Margin::symmetric(4, 2));
            frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(row.depth as f32 * 14.0);
                    let name = ui.add_sized(
                        [name_width - row.depth as f32 * 14.0, 18.0],
                        egui::Label::new(RichText::new(&row.name).monospace()).truncate().sense(egui::Sense::click()),
                    );
                    if name.clicked() {
                        publisher.cursor = i;
                    }
                    let editing_here = publisher.editing.as_ref().is_some_and(|(p, _)| *p == row.path);
                    if editing_here {
                        let (_, text) = publisher.editing.as_mut().expect("checked");
                        let response = ui.add(
                            TextEdit::singleline(text).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY),
                        );
                        if !response.has_focus() && !response.lost_focus() {
                            response.request_focus();
                        }
                        if response.lost_focus() {
                            if ui.input(|i| i.key_pressed(Key::Escape)) {
                                cancel = true;
                            } else {
                                commit = Some((row.path.clone(), text.clone()));
                            }
                        }
                    } else if !row.editable {
                        ui.label(RichText::new(value_text(&row.value, Some(true))).weak().monospace());
                    } else if let Value::Bool(b) = row.value {
                        let mut checked = b;
                        if ui.checkbox(&mut checked, RichText::new(b.to_string()).monospace()).changed() {
                            commit = Some((row.path.clone(), checked.to_string()));
                            publisher.cursor = i;
                        }
                    } else {
                        let text = RichText::new(value_text(&row.value, None)).monospace();
                        let response = ui
                            .add(egui::Label::new(text).sense(egui::Sense::click()))
                            .on_hover_text("click or Enter to change");
                        if response.clicked() {
                            start = Some((i, row.path.clone(), row.value.clone()));
                        }
                    }
                });
                if let Some((path, why)) = &publisher.field_error
                    && *path == row.path
                {
                    ui.label(RichText::new(why).small().color(visuals.error_fg_color));
                }
            });
        }
    });

    if let Some((i, path, value)) = start {
        publisher.cursor = i;
        publisher.editing = Some((path, plain(&value)));
    }
    if cancel {
        publisher.editing = None;
        publisher.field_error = None;
    }
    if let Some((path, text)) = commit {
        match publisher.set(registry, &path, &text) {
            Ok(()) => {
                publisher.editing = None;
                publisher.field_error = None;
            }
            // Keep typing until it fits
            Err(why) => publisher.field_error = Some((path, why)),
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with(defs: &[(&str, &str)]) -> (Registry, std::path::PathBuf) {
        // Each test its own directory, since they run at once and each removes its own
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("archon-publish-test-{}-{n}", std::process::id()));
        for (name, text) in defs {
            let (pkg, ty) = name.split_once('/').unwrap();
            std::fs::create_dir_all(dir.join(pkg).join("msg")).unwrap();
            std::fs::write(dir.join(pkg).join("msg").join(format!("{ty}.msg")), text).unwrap();
        }
        (Registry::new(vec![dir.clone()]), dir)
    }

    #[test]
    fn profile_values_fill_the_message_and_save_back() {
        let (mut registry, dir) = registry_with(&[
            ("demo/Drive", "demo/Vec linear\nfloat32[2] gains\nbool armed\nstring mode\n"),
            ("demo/Vec", "float64 x\nfloat64 y\n"),
        ]);
        let mut publisher = Publisher::new(&mut registry, "drive", "/drive", "demo/msg/Drive");
        let table: toml::Table =
            toml::from_str("linear = { x = 0.5 }\ngains = [1, 2]\narmed = true\nmode = \"auto\"").unwrap();
        publisher.apply_table(&registry, &table).unwrap();
        let value = publisher.value.as_ref().unwrap();
        assert_eq!(value.get("linear.x").and_then(Value::as_f64), Some(0.5));
        assert_eq!(value.get("gains[1]").and_then(Value::as_f64), Some(2.0));
        assert_eq!(value.get("mode"), Some(&Value::String("auto".into())));

        // What's saved to the profile reads back as the same message
        let entry = publisher.profile_entry().unwrap();
        let file: toml::Table = toml::from_str(&entry).unwrap();
        let saved = file["publish"][0]["value"].as_table().unwrap().clone();
        let mut again = Publisher::new(&mut registry, "drive", "/drive", "demo/msg/Drive");
        again.apply_table(&registry, &saved).unwrap();
        assert_eq!(again.value, publisher.value);
        assert!(publisher.encode(&registry).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rows_and_bool_toggle() {
        let (mut registry, dir) = registry_with(&[("demo/Kill", "bool data\n")]);
        let mut publisher = Publisher::new(&mut registry, "kill", "/kill", "demo/msg/Kill");
        assert_eq!(publisher.rows().len(), 1);
        publisher.edit_at_cursor(&registry);
        assert_eq!(publisher.value.as_ref().unwrap().get("data"), Some(&Value::Bool(true)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_types_say_why() {
        let (mut registry, dir) = registry_with(&[("demo/Kill", "bool data\n")]);
        let publisher = Publisher::new(&mut registry, "x", "/x", "demo/msg/Nope");
        assert!(publisher.value.unwrap_err().contains("demo/msg/Nope"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rate_and_due() {
        let (mut registry, dir) = registry_with(&[("demo/Kill", "bool data\n")]);
        let mut publisher = Publisher::new(&mut registry, "kill", "/kill", "demo/msg/Kill");
        assert_eq!(publisher.due(), Some(Duration::ZERO), "never sent, so due now");
        publisher.last_sent = Some(Instant::now());
        assert!(publisher.due().unwrap() > Duration::from_millis(50));
        publisher.rate_text = "zero".into();
        assert_eq!(publisher.due(), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
