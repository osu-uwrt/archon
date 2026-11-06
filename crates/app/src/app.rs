//! The window: sidebar, one or two panes of output, the bars along the bottom, and the overlays.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use archon_core::command::{COMMANDS, Command};
use archon_core::profile::{FILE_NAME, Profile};
use archon_core::source::SourceId;
use archon_sources::{Entry, Supervisor};
use egui::{
    Align2, CentralPanel, Color32, Context, Event, Id, Key, Margin, Modifiers, Order, Panel, RichText, Sense, TextEdit,
    Ui, UiBuilder, vec2,
};

use crate::keymap::{Chord, Keymap, Matcher, Next, Outcome, format_sequence};
use crate::picker::{self, Choice, Picker};
use crate::view::{self, Scroll, ViewState, kind_tag, status_color, status_dot, status_text};

/// How long a message in the status bar stays
const FLASH_TIME: Duration = Duration::from_secs(4);

/// Commands whose Ctrl chords still work while typing in the input or filter bar
const WHILE_TYPING: &[Command] = &[Command::QuickOpen, Command::OpenPalette, Command::Quit, Command::ToggleSidebar];

/// Commands run from the picker that it lists first next time
const RECENT_COMMANDS: usize = 5;

pub struct ArchonApp {
    sup: Supervisor,
    keymap: Keymap,
    matcher: Matcher,
    /// One per source
    views: Vec<ViewState>,
    /// How much of each source has been on screen, for the sidebar's unread marks
    seen: Vec<Seen>,
    /// The source each pane shows. The second is only used while split.
    panes: [SourceId; 2],
    active_pane: usize,
    split: bool,
    sidebar: bool,
    /// The sidebar row under the pointer last frame, which shows its buttons
    hovered_row: Option<SourceId>,
    timestamps: bool,
    show_keys: bool,
    picker: Option<Picker>,
    recent: Vec<Command>,
    /// The input bar, while open
    input: Option<String>,
    history: Vec<String>,
    /// Where Up/Down in the input bar is in `history`
    history_at: Option<usize>,
    searching: bool,
    profile_path: Option<PathBuf>,
    /// Problems loading the profile or keymap, shown until dismissed
    problems: Vec<String>,
    flash: Option<Flash>,
}

#[derive(Default, Clone, Copy)]
struct Seen {
    total: u64,
    errors: u64,
    warnings: u64,
}

struct Flash {
    text: String,
    bad: bool,
    at: Instant,
}

/// What a click in the sidebar asks for
enum RowAction {
    Focus,
    Beside,
    Run(Command),
}

impl ArchonApp {
    pub fn new(cc: &eframe::CreationContext<'_>, profile: Profile, keymap: Keymap, problems: Vec<String>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let mut sup = Supervisor::new(&profile, std::sync::Arc::new(move || ctx.request_repaint()));
        sup.start_autostart();
        let sources = sup.entries().len();
        Self {
            sup,
            keymap,
            matcher: Matcher::default(),
            views: (0..sources).map(|_| ViewState::default()).collect(),
            seen: vec![Seen::default(); sources],
            panes: [0, if sources > 1 { 1 } else { 0 }],
            active_pane: 0,
            split: false,
            sidebar: true,
            hovered_row: None,
            timestamps: false,
            show_keys: false,
            picker: None,
            recent: Vec::new(),
            input: None,
            history: Vec::new(),
            history_at: None,
            searching: false,
            profile_path: profile.path,
            problems,
            flash: None,
        }
    }

    /// The source in the active pane
    fn current(&self) -> Option<SourceId> {
        (!self.sup.entries().is_empty()).then_some(self.panes[self.active_pane])
    }

    /// The sources on screen
    fn visible(&self) -> impl Iterator<Item = SourceId> + '_ {
        let count = if self.split { 2 } else { 1 };
        self.panes[..count].iter().copied().filter(|_| !self.sup.entries().is_empty())
    }

    fn focus(&mut self, id: SourceId) {
        if id < self.sup.entries().len() {
            self.panes[self.active_pane] = id;
        }
    }

    /// Shows a source in the other pane, splitting if needed, and makes that pane active
    fn open_beside(&mut self, id: SourceId) {
        if !self.split {
            self.split = true;
            self.active_pane = 0;
        }
        self.active_pane ^= 1;
        self.panes[self.active_pane] = id;
    }

    fn flash(&mut self, text: impl Into<String>, bad: bool) {
        self.flash = Some(Flash { text: text.into(), bad, at: Instant::now() });
    }

    /// The shortest keys for a command, for hints
    fn key_of(&self, command: Command) -> Option<String> {
        self.keymap.keys_for(command).into_iter().min_by_key(|keys| keys.len()).map(|keys| format_sequence(keys))
    }

    /// Runs the keymap over this frame's input before egui sees it, so bound keys never reach widgets: Tab doesn't
    /// move widget focus, and Space doesn't press a focused button.
    fn handle_keys(&mut self, ctx: &Context, events: &mut Vec<Event>) {
        // The picker handles its own keys
        if self.picker.is_some() {
            self.matcher.cancel();
            return;
        }
        let typing = ctx.text_edit_focused();
        if typing {
            self.matcher.cancel();
        }
        let mut commands = Vec::new();
        let mut consumed_any = false;
        events.retain(|event| {
            let Event::Key { key, pressed: true, modifiers, .. } = event else { return true };
            let chord = Chord::new(*key, *modifiers);
            let consumed = if typing {
                // Only a few Ctrl chords, so the text field keeps its own editing keys
                match self.keymap.lookup(&[chord]).filter(|c| chord.ctrl && WHILE_TYPING.contains(c)) {
                    Some(command) => {
                        commands.push(command);
                        true
                    }
                    None => false,
                }
            } else if *key == Key::Escape && *modifiers == Modifiers::NONE {
                self.escape();
                true
            } else {
                match self.matcher.press(chord, &self.keymap) {
                    Outcome::Run(command) => {
                        commands.push(command);
                        true
                    }
                    Outcome::Pending => true,
                    Outcome::Unbound => false,
                }
            };
            consumed_any |= consumed;
            !consumed
        });
        if consumed_any {
            // The characters typed by those keys too, or a text field a command opens would receive them
            events.retain(|e| !matches!(e, Event::Text(_)));
        }
        for command in commands {
            self.run(command, ctx);
        }
    }

    /// Escape backs out of one thing at a time: a key sequence, the key list, the filter, an error mark
    fn escape(&mut self) {
        if !self.matcher.pending().is_empty() {
            self.matcher.cancel();
        } else if self.show_keys {
            self.show_keys = false;
        } else if let Some(id) = self.current() {
            let view = &mut self.views[id];
            if !view.filter.is_empty() {
                view.filter.clear();
            } else if !view.follow {
                view.scroll(Scroll::Bottom);
            }
        }
    }

    fn run(&mut self, command: Command, ctx: &Context) {
        let count = self.sup.entries().len();
        match command {
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Command::QuickOpen => self.picker = Some(Picker::sources()),
            Command::OpenPalette => self.picker = Some(Picker::commands()),
            Command::ShowKeys => self.show_keys = !self.show_keys,
            Command::ToggleSidebar => self.sidebar = !self.sidebar,
            Command::NextSource if count > 0 => self.focus((self.panes[self.active_pane] + 1) % count),
            Command::PrevSource if count > 0 => self.focus((self.panes[self.active_pane] + count - 1) % count),
            Command::FocusSource(n) => self.focus(usize::from(n) - 1),
            Command::StartAll => self.sup.start_all(),
            Command::StopAll => self.sup.stop_all(),
            Command::ToggleTimestamps => self.timestamps = !self.timestamps,
            Command::ToggleSplit => {
                if self.split {
                    self.panes[0] = self.panes[self.active_pane];
                    self.active_pane = 0;
                    self.split = false;
                } else if count > 0 {
                    let other =
                        if self.panes[1] == self.panes[0] { (self.panes[0] + 1) % count } else { self.panes[1] };
                    self.open_beside(other);
                }
            }
            Command::OtherPane if self.split => self.active_pane ^= 1,
            _ => {
                if let Some(id) = self.current() {
                    self.run_on(command, id, ctx);
                }
            }
        }
    }

    /// Runs a command that acts on one source
    fn run_on(&mut self, command: Command, id: SourceId, ctx: &Context) {
        let view = &mut self.views[id];
        let buffer = &self.sup.entries()[id].buffer;
        match command {
            Command::Start => self.sup.start(id),
            Command::Stop => self.sup.stop(id),
            Command::Restart => self.sup.restart(id),
            Command::FocusInput => {
                self.input = Some(String::new());
                self.history_at = None;
            }
            Command::ScrollDown => view.scroll(Scroll::Lines(3.0)),
            Command::ScrollUp => view.scroll(Scroll::Lines(-3.0)),
            Command::PageDown => view.scroll(Scroll::Pages(1.0)),
            Command::PageUp => view.scroll(Scroll::Pages(-1.0)),
            Command::Top => view.scroll(Scroll::Top),
            Command::Bottom => view.scroll(Scroll::Bottom),
            Command::ToggleFollow => view.toggle_follow(),
            Command::Search => self.searching = true,
            Command::ClearSearch => {
                view.filter.clear();
                self.searching = false;
            }
            Command::ClearView => self.sup.entry_mut(id).buffer.clear(),
            Command::CopyOutput => {
                let (text, lines) = view.shown_text(buffer);
                ctx.copy_text(text);
                let what = if view.filter.is_empty() { "" } else { " matching the filter" };
                self.flash(format!("copied {lines} lines{what}"), false);
            }
            Command::NextError | Command::PrevError => {
                let forward = command == Command::NextError;
                if !view.jump_to_problem(buffer, forward) {
                    let way = if forward { "below" } else { "above" };
                    self.flash(format!("no errors or warnings {way}"), false);
                }
            }
            _ => {}
        }
    }

    fn mark_seen(&mut self) {
        for id in self.visible().collect::<Vec<_>>() {
            let buffer = &self.sup.entries()[id].buffer;
            self.seen[id] = Seen { total: buffer.total(), errors: buffer.errors(), warnings: buffer.warnings() };
        }
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("SOURCES").small().weak());
            if let Some(keys) = self.key_of(Command::QuickOpen) {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(keys).small().weak().monospace());
                });
            }
        });
        ui.add_space(4.0);

        let current = self.current();
        let visible: Vec<_> = self.visible().collect();
        let mut action: Option<(RowAction, SourceId)> = None;
        let mut hovered = None;
        for (id, entry) in self.sup.entries().iter().enumerate() {
            let selected = current == Some(id);
            let shown_beside = !selected && visible.contains(&id);
            let show_buttons = selected || self.hovered_row == Some(id);
            let response = ui
                .scope_builder(UiBuilder::new().sense(Sense::click()), |ui| {
                    source_row(ui, id, entry, self.seen[id], selected, shown_beside, show_buttons, &mut action);
                })
                .response;
            if response.hovered() || response.contains_pointer() {
                hovered = Some(id);
            }
            if response.clicked() {
                let beside = ui.input(|i| i.modifiers.command);
                action = Some((if beside { RowAction::Beside } else { RowAction::Focus }, id));
            }
            response.context_menu(|ui| {
                let mut item = |ui: &mut Ui, label: &str, row_action: RowAction| {
                    if ui.button(label).clicked() {
                        action = Some((row_action, id));
                        ui.close();
                    }
                };
                item(ui, "Open", RowAction::Focus);
                item(ui, "Open beside", RowAction::Beside);
                ui.separator();
                if entry.status.is_active() || entry.restart_at.is_some() {
                    item(ui, "Stop", RowAction::Run(Command::Stop));
                    item(ui, "Restart", RowAction::Run(Command::Restart));
                } else {
                    item(ui, "Start", RowAction::Run(Command::Start));
                }
                item(ui, "Clear output", RowAction::Run(Command::ClearView));
            });
        }
        self.hovered_row = hovered;

        ui.add_space(8.0);
        ui.label(RichText::new("Ctrl+click opens beside · right-click for more").small().weak());

        let ctx = ui.ctx().clone();
        match action {
            Some((RowAction::Focus, id)) => self.focus(id),
            Some((RowAction::Beside, id)) => self.open_beside(id),
            Some((RowAction::Run(command), id)) => self.run_on(command, id, &ctx),
            None => {}
        }
    }

    fn panes(&mut self, ui: &mut Ui) {
        if self.sup.entries().is_empty() {
            self.empty_state(ui);
            return;
        }
        let entries = self.sup.entries();
        if self.split {
            let mut clicked = None;
            ui.columns(2, |columns| {
                for (pane, column) in columns.iter_mut().enumerate() {
                    let id = self.panes[pane];
                    if view::show(
                        column,
                        pane,
                        &entries[id],
                        &mut self.views[id],
                        self.timestamps,
                        pane == self.active_pane,
                    ) {
                        clicked = Some(pane);
                    }
                }
            });
            if let Some(pane) = clicked {
                self.active_pane = pane;
            }
        } else {
            let id = self.panes[0];
            view::show(ui, 0, &entries[id], &mut self.views[id], self.timestamps, false);
        }
    }

    fn empty_state(&self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 4.0);
            ui.heading("No sources");
            ui.add_space(8.0);
            let where_ = match &self.profile_path {
                Some(path) => format!("{} has no [[process]] or [[serial]] entries.", path.display()),
                None => format!("No {FILE_NAME} found here or in any directory above."),
            };
            ui.label(where_);
            ui.label("Pass a profile with `archon path/to/archon.toml`, or create one like this:");
            ui.add_space(8.0);
            let mut example = EXAMPLE.to_owned();
            ui.add(TextEdit::multiline(&mut example).code_editor().desired_width(520.0).interactive(false));
        });
    }

    /// The keys that matter right now, so nobody has to remember them
    fn hints(&self) -> Vec<(String, &'static str)> {
        let fixed = |pairs: &[(&str, &'static str)]| pairs.iter().map(|(k, l)| ((*k).to_owned(), *l)).collect();
        if let Some(picker) = &self.picker {
            return if picker.is_commands() {
                fixed(&[("Up/Down", "select"), ("Enter", "run"), ("Ctrl+P", "sources"), ("Esc", "close")])
            } else {
                fixed(&[
                    ("Up/Down", "select"),
                    ("Enter", "open"),
                    ("Ctrl+Enter", "open beside"),
                    ("Ctrl+Shift+P", "commands"),
                    ("Esc", "close"),
                ])
            };
        }
        if self.input.is_some() {
            return fixed(&[("Enter", "send"), ("Up/Down", "history"), ("Esc", "close")]);
        }
        if self.searching {
            return fixed(&[("Enter", "keep filter"), ("Esc", "clear filter")]);
        }
        let Some(id) = self.current() else {
            return [(Command::OpenPalette, "commands"), (Command::ShowKeys, "keys")]
                .into_iter()
                .filter_map(|(c, l)| Some((self.key_of(c)?, l)))
                .collect();
        };
        let entry = &self.sup.entries()[id];
        let view = &self.views[id];
        let running = entry.status.is_active() || entry.restart_at.is_some();
        let mut wanted = vec![(Command::QuickOpen, "sources"), (Command::OpenPalette, "commands")];
        if running {
            wanted.extend([(Command::Stop, "stop"), (Command::Restart, "restart"), (Command::FocusInput, "send")]);
        } else {
            wanted.push((Command::Start, "start"));
        }
        wanted.push((Command::Search, "filter"));
        wanted.push((Command::ToggleFollow, if view.follow { "pause" } else { "follow" }));
        if entry.buffer.errors() + entry.buffer.warnings() > 0 {
            wanted.push((Command::NextError, "next error"));
        }
        wanted.push((Command::ShowKeys, "all keys"));
        let mut hints: Vec<_> = wanted.into_iter().filter_map(|(c, l)| Some((self.key_of(c)?, l))).collect();
        if !view.filter.is_empty() || !view.follow {
            hints.insert(2, ("Esc".to_owned(), if view.filter.is_empty() { "back to end" } else { "clear filter" }));
        }
        hints
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let pending = self.matcher.pending();
            if pending.is_empty() {
                let chip = ui.visuals().widgets.inactive.bg_fill;
                for (keys, label) in self.hints() {
                    ui.label(RichText::new(format!(" {keys} ")).monospace().small().background_color(chip));
                    ui.label(RichText::new(label).small().weak());
                    ui.add_space(6.0);
                }
            } else {
                ui.label(RichText::new(format!("{} …", format_sequence(pending))).monospace().strong());
                ui.label(RichText::new("Esc cancels").small().weak());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let profile = self.profile_path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy());
                let profile = profile.unwrap_or("no profile".into());
                ui.label(RichText::new(profile).small().weak())
                    .on_hover_text(self.profile_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
                if self.timestamps {
                    ui.label(RichText::new("TIME").small().weak());
                }
                if self.split {
                    ui.label(RichText::new(format!("PANE {}", self.active_pane + 1)).small().weak());
                }
                if let Some(flash) = &self.flash
                    && flash.at.elapsed() < FLASH_TIME
                {
                    let color = if flash.bad { ui.visuals().error_fg_color } else { ui.visuals().strong_text_color() };
                    ui.label(RichText::new(&flash.text).small().color(color));
                    ui.ctx().request_repaint_after(FLASH_TIME);
                }
            });
        });
    }

    fn input_bar(&mut self, ui: &mut Ui) {
        let Some(id) = self.current() else {
            self.input = None;
            return;
        };
        let (enter, escape, up, down) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
            )
        });
        if escape {
            self.input = None;
            return;
        }
        if up || down {
            self.recall_history(up);
        }
        if enter {
            self.send_input(id);
        }

        let Some(text) = &mut self.input else { return };
        let name = &self.sup.entries()[id].name;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{name} <")).monospace().strong());
            let edit =
                TextEdit::singleline(text).hint_text("type a line to send").code_editor().desired_width(f32::INFINITY);
            ui.add(edit).request_focus();
        });
    }

    fn recall_history(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let last = self.history.len() - 1;
        self.history_at = match (self.history_at, older) {
            (None, true) => Some(last),
            (None, false) => None,
            (Some(at), true) => Some(at.saturating_sub(1)),
            (Some(at), false) if at < last => Some(at + 1),
            (Some(_), false) => None,
        };
        self.input = Some(self.history_at.map(|at| self.history[at].clone()).unwrap_or_default());
    }

    fn send_input(&mut self, id: SourceId) {
        let Some(text) = self.input.replace(String::new()) else { return };
        let ending = self.sup.entries()[id].line_ending;
        if let Err(e) = self.sup.write(id, format!("{text}{ending}").as_bytes()) {
            self.flash(format!("couldn't send to {}: {e}", self.sup.entries()[id].name), true);
        }
        if !text.is_empty() && self.history.last() != Some(&text) {
            self.history.push(text);
        }
        self.history_at = None;
    }

    fn search_bar(&mut self, ui: &mut Ui) {
        let Some(id) = self.current() else {
            self.searching = false;
            return;
        };
        let (enter, escape) =
            ui.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape)));
        let view = &mut self.views[id];
        if escape {
            view.filter.clear();
        }
        if enter || escape {
            self.searching = false;
            return;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("/").monospace().strong());
            let edit = TextEdit::singleline(&mut view.filter)
                .hint_text("show only lines containing… (a capital makes it case-sensitive)")
                .code_editor()
                .desired_width(f32::INFINITY);
            ui.add(edit).request_focus();
        });
    }

    fn problems_bar(&mut self, ui: &mut Ui) {
        let mut dismiss = false;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                for problem in &self.problems {
                    ui.label(RichText::new(problem).color(ui.visuals().error_fg_color));
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                dismiss = ui.small_button("dismiss").clicked();
            });
        });
        if dismiss {
            self.problems.clear();
        }
    }

    /// Lists what can follow the keys typed so far
    fn which_key(&self, ctx: &Context) {
        let pending = self.matcher.pending();
        if pending.is_empty() {
            return;
        }
        let next = self.keymap.continuations(pending);
        egui::Area::new(Id::unique("which-key"))
            .order(Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, vec2(-12.0, -36.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(RichText::new(format_sequence(pending)).monospace().strong());
                    ui.separator();
                    egui::Grid::new("which-key-grid").spacing(vec2(16.0, 2.0)).show(ui, |ui| {
                        for (chord, next) in &next {
                            ui.label(RichText::new(chord.to_string()).monospace().strong());
                            match next {
                                Next::Command(command) => ui.label(command.info().title),
                                Next::Group(name) => {
                                    ui.label(RichText::new(format!("+{name}")).color(ui.visuals().hyperlink_color))
                                }
                            };
                            ui.end_row();
                        }
                    });
                });
            });
    }

    fn keys_window(&mut self, ctx: &Context) {
        const GROUPS: &[(&str, &[&str])] = &[
            ("General", &["app.", "palette.", "picker.", "help.", "sidebar."]),
            ("Sources", &["source."]),
            ("Output", &["view."]),
            ("Panes", &["pane."]),
        ];
        let mut open = self.show_keys;
        egui::Window::new("Keys")
            .open(&mut open)
            .collapsible(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .default_height(560.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (group, prefixes) in GROUPS {
                        ui.add_space(6.0);
                        ui.label(RichText::new(*group).strong());
                        egui::Grid::new(("keys-grid", *group))
                            .striped(true)
                            .min_col_width(200.0)
                            .spacing(vec2(24.0, 4.0))
                            .show(ui, |ui| {
                                for info in COMMANDS.iter().filter(|i| prefixes.iter().any(|p| i.id.starts_with(p))) {
                                    // The nine number keys share one row
                                    let (keys, title) = match info.command {
                                        Command::FocusSource(1) => (self.number_keys(), "Focus source by number"),
                                        Command::FocusSource(_) => continue,
                                        command => {
                                            let keys = self.keymap.keys_for(command);
                                            (keys.iter().map(|k| format_sequence(k)).collect(), info.title)
                                        }
                                    };
                                    if keys.is_empty() {
                                        continue;
                                    }
                                    ui.label(RichText::new(keys.join("   ")).monospace());
                                    ui.label(title);
                                    ui.end_row();
                                }
                            });
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("Esc closes · rebind in ~/.config/archon/keymap.toml").small().weak());
                });
            });
        self.show_keys = open && self.show_keys;
    }

    /// The first key of each "focus source N" command, as one entry: `1 2 3 4 5 6 7 8 9`
    fn number_keys(&self) -> Vec<String> {
        let keys: Vec<String> = (1..=9)
            .filter_map(|n| self.keymap.keys_for(Command::FocusSource(n)).first().map(|k| format_sequence(k)))
            .collect();
        if keys.is_empty() { Vec::new() } else { vec![keys.join(" ")] }
    }

    fn picker(&mut self, ctx: &Context) {
        let Some(picker) = &mut self.picker else { return };
        match picker.show(ctx, self.sup.entries(), &self.keymap, &self.recent) {
            picker::Outcome::Open => {}
            picker::Outcome::Closed => self.picker = None,
            picker::Outcome::Chose(choice) => {
                self.picker = None;
                match choice {
                    Choice::Command(command) => {
                        self.recent.retain(|c| *c != command);
                        self.recent.insert(0, command);
                        self.recent.truncate(RECENT_COMMANDS);
                        self.run(command, ctx);
                    }
                    Choice::Source { id, beside: true } => self.open_beside(id),
                    Choice::Source { id, beside: false } => self.focus(id),
                }
            }
        }
    }
}

/// One source in the sidebar: number, status, name and unread marks, with its status detail underneath
#[allow(clippy::too_many_arguments)]
fn source_row(
    ui: &mut Ui,
    id: SourceId,
    entry: &Entry,
    seen: Seen,
    selected: bool,
    shown_beside: bool,
    show_buttons: bool,
    action: &mut Option<(RowAction, SourceId)>,
) {
    let visuals = ui.visuals().clone();
    let fill = if selected {
        visuals.selection.bg_fill.gamma_multiply(0.6)
    } else if shown_beside {
        visuals.selection.bg_fill.gamma_multiply(0.25)
    } else if show_buttons {
        visuals.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    egui::Frame::NONE.fill(fill).corner_radius(4.0).inner_margin(Margin::symmetric(6, 4)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let number = if id < 9 { format!("{}", id + 1) } else { " ".to_owned() };
            ui.label(RichText::new(number).weak().monospace());
            status_dot(ui, status_color(&entry.status, &visuals));
            // Bold when there's output you haven't seen, like an unread chat
            let unread = entry.buffer.total() > seen.total;
            let name = RichText::new(&entry.name);
            ui.add(egui::Label::new(if unread || selected { name.strong() } else { name }).selectable(false));
            let errors = entry.buffer.errors() - seen.errors;
            let warnings = entry.buffer.warnings() - seen.warnings;
            if errors > 0 {
                badge(ui, errors, visuals.error_fg_color, Color32::WHITE).on_hover_text(format!("{errors} new errors"));
            } else if warnings > 0 {
                badge(ui, warnings, visuals.warn_fg_color, Color32::BLACK)
                    .on_hover_text(format!("{warnings} new warnings"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !show_buttons {
                    ui.label(RichText::new(kind_tag(entry.kind)).small().weak());
                    return;
                }
                let buttons: &[(&str, Command)] = if entry.status.is_active() || entry.restart_at.is_some() {
                    &[("stop", Command::Stop), ("restart", Command::Restart)]
                } else {
                    &[("start", Command::Start)]
                };
                for &(label, command) in buttons {
                    if ui.small_button(label).clicked() {
                        *action = Some((RowAction::Run(command), id));
                    }
                }
            });
        });
        ui.label(RichText::new(detail(entry, ui.ctx())).small().weak());
    });
}

fn badge(ui: &mut Ui, count: u64, fill: Color32, text_color: Color32) -> egui::Response {
    let text = if count > 99 { "99+".to_owned() } else { count.to_string() };
    ui.label(RichText::new(format!(" {text} ")).small().strong().color(text_color).background_color(fill))
}

/// The line under a source's name: how long it's been up, or when it restarts, or why it stopped
fn detail(entry: &Entry, ctx: &Context) -> String {
    if let Some(at) = entry.restart_at {
        ctx.request_repaint_after(Duration::from_millis(250));
        let left = at.saturating_duration_since(Instant::now()).as_secs_f32().ceil();
        return format!("{} · restarting in {left}s", status_text(&entry.status));
    }
    match &entry.status {
        archon_core::source::Status::Running => {
            ctx.request_repaint_after(Duration::from_secs(1));
            format!("running · {}", uptime(entry.started.elapsed()))
        }
        status => status_text(status),
    }
}

fn uptime(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
    }
}

impl eframe::App for ArchonApp {
    fn raw_input_hook(&mut self, ctx: &Context, raw_input: &mut egui::RawInput) {
        self.handle_keys(ctx, &mut raw_input.events);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.sup.poll();

        Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.input.is_some() {
            Panel::bottom("input").show(ui, |ui| self.input_bar(ui));
        }
        if self.searching {
            Panel::bottom("search").show(ui, |ui| self.search_bar(ui));
        }
        if !self.problems.is_empty() {
            Panel::top("problems").show(ui, |ui| self.problems_bar(ui));
        }
        if self.sidebar {
            Panel::left("sources").resizable(true).default_size(260.0).show(ui, |ui| self.sidebar(ui));
        }
        CentralPanel::default_margins().show(ui, |ui| self.panes(ui));
        self.mark_seen();

        self.picker(&ctx);
        self.which_key(&ctx);
        if self.show_keys {
            self.keys_window(&ctx);
        }
    }
}

impl Drop for ArchonApp {
    fn drop(&mut self) {
        self.sup.shutdown(Duration::from_secs(3));
    }
}

const EXAMPLE: &str = r#"env = { RMW_IMPLEMENTATION = "rmw_fastrtps_cpp" }

[[process]]
name = "agent"
cmd = "ros2 run micro_ros_agent micro_ros_agent serial --dev /dev/ttyACM0"
autostart = true
restart = "on-failure"

[[serial]]
name = "debug"
port = "/dev/serial/by-id/usb-..."
baud = 115200"#;

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    const PROFILE: &str = r#"
        [[process]]
        name = "agent"
        cmd = "sleep 30"
        [[process]]
        name = "imu"
        cmd = "sleep 30"
        [[process]]
        name = "thrusters"
        cmd = "sleep 30"
    "#;

    fn harness() -> Harness<'static, ArchonApp> {
        let mut harness = Harness::builder()
            .with_size(vec2(1200.0, 700.0))
            .build_eframe(|cc| ArchonApp::new(cc, Profile::parse(PROFILE).unwrap(), Keymap::defaults(), Vec::new()));
        harness.run_steps(2);
        harness
    }

    /// Sends a key the way the real window does: through `raw_input_hook` first, then to egui
    fn press(harness: &mut Harness<'static, ArchonApp>, modifiers: Modifiers, key: Key) {
        let mut events = vec![
            Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers },
            Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers },
        ];
        let ctx = harness.ctx.clone();
        harness.state_mut().handle_keys(&ctx, &mut events);
        for event in events {
            harness.event(event);
        }
        harness.run_steps(2);
    }

    fn type_text(harness: &mut Harness<'static, ArchonApp>, text: &str) {
        harness.event(Event::Text(text.to_owned()));
        harness.run_steps(2);
    }

    #[test]
    fn ctrl_p_picks_a_source() {
        let mut h = harness();
        press(&mut h, Modifiers::CTRL, Key::P);
        assert!(h.state().picker.as_ref().is_some_and(|p| !p.is_commands()));
        type_text(&mut h, "thr");
        press(&mut h, Modifiers::NONE, Key::Enter);
        assert!(h.state().picker.is_none());
        assert_eq!(h.state().current(), Some(2));
    }

    #[test]
    fn ctrl_shift_p_runs_a_command() {
        let mut h = harness();
        press(&mut h, Modifiers::CTRL | Modifiers::SHIFT, Key::P);
        assert!(h.state().picker.as_ref().is_some_and(Picker::is_commands));
        type_text(&mut h, "timestamps");
        press(&mut h, Modifiers::NONE, Key::Enter);
        assert!(h.state().timestamps);
        assert_eq!(h.state().recent, [Command::ToggleTimestamps]);
    }

    #[test]
    fn picker_switches_modes() {
        let mut h = harness();
        press(&mut h, Modifiers::CTRL, Key::P);
        press(&mut h, Modifiers::CTRL | Modifiers::SHIFT, Key::P);
        assert!(h.state().picker.as_ref().is_some_and(Picker::is_commands));
        press(&mut h, Modifiers::CTRL, Key::P);
        assert!(h.state().picker.as_ref().is_some_and(|p| !p.is_commands()));
        press(&mut h, Modifiers::NONE, Key::Escape);
        assert!(h.state().picker.is_none());
    }

    #[test]
    fn ctrl_enter_opens_beside() {
        let mut h = harness();
        press(&mut h, Modifiers::CTRL, Key::P);
        type_text(&mut h, "imu");
        press(&mut h, Modifiers::CTRL, Key::Enter);
        let app = h.state();
        assert!(app.split);
        assert_eq!(app.panes, [0, 1]);
        assert_eq!(app.active_pane, 1);
    }

    #[test]
    fn tab_cycles_sources_instead_of_widget_focus() {
        let mut h = harness();
        press(&mut h, Modifiers::NONE, Key::Tab);
        press(&mut h, Modifiers::NONE, Key::Tab);
        assert_eq!(h.state().current(), Some(2));
        press(&mut h, Modifiers::SHIFT, Key::Tab);
        assert_eq!(h.state().current(), Some(1));
        assert!(h.ctx.memory(|m| m.focused().is_none()), "no widget took focus");
    }

    #[test]
    fn number_keys_and_leader_sequences() {
        let mut h = harness();
        press(&mut h, Modifiers::NONE, Key::Num3);
        assert_eq!(h.state().current(), Some(2));
        press(&mut h, Modifiers::NONE, Key::Space);
        press(&mut h, Modifiers::NONE, Key::W);
        assert_eq!(h.state().matcher.pending().len(), 2, "waiting for the rest of Space w …");
        press(&mut h, Modifiers::NONE, Key::V);
        assert!(h.state().split);
    }

    #[test]
    fn ctrl_p_works_while_filtering() {
        let mut h = harness();
        press(&mut h, Modifiers::NONE, Key::Slash);
        assert!(h.state().searching);
        type_text(&mut h, "x");
        assert_eq!(h.state().views[0].filter, "x", "the / itself isn't typed into the filter");
        press(&mut h, Modifiers::CTRL, Key::P);
        assert!(h.state().picker.is_some());
    }

    #[test]
    fn escape_clears_the_filter() {
        let mut h = harness();
        press(&mut h, Modifiers::NONE, Key::Slash);
        type_text(&mut h, "imu");
        press(&mut h, Modifiers::NONE, Key::Enter);
        assert_eq!(h.state().views[0].filter, "imu", "Enter keeps it");
        press(&mut h, Modifiers::NONE, Key::Escape);
        assert!(h.state().views[0].filter.is_empty());
    }

    #[test]
    fn hints_follow_the_source_state() {
        let mut h = harness();
        let labels =
            |h: &Harness<'static, ArchonApp>| h.state().hints().into_iter().map(|(_, l)| l).collect::<Vec<_>>();
        assert!(labels(&h).contains(&"start"));
        press(&mut h, Modifiers::NONE, Key::S);
        assert!(labels(&h).contains(&"stop"));
        assert_eq!(h.state().hints()[0], ("Ctrl+P".to_owned(), "sources"));
    }

    /// Renders the main states to target/screenshots, to look at the UI without a window. Needs a GPU (or
    /// lavapipe), so it only runs when asked: `cargo test -p archon screenshots -- --ignored`
    #[test]
    #[ignore]
    fn screenshots() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = harness();
        {
            let app = h.state_mut();
            app.sup.start(0);
            app.sup
                .entry_mut(0)
                .buffer
                .push_output("[INFO] [1.0] [agent]: session established\n[WARN] [1.2] [agent]: slow heartbeat\n");
            app.sup.entry_mut(1).buffer.push_output("[ERROR] [2.0] [imu]: no data for 500 ms\n");
        }
        h.run_steps(3);
        let save = |h: &mut Harness<'static, ArchonApp>, name: &str| {
            h.run_steps(2);
            h.render().unwrap().save(dir.join(format!("{name}.png"))).unwrap();
        };
        save(&mut h, "main");
        press(&mut h, Modifiers::CTRL, Key::P);
        type_text(&mut h, "im");
        save(&mut h, "picker-sources");
        press(&mut h, Modifiers::CTRL | Modifiers::SHIFT, Key::P);
        type_text(&mut h, "rest");
        save(&mut h, "picker-commands");
        press(&mut h, Modifiers::NONE, Key::Escape);
        press(&mut h, Modifiers::NONE, Key::Space);
        save(&mut h, "which-key");
        press(&mut h, Modifiers::NONE, Key::Escape);
        press(&mut h, Modifiers::NONE, Key::Questionmark);
        save(&mut h, "keys");
        h.state_mut().sup.stop_all();
    }

    #[test]
    fn formats_uptime() {
        assert_eq!(uptime(Duration::from_secs(42)), "42s");
        assert_eq!(uptime(Duration::from_secs(252)), "4m12s");
        assert_eq!(uptime(Duration::from_secs(3840)), "1h04m");
    }
}
