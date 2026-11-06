//! The window: the health strip, the sidebar, tiled panes of views, the bars along the bottom, and the overlays.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use archon_core::command::{COMMANDS, Command};
use archon_core::profile::{FILE_NAME, Profile, RestartPolicy};
use archon_core::source::{SourceId, SourceKind};
use archon_msg::Registry;
use archon_ros::{EchoSource, Graph, Ros, TopicState, Watcher};
use egui::{
    Align2, CentralPanel, Color32, Context, Event, Id, Key, Modifiers, Order, Panel, RichText, TextEdit, Ui, vec2,
};

use crate::bench::{self, Port};
use crate::bytes::{self, BytesState};
use crate::health::{self, Watch};
use crate::inspector::{self, InspectorState};
use crate::keymap::{Chord, Keymap, Matcher, Next, Outcome, format_sequence};
use crate::layout::{Direction, Layout, View};
use crate::logview::{self, Merged, Rows, Scroll, TopicMode, ViewState};
use crate::persist::{self, PaneSpec, Saved};
use crate::picker::OptionItem;
use crate::picker::{self, ActionItem, Choice, Picker};
use crate::plot::{self, Plot};
use crate::publish::{self, Publisher};
use crate::topics::{self, TopicRow, TopicsTable};

/// How long a message in the status bar stays
const FLASH_TIME: Duration = Duration::from_secs(4);

/// Commands whose Ctrl chords still work while typing in the input or filter bar
const WHILE_TYPING: &[Command] = &[Command::QuickOpen, Command::OpenPalette, Command::Quit, Command::ToggleSidebar];

/// Commands run from the picker that it lists first next time
const RECENT_COMMANDS: usize = 5;

/// Rows a page up or down moves in the inspector and the topics table
const PAGE: isize = 20;

/// What archon starts with
/// What the removal question is about
#[derive(Clone, Copy, Debug, PartialEq)]
enum Removal {
    Source(SourceId),
    Publisher(usize),
}

pub struct Startup {
    pub profile: Profile,
    pub keymap: Keymap,
    pub problems: Vec<String>,
    /// Join the ROS graph, which the tests leave out
    pub with_ros: bool,
    /// Where the layout is kept between runs; the tests have none
    pub state_file: Option<PathBuf>,
    pub config: bench::Config,
    /// The port of this window's own discovery server, when it's isolated
    pub discovery: Option<u16>,
    /// A firmware build or robot file to simulate on start
    pub simulate: Option<PathBuf>,
    /// Agents and consoles to open on start, as a robot's isolated window is given them
    pub attach: Vec<crate::robot::Attach>,
}

pub struct ArchonApp {
    pub(crate) sup: archon_sources::Supervisor,
    keymap: Keymap,
    matcher: Matcher,
    /// One per source: its log view, and for topics the inspector
    pub(crate) views: Vec<ViewState>,
    pub(crate) inspectors: Vec<InspectorState>,
    pub(crate) bytes: Vec<BytesState>,
    /// The topic sources' latest messages
    pub(crate) topic_states: HashMap<SourceId, Arc<Mutex<TopicState>>>,
    pub(crate) merged: ViewState,
    pub(crate) table: TopicsTable,
    /// How much of each source has been on screen, for unread marks
    pub(crate) seen: Vec<Seen>,
    pub(crate) layout: Layout,
    pub(crate) sidebar: bool,
    /// The sidebar row under the pointer last frame, which shows its buttons
    pub(crate) hovered_row: Option<SourceId>,
    pub(crate) timestamps: bool,
    show_keys: bool,
    picker: Option<Picker>,
    recent: Vec<Command>,
    /// The input bar, while open
    input: Option<String>,
    history: Vec<String>,
    /// Where Up/Down in the input bar is in `history`
    history_at: Option<usize>,
    searching: bool,
    /// The name being typed for "Save layout as"
    naming_layout: Option<String>,
    profile_path: Option<PathBuf>,
    /// The profile's environment, for processes archon starts itself, like a bag recording
    profile_env: std::collections::BTreeMap<String, String>,
    /// Problems loading the profile or keymap, shown until dismissed
    problems: Vec<String>,
    pub(crate) flash: Option<Flash>,
    pub(crate) ros: Result<RosLink, String>,
    /// The latest graph snapshot, taken each frame
    pub(crate) graph: Arc<Graph>,
    pub(crate) watches: Vec<Watch>,
    /// Rate-only subscriptions for the topics table's "measure every rate"
    measured: HashMap<String, Watcher>,
    /// Message definitions, shared with every topic echo
    registry: Arc<Mutex<Registry>>,
    /// Publish forms: the profile's actions first, then those opened since
    pub(crate) publishers: Vec<Publisher>,
    pub(crate) plots: Vec<Plot>,
    /// The plot `p` adds to while it's on screen
    current_plot: Option<usize>,
    state_file: Option<PathBuf>,
    /// The zoom, kept to save; `zoom_pending` applies a restored one on the first frame
    zoom: f32,
    zoom_pending: bool,
    /// Things a view asked for while drawing, done after
    deferred: Vec<Deferred>,
    config: bench::Config,
    /// What the options picker is choosing for
    picking: Option<Pick>,
    /// The port of this window's own discovery server, when it's isolated
    discovery: Option<u16>,
    /// The robot file being written, in its view
    builder: Option<crate::builder::Builder>,
    /// Flashes started here, so a finished one makes way for the next
    flashes: Vec<SourceId>,
    /// The line of each flash's output whose question has been put to the user
    asked: HashMap<SourceId, u64>,
    /// The board file of each simulator started here, to tell when another copy of a board would mix topics with it
    simulated: Vec<(SourceId, PathBuf)>,
    /// The arguments of the windows opened, in place of opening them
    #[cfg(test)]
    pub(crate) spawned: Vec<Vec<String>>,
    /// A source waiting on whether its removal takes it out of the profile too
    removing: Option<(Removal, String)>,
    /// Sources opened from inside archon, not yet in a profile
    added: Vec<Added>,
}

/// What a list of ports is being picked for
enum Pick {
    Agent(Vec<Port>),
    Serial(Vec<Port>),
    Can(Vec<Port>),
    Simulate(Vec<Simulatable>),
    /// A build to add to the robot being written
    RobotBoard(Vec<bench::Firmware>),
    /// A build to flash
    Flash(Vec<bench::Firmware>),
    /// How much of a build to flash: the build's name, and its images
    FlashMode(String, Vec<(crate::flash::Mode, PathBuf)>),
    /// The answer to upload_tool's question, for this flash: what to type for each choice
    FlashAnswer(SourceId, Vec<String>),
}

/// What Space n f can run
enum Simulatable {
    NewRobot,
    Robot(crate::robot::Robot),
    Build(bench::Firmware),
}

/// A source opened from inside archon, as it would be written to a profile
enum Added {
    Process(archon_core::profile::ProcessSpec),
    Serial(archon_core::profile::SerialSpec),
    Can(archon_core::profile::CanSpec),
}

/// The ROS client and the message definitions shared by every topic echo
pub(crate) struct RosLink {
    ros: Arc<Ros>,
    registry: Arc<Mutex<Registry>>,
}

#[derive(Default, Clone, Copy)]
pub(crate) struct Seen {
    pub total: u64,
    pub errors: u64,
    pub warnings: u64,
}

pub(crate) struct Flash {
    pub text: String,
    bad: bool,
    at: Instant,
}

enum Deferred {
    OpenTopic { name: String, ty: Option<String>, beside: bool },
    ToggleBranch(SourceId),
    Send(usize),
    ToggleRepeat(usize),
    SaveToProfile(usize),
    Unplot(usize, usize),
    Run(Command),
    Builder(crate::builder::Action),
}

impl ArchonApp {
    pub fn new(cc: &eframe::CreationContext<'_>, startup: Startup) -> Self {
        let Startup { profile, keymap, mut problems, with_ros, state_file, config, discovery, simulate, attach } =
            startup;
        set_text_sizes(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        let sup = archon_sources::Supervisor::new(&profile, wake.clone());
        let registry = Arc::new(Mutex::new(Registry::from_env()));
        let ros = if with_ros {
            Ros::start(archon_ros::Options::default(), wake)
                .map(|ros| RosLink { ros: Arc::new(ros), registry: registry.clone() })
                .map_err(|e| format!("{e:#}"))
        } else {
            Err("started without ROS".to_owned())
        };
        let watches = profile
            .watches
            .iter()
            .map(|spec| Watch { watcher: Watcher::new(&spec.topic), min_hz: spec.min_hz })
            .collect();
        let sources = sup.entries().len();
        let first_view = if sources > 0 || !profile.echoes.is_empty() { View::Source(0) } else { View::Welcome };
        let mut keymap = keymap;
        let mut publishers = Vec::new();
        for (i, spec) in profile.publishes.iter().enumerate() {
            let mut publisher = Publisher::new(&mut registry.lock().unwrap(), &spec.name, &spec.topic, &spec.ty);
            if let Err(why) = publisher.apply_table(&registry.lock().unwrap(), &spec.value) {
                problems.push(format!("publish action {}: {why}", spec.name));
            }
            if let Some(rate) = spec.rate {
                publisher.rate_text = rate.to_string();
                publisher.repeats = true;
            }
            publisher.in_profile = true;
            publisher.key = spec.key.clone();
            if let Some(key) = &spec.key {
                match (crate::keymap::parse_sequence(key), u8::try_from(i + 1)) {
                    (Ok(sequence), Ok(n @ 1..=9)) => keymap.bind(sequence, Command::RunAction(n)),
                    (Err(e), _) => problems.push(format!("publish action {}: key {key:?}: {e}", spec.name)),
                    (Ok(_), _) => {
                        problems.push(format!("publish action {}: only the first nine can have keys", spec.name))
                    }
                }
            }
            publishers.push(publisher);
        }
        let mut app = Self {
            sup,
            keymap,
            matcher: Matcher::default(),
            views: Vec::new(),
            inspectors: Vec::new(),
            bytes: Vec::new(),
            topic_states: HashMap::new(),
            merged: ViewState::default(),
            table: TopicsTable::default(),
            seen: Vec::new(),
            layout: Layout::new(first_view),
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
            naming_layout: None,
            profile_path: profile.path.clone(),
            profile_env: profile.env.clone(),
            problems: Vec::new(),
            flash: None,
            ros,
            graph: Arc::default(),
            watches,
            measured: HashMap::new(),
            registry,
            publishers,
            plots: Vec::new(),
            current_plot: None,
            state_file,
            zoom: 1.0,
            zoom_pending: false,
            deferred: Vec::new(),
            config,
            picking: None,
            removing: None,
            added: Vec::new(),
            discovery,
            simulated: Vec::new(),
            builder: None,
            flashes: Vec::new(),
            asked: HashMap::new(),
            #[cfg(test)]
            spawned: Vec::new(),
        };
        app.grow_views();
        for i in 0..app.publishers.len() {
            app.advertise(i);
        }
        for spec in &profile.echoes {
            if let Err(why) = app.add_echo(spec.name(), &spec.topic, spec.ty.as_deref(), spec.autostart) {
                problems.push(format!("can't echo {}: ROS isn't running ({why})", spec.topic));
            }
        }
        if let Some(port) = discovery {
            let spec = archon_core::profile::ProcessSpec {
                name: "discovery".to_owned(),
                cmd: bench::discovery_command(port),
                cwd: None,
                env: Default::default(),
                autostart: true,
                restart: RestartPolicy::OnFailure,
            };
            let source = archon_sources::process::ProcessSource::new(spec.clone(), &app.profile_env);
            app.sup.add(&spec.name, Box::new(source), spec.restart, true, "\n");
            app.grow_views();
        }
        app.sup.start_autostart();
        if let Some(saved) = app.state_file.as_deref().and_then(persist::load) {
            app.restore(saved);
        }
        app.problems = problems;
        for one in &attach {
            app.attach(one);
        }
        if let Some(image) = simulate {
            let home = dirs::home_dir().unwrap_or_default();
            if image.extension().is_some_and(|e| e == "toml") {
                match crate::robot::load(&image, crate::robot::titan_firmware_dir(&home).as_deref()) {
                    Ok(robot) => app.simulate_robot(&robot),
                    Err(why) => app.flash(why, true),
                }
            } else {
                match bench::Firmware::from_image(image.clone()) {
                    Some(firmware) => app.simulate(&firmware),
                    None => {
                        app.flash(format!("{} isn't a firmware image (a .uf2) or robot file", image.display()), true)
                    }
                }
            }
        }
        app
    }

    /// Gives every source added since a view state, an inspector and an unread count
    fn grow_views(&mut self) {
        let sources = self.sup.entries().len();
        self.views.resize_with(sources, ViewState::default);
        self.inspectors.resize_with(sources, InspectorState::default);
        self.bytes.resize_with(sources, BytesState::default);
        self.seen.resize(sources, Seen::default());
    }

    /// Adds a topic echo as a source
    fn add_echo(&mut self, name: &str, topic: &str, ty: Option<&str>, start: bool) -> Result<SourceId, String> {
        let link = self.ros.as_ref().map_err(String::clone)?;
        let echo = EchoSource::new(link.ros.clone(), link.registry.clone(), topic, ty);
        let state = echo.state();
        let id = self.sup.add(name, Box::new(echo), RestartPolicy::Never, start, "\n");
        self.topic_states.insert(id, state);
        self.grow_views();
        Ok(id)
    }

    /// Shows a topic's echo, opening one if there isn't one yet
    pub(crate) fn open_topic(&mut self, topic: &str, ty: Option<&str>, beside: bool) {
        if let Some(id) = self.echo_for(topic, ty) {
            self.show_view(View::Source(id), beside);
        }
    }

    /// A topic's echo, opened if there isn't one yet
    fn echo_for(&mut self, topic: &str, ty: Option<&str>) -> Option<SourceId> {
        let existing =
            self.sup.entries().iter().position(|e| !e.removed && e.kind == SourceKind::Topic && e.name == topic);
        match existing {
            Some(id) => Some(id),
            None => match self.add_echo(topic, topic, ty, false) {
                Ok(id) => {
                    self.sup.start(id);
                    Some(id)
                }
                Err(why) => {
                    self.flash(format!("ROS isn't running: {why}"), true);
                    None
                }
            },
        }
    }

    /// Puts what was dragged from the sidebar where it was dropped
    fn dock(&mut self, drop: crate::layout::Drop) {
        let view = match drop.what {
            crate::layout::Dropped::View(view) => view,
            crate::layout::Dropped::Topic { name, ty } => match self.echo_for(&name, ty.as_deref()) {
                Some(id) => View::Source(id),
                None => return,
            },
        };
        self.layout.place(view, drop.pane, drop.side);
    }

    /// Shows a view in the active pane, or beside it in another pane (made if needed), which becomes active
    pub(crate) fn show_view(&mut self, view: View, beside: bool) {
        if beside {
            let pane = self.layout.beside();
            self.layout.set_view(pane, view);
            self.layout.focus(pane);
        } else {
            self.layout.set_active_view(view);
        }
    }

    /// The source in the active pane, if it shows one
    fn current_source(&self) -> Option<SourceId> {
        match self.layout.active_view() {
            View::Source(id) if id < self.sup.entries().len() => Some(id),
            _ => None,
        }
    }

    /// Whether a source shows as an inspector rather than a log
    fn inspecting(&self, id: SourceId) -> bool {
        self.topic_states.contains_key(&id) && self.views[id].mode == TopicMode::Inspector
    }

    /// Whether a source shows its raw messages' bytes
    fn showing_bytes(&self, id: SourceId) -> bool {
        self.topic_states.contains_key(&id) && self.views[id].mode == TopicMode::Bytes
    }

    pub(crate) fn flash(&mut self, text: impl Into<String>, bad: bool) {
        self.flash = Some(Flash { text: text.into(), bad, at: Instant::now() });
    }

    /// The shortest keys for a command, for hints
    pub(crate) fn key_of(&self, command: Command) -> Option<String> {
        self.keymap.keys_for(command).into_iter().min_by_key(|keys| keys.len()).map(|keys| format_sequence(keys))
    }

    /// Errors in output that hasn't been on screen
    pub(crate) fn unseen_errors(&self) -> u64 {
        self.sup
            .entries()
            .iter()
            .zip(&self.seen)
            .filter(|(e, _)| !e.removed)
            .map(|(e, seen)| e.buffer.errors() - seen.errors)
            .sum()
    }

    /// Runs the keymap over this frame's input before egui sees it, so bound keys never reach widgets: Tab doesn't
    /// move widget focus, and Space doesn't press a focused button.
    fn handle_keys(&mut self, ctx: &Context, events: &mut Vec<Event>) {
        // The picker and the removal question handle their own keys
        if self.picker.is_some() || self.removing.is_some() {
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
                // An open menu closes on its own
                if ctx.any_popup_open() {
                    false
                } else {
                    self.escape();
                    true
                }
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

    /// Escape backs out of one thing at a time: a key sequence, the key list, the filter, a paused view
    fn escape(&mut self) {
        if !self.matcher.pending().is_empty() {
            self.matcher.cancel();
            return;
        }
        if self.show_keys {
            self.show_keys = false;
            return;
        }
        if let Some(filter) = self.active_filter().filter(|f| !f.is_empty()) {
            filter.clear();
            return;
        }
        if let Some(view) = self.active_log().filter(|v| !v.follow) {
            view.scroll(Scroll::Bottom);
            return;
        }
        // Then back to what the pane showed before, like a browser
        self.layout.back();
    }

    /// The filter of the view in the active pane
    fn active_filter(&mut self) -> Option<&mut String> {
        match self.layout.active_view() {
            View::Topics => Some(&mut self.table.filter),
            View::AllLogs => Some(&mut self.merged.filter),
            View::Source(id) if id < self.views.len() => Some(&mut self.views[id].filter),
            View::Source(_) | View::Publish(_) | View::Plot(_) | View::Welcome | View::RobotBuilder => None,
        }
    }

    /// The log view in the active pane, if it shows a log
    fn active_log(&mut self) -> Option<&mut ViewState> {
        match self.layout.active_view() {
            View::AllLogs => Some(&mut self.merged),
            View::Source(id) if id < self.views.len() && !self.inspecting(id) => Some(&mut self.views[id]),
            _ => None,
        }
    }

    /// A view for a new pane: the first source not on screen, or else the merged log
    fn unshown_view(&self) -> View {
        let shown = self.layout.views();
        (0..self.sup.entries().len()).map(View::Source).find(|v| !shown.contains(v)).unwrap_or(View::AllLogs)
    }

    fn run(&mut self, command: Command, ctx: &Context) {
        let current = self.current_source();
        match command {
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Command::QuickOpen => self.picker = Some(Picker::sources()),
            Command::OpenPalette => self.picker = Some(Picker::commands()),
            Command::ShowKeys => self.show_keys = !self.show_keys,
            Command::ToggleSidebar => self.sidebar = !self.sidebar,
            Command::NextSource | Command::PrevSource => {
                // Through the sources still on the bench, in the sidebar's order
                let live: Vec<SourceId> = self.sup.live().map(|(id, _)| id).collect();
                if live.is_empty() {
                    return;
                }
                let at = current.and_then(|id| live.iter().position(|l| *l == id));
                let next = match (at, command == Command::NextSource) {
                    (Some(at), true) => (at + 1) % live.len(),
                    (Some(at), false) => (at + live.len() - 1) % live.len(),
                    (None, _) => 0,
                };
                self.show_view(View::Source(live[next]), false);
            }
            Command::FocusSource(n) => {
                let nth = self.sup.live().nth(usize::from(n) - 1).map(|(id, _)| id);
                if let Some(id) = nth {
                    self.show_view(View::Source(id), false);
                }
            }
            Command::StartAll => self.sup.start_all(),
            Command::StopAll => self.sup.stop_all(),
            Command::ToggleTimestamps => self.timestamps = !self.timestamps,
            Command::ToggleSplit | Command::SplitDown => {
                let view = self.unshown_view();
                self.layout.split(command == Command::ToggleSplit, view);
            }
            Command::ClosePane => {
                if !self.layout.close_active() {
                    self.flash("that's the last pane", false);
                }
            }
            Command::OtherPane => self.layout.cycle(),
            Command::FocusLeft => _ = self.layout.focus_direction(Direction::Left),
            Command::FocusRight => _ = self.layout.focus_direction(Direction::Right),
            Command::FocusUp => _ = self.layout.focus_direction(Direction::Up),
            Command::FocusDown => _ = self.layout.focus_direction(Direction::Down),
            Command::OpenAllLogs => self.show_view(View::AllLogs, false),
            Command::OpenTopics => self.show_view(View::Topics, false),
            Command::MeasureAll => {
                self.table.measure_all = !self.table.measure_all;
                let state = if self.table.measure_all { "measuring every topic's rate" } else { "stopped measuring" };
                self.flash(state, false);
            }
            Command::ToggleWatch => self.toggle_watch(),
            Command::OpenPublish => match self.active_topic() {
                Some((topic, Some(ty))) => self.open_publish(&topic, &ty),
                Some((topic, None)) => self.flash(format!("{topic}'s type isn't known yet"), true),
                None => self.picker = Some(Picker::publish()),
            },
            Command::PublishAny => self.picker = Some(Picker::publish()),
            Command::RunAction(n) => self.run_action(usize::from(n) - 1),
            Command::RecordBag => self.record_bag(),
            Command::StartAgent => self.pick_ports(true),
            Command::OpenSerial => self.pick_ports(false),
            Command::OpenCan => {
                let ports = bench::can_ports();
                if ports.is_empty() {
                    self.flash("no CAN interfaces here (can0, vcan0, ...)", true);
                } else {
                    let items = ports.iter().map(|p| option(p, "dump")).collect();
                    self.picker = Some(Picker::options("Dump which CAN bus?", items));
                    self.picking = Some(Pick::Can(ports));
                }
            }
            Command::SaveBench => self.save_bench(),
            Command::Simulate => self.pick_firmware(),
            Command::NewRobot => self.open_builder(),
            Command::Flash => self.pick_flash(),
            Command::OpenIsolated => self.open_isolated(None),
            Command::CopyRosEnv => match std::env::var("ROS_DISCOVERY_SERVER") {
                Ok(server) => {
                    ctx.copy_text(format!("export ROS_DISCOVERY_SERVER={server} ROS_SUPER_CLIENT=TRUE"));
                    self.flash("copied; paste it into a terminal, then use ros2 with --no-daemon", false);
                }
                Err(_) => {
                    self.flash("this window is on the usual ROS network; a terminal needs nothing to join it", false)
                }
            },
            Command::ShowWelcome => self.show_view(View::Welcome, false),
            Command::SaveLayout => match &self.state_file {
                Some(_) => self.naming_layout = Some(String::new()),
                None => self.flash("layouts can't be saved here", true),
            },
            Command::LoadLayout => {
                let names = self.state_file.as_deref().map(|f| persist::layout_names(&persist::layouts_dir(f)));
                match names {
                    Some(names) if !names.is_empty() => self.picker = Some(Picker::layouts(names)),
                    _ => self.flash("no saved layouts yet; save one with Save layout as", false),
                }
            }
            _ => self.run_in_view(command, ctx),
        }
    }

    /// The topic the active pane is about: its echo, or the table's selection
    fn active_topic(&self) -> Option<(String, Option<String>)> {
        match self.layout.active_view() {
            View::Source(id) if self.topic_states.contains_key(&id) => {
                let entry = &self.sup.entries()[id];
                let topic = self.graph.topics.iter().find(|t| t.name == entry.name);
                Some((entry.name.clone(), topic.and_then(|t| t.types.first().cloned())))
            }
            View::Topics => {
                let rows = self.table.arrange(self.topic_rows());
                rows.get(self.table.selected).map(|r| (r.name.clone(), Some(r.ty.clone())))
            }
            _ => None,
        }
    }

    /// Adds the active topic to the health strip, or takes it off
    fn toggle_watch(&mut self) {
        let Some((topic, _)) = self.active_topic() else {
            self.flash("watch works on a topic: select one in the topics table or open its echo", false);
            return;
        };
        match self.watches.iter().position(|w| w.watcher.topic() == topic) {
            Some(at) => {
                let mut watch = self.watches.remove(at);
                if let Ok(link) = &self.ros {
                    watch.watcher.stop(&link.ros);
                }
                self.flash(format!("stopped watching {topic}"), false);
            }
            None => {
                self.watches.push(Watch { watcher: Watcher::new(&topic), min_hz: None });
                self.flash(format!("watching {topic}; set min_hz with a [[watch]] in the profile"), false);
            }
        }
    }

    /// Runs a command on the view in the active pane
    fn run_in_view(&mut self, command: Command, ctx: &Context) {
        match self.layout.active_view() {
            View::Source(id) if id >= self.sup.entries().len() => {}
            View::Source(id) if self.showing_bytes(id) => {
                let topic = self.topic_states[&id].clone();
                let topic = topic.lock().unwrap();
                let state = &mut self.bytes[id];
                match command {
                    Command::ToggleStream => self.views[id].mode = TopicMode::Inspector,
                    Command::ScrollDown => state.move_selection(&topic, 1),
                    Command::ScrollUp => state.move_selection(&topic, -1),
                    Command::PageDown => state.move_selection(&topic, PAGE),
                    Command::PageUp => state.move_selection(&topic, -PAGE),
                    Command::Top => state.move_selection(&topic, -(crate::bytes::KEEP as isize)),
                    Command::Bottom => state.follow(),
                    Command::CopyOutput => {
                        if let Some(message) = state.selected(&topic) {
                            ctx.copy_text(bytes::hex_dump(&message.bytes));
                            let number = message.number;
                            drop(topic);
                            self.flash(format!("copied message #{number} as hex"), false);
                        }
                    }
                    _ => {}
                }
            }
            View::Source(id) if self.inspecting(id) => match command {
                Command::ToggleStream => self.views[id].mode = TopicMode::Stream,
                Command::Start | Command::Stop | Command::Restart | Command::ClearView | Command::Remove => {
                    self.source_command(command, id, ctx)
                }
                _ => self.inspector_command(command, id, ctx),
            },
            View::Source(id) => match command {
                Command::ToggleStream if self.topic_states.contains_key(&id) => {
                    self.views[id].mode = self.views[id].mode.next();
                }
                Command::ToggleStream => self.flash("only topics have an inspector", false),
                Command::Start
                | Command::Stop
                | Command::Restart
                | Command::ClearView
                | Command::FocusInput
                | Command::Remove => self.source_command(command, id, ctx),
                _ => {
                    let flash = log_command(command, &mut self.views[id], &self.sup.entries()[id].buffer, ctx);
                    self.after_log_command(command, flash);
                }
            },
            View::AllLogs => {
                let rows = Merged { timeline: self.sup.timeline(), entries: self.sup.entries() };
                let flash = log_command(command, &mut self.merged, &rows, ctx);
                self.after_log_command(command, flash);
            }
            View::Topics => self.table_command(command, ctx),
            View::Publish(i) if i < self.publishers.len() => match command {
                Command::Remove => self.ask_remove_publisher(i),
                Command::ClearView => {
                    self.publishers[i].reset(&mut self.registry.lock().unwrap());
                    self.flash("back to the defaults", false);
                }
                Command::Send | Command::ActivateBeside => self.send(i),
                Command::ToggleRepeat => self.toggle_repeat(i),
                Command::ScrollDown => self.publishers[i].move_cursor(1),
                Command::ScrollUp => self.publishers[i].move_cursor(-1),
                Command::PageDown => self.publishers[i].move_cursor(PAGE),
                Command::PageUp => self.publishers[i].move_cursor(-PAGE),
                Command::Activate | Command::Expand => {
                    let registry = self.registry.lock().unwrap();
                    self.publishers[i].edit_at_cursor(&registry);
                }
                _ => {}
            },
            View::Publish(_) => {}
            View::Plot(i) if i < self.plots.len() => match command {
                Command::PlotWider => self.plots[i].wider(true),
                Command::PlotNarrower => self.plots[i].wider(false),
                Command::ToggleFollow => self.plots[i].toggle_pause(),
                Command::ClearView => {
                    for (id, _) in &self.plots[i].series {
                        if let Some(state) = self.topic_states.get(id) {
                            state.lock().unwrap().clear_traces();
                        }
                    }
                }
                _ => {}
            },
            View::RobotBuilder => match command {
                Command::Send => self.save_robot(false),
                Command::ToggleRepeat => self.save_robot(true),
                Command::Remove => {
                    if let Some(builder) = &mut self.builder {
                        builder.remove_selected();
                    }
                }
                Command::ScrollDown | Command::ScrollUp => {
                    if let Some(builder) = &mut self.builder
                        && !builder.boards.is_empty()
                    {
                        let last = builder.boards.len() - 1;
                        builder.selected = match command {
                            Command::ScrollDown => (builder.selected + 1).min(last),
                            _ => builder.selected.saturating_sub(1),
                        };
                    }
                }
                _ => {}
            },
            View::Plot(_) | View::Welcome => {}
        }
    }

    /// Adds a field to the plot on screen, or to a new plot beside the active pane
    fn plot_field(&mut self, id: SourceId, path: &str) {
        let shown = self.current_plot.filter(|i| self.layout.views().contains(&View::Plot(*i)));
        let index = match shown {
            Some(i) => i,
            None => {
                self.plots.push(Plot::new());
                let i = self.plots.len() - 1;
                let pane = self.layout.beside();
                self.layout.set_view(pane, View::Plot(i));
                i
            }
        };
        self.current_plot = Some(index);
        let series = (id, path.to_owned());
        if !self.plots[index].series.contains(&series) {
            self.plots[index].series.push(series);
        }
        if let Some(state) = self.topic_states.get(&id) {
            state.lock().unwrap().trace(path);
        }
        self.flash(format!("plotting {path}"), false);
    }

    /// Starts `ros2 bag record` of every topic into `bags/` beside the profile, as a source of its own. Stopping it
    /// sends Ctrl+C, which closes the bag properly.
    fn record_bag(&mut self) {
        let dir =
            self.profile_path.as_deref().and_then(|p| p.parent()).map_or_else(|| PathBuf::from("."), PathBuf::from);
        let stamp = logview::file_stamp(std::time::SystemTime::now());
        let out = dir.join("bags").join(format!("archon-{stamp}"));
        let spec = archon_core::profile::ProcessSpec {
            name: format!("bag {stamp}"),
            cmd: format!("mkdir -p '{}' && exec ros2 bag record -a -o '{}'", dir.join("bags").display(), out.display()),
            cwd: Some(dir),
            env: Default::default(),
            autostart: false,
            restart: RestartPolicy::Never,
        };
        let source = archon_sources::process::ProcessSource::new(spec.clone(), &self.profile_env);
        let id = self.sup.add(&spec.name, Box::new(source), RestartPolicy::Never, false, "\n");
        self.grow_views();
        self.sup.start(id);
        self.show_view(View::Source(id), true);
        self.flash(format!("recording to {}; x stops it", out.display()), false);
    }

    /// Opens another archon on a ROS network of its own, simulating `image` there if given
    fn open_isolated(&mut self, image: Option<&std::path::Path>) {
        let mut args = vec!["--isolated".to_owned()];
        if let Some(image) = image {
            args.extend(["--simulate".to_owned(), image.display().to_string()]);
        }
        let quiet = image.is_some();
        self.open_window(args, quiet);
    }

    /// Opens another archon with these arguments; `quiet` leaves saying so to the caller
    fn open_window(&mut self, args: Vec<String>, quiet: bool) {
        #[cfg(test)]
        {
            let _ = quiet;
            self.spawned.push(args);
        }
        #[cfg(not(test))]
        {
            use std::os::unix::process::CommandExt;
            let spawned = std::env::current_exe().and_then(|exe| {
                std::process::Command::new(exe)
                    .args(&args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    // Its own process group, so it outlives this window and a Ctrl+C in this one's terminal
                    .process_group(0)
                    .spawn()
            });
            match spawned {
                Ok(_) if !quiet => self.flash("opened an isolated window", false),
                Ok(_) => {}
                Err(e) => self.flash(format!("couldn't open another window: {e}"), true),
            }
        }
    }

    /// Opens an agent or console for a board or bus, and starts it
    fn attach(&mut self, attach: &crate::robot::Attach) -> Option<SourceId> {
        use crate::robot::Attach;
        let process = |name: String, cmd: String| archon_core::profile::ProcessSpec {
            name,
            cmd,
            cwd: None,
            env: Default::default(),
            autostart: true,
            restart: RestartPolicy::OnFailure,
        };
        let spec = match attach {
            Attach::Console { name, path } => {
                let spec = archon_core::profile::SerialSpec {
                    name: self.unique_name(&format!("console {name}")),
                    port: path.display().to_string(),
                    baud: 115_200,
                    autostart: true,
                    line_ending: Default::default(),
                };
                let source = archon_sources::serial::SerialSource::new(spec.clone());
                let id = self.sup.add(&spec.name, Box::new(source), RestartPolicy::Never, true, "\n");
                self.grow_views();
                self.sup.start(id);
                self.added.push(Added::Serial(spec));
                return Some(id);
            }
            Attach::Agent { name, .. } | Attach::UdpAgent { name, .. } => {
                let agent = match self.find_agent() {
                    Ok(agent) => agent,
                    Err(why) => {
                        self.flash(why, true);
                        return None;
                    }
                };
                let cmd = match attach {
                    // A simulated board's terminal appears once the simulator is up
                    Attach::Agent { path, .. } => format!(
                        "while [ ! -e {p} ]; do sleep 0.2; done; {}",
                        bench::agent_command(&agent, &Port::Simulated(path.clone())),
                        p = bench::quote(path)
                    ),
                    Attach::UdpAgent { port, .. } => bench::agent_command(&agent, &Port::Udp(*port)),
                    Attach::Console { .. } => unreachable!(),
                };
                process(self.unique_name(&format!("agent {name}")), cmd)
            }
        };
        let source = archon_sources::process::ProcessSource::new(spec.clone(), &self.profile_env);
        let id = self.sup.add(&spec.name, Box::new(source), spec.restart, true, "\n");
        self.grow_views();
        self.sup.start(id);
        self.added.push(Added::Process(spec));
        Some(id)
    }

    /// Runs a robot file: the simulator with every board, a console per C board, an agent for each USB board here
    /// and one per CAN bus; a second copy of a USB board goes, with its console, to an isolated window
    pub(crate) fn simulate_robot(&mut self, robot: &crate::robot::Robot) {
        let simulator = match self.find_simulator().and_then(|s| self.find_agent().map(|_| s)) {
            Ok(simulator) => simulator,
            Err(why) => return self.flash(why, true),
        };
        if let Some(why) = robot.problem() {
            return self.flash(format!("can't run {}: {why}", robot.name), true);
        }
        let base: String =
            robot.name.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
        let slug = (1..)
            .map(|n| if n == 1 { base.clone() } else { format!("{base}{n}") })
            .find(|s| !self.sup.entries().iter().any(|e| !e.removed && e.name == format!("robot {s}")))
            .expect("unbounded");
        let running: Vec<PathBuf> = self
            .simulated
            .iter()
            .filter(|(id, _)| {
                let entry = &self.sup.entries()[*id];
                !entry.removed && entry.status.is_active()
            })
            .map(|(_, board)| board.clone())
            .collect();
        let mut taken = Vec::new();
        let plan = crate::robot::plan(&simulator, robot, &slug, &running, || {
            let port = crate::robot::free_udp_port(&taken);
            taken.push(port);
            port
        });

        let spec = archon_core::profile::ProcessSpec {
            name: format!("robot {slug}"),
            cmd: plan.command.clone(),
            cwd: Some(simulator.checkout.clone()),
            env: Default::default(),
            autostart: true,
            restart: RestartPolicy::Never,
        };
        let source = archon_sources::process::ProcessSource::new(spec.clone(), &self.profile_env);
        let sim = self.sup.add(&spec.name, Box::new(source), spec.restart, true, "\n");
        self.grow_views();
        self.sup.start(sim);
        self.added.push(Added::Process(spec));
        // The USB boards running here, so another copy of one goes elsewhere too
        for attach in &plan.here {
            if let crate::robot::Attach::Agent { name, .. } = attach
                && let Some(board) = robot.boards.iter().find(|b| b.name == *name)
            {
                self.simulated.push((sim, board.board.clone()));
            }
        }
        let mut first_console = None;
        for attach in &plan.here {
            let id = self.attach(attach);
            if first_console.is_none() && matches!(attach, crate::robot::Attach::Console { .. }) {
                first_console = id;
            }
        }
        for window in &plan.windows {
            let mut args = vec!["--isolated".to_owned()];
            args.extend(window.iter().flat_map(|a| a.args()));
            self.open_window(args, true);
        }
        self.show_view(View::Source(first_console.unwrap_or(sim)), false);
        let elsewhere = match plan.windows.len() {
            0 => String::new(),
            1 => "; a second copy of a board runs in an isolated window".to_owned(),
            n => format!("; further copies of its boards run in {n} isolated windows"),
        };
        self.flash(format!("running {} ({}){elsewhere}", robot.name, robot.board_names()), false);
    }

    /// Removes a source, asking first whether to take it out of the profile too when it's in there
    fn ask_remove(&mut self, id: SourceId) {
        let name = self.sup.entries()[id].name.clone();
        let in_profile =
            self.profile_path.as_deref().and_then(|p| Profile::load(p).ok()).is_some_and(|p| p.defines(&name));
        if in_profile {
            self.removing = Some((Removal::Source(id), name));
        } else {
            self.remove_source(id, false);
        }
    }

    /// Takes a source off the bench, and out of the profile file with `from_profile`
    fn remove_source(&mut self, id: SourceId, from_profile: bool) {
        let name = self.sup.entries()[id].name.clone();
        self.sup.remove(id);
        self.added.retain(|added| {
            let added_name = match added {
                Added::Process(p) => &p.name,
                Added::Serial(s) => &s.name,
                Added::Can(c) => &c.name,
            };
            *added_name != name
        });
        self.topic_states.remove(&id);
        for plot in &mut self.plots {
            plot.series.retain(|(source, _)| *source != id);
        }
        let fallback = match self.sup.live().next() {
            Some((other, _)) => View::Source(other),
            None => View::Welcome,
        };
        self.layout.replace_everywhere(View::Source(id), fallback);
        let mut note = format!("removed {name}");
        if from_profile && let Some(path) = self.profile_path.clone() {
            match Profile::remove_from_file(&path, &name) {
                Ok(_) => note.push_str(&format!(" here and from {}", path.display())),
                Err(e) => {
                    return self
                        .flash(format!("removed {name} here, but couldn't change {}: {e:#}", path.display()), true);
                }
            }
        } else if self.profile_path.as_deref().and_then(|p| Profile::load(p).ok()).is_some_and(|p| p.defines(&name)) {
            note.push_str("; it's still in the profile, so it comes back next time");
        }
        self.flash(note, false);
    }

    /// The bar asking whether a removal takes the source out of the profile too
    fn removal_bar(&mut self, ui: &mut Ui) {
        let Some((what, name)) = self.removing.clone() else { return };
        let (yes, no, escape) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Y) | i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::N),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let profile = self
            .profile_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut choice = if yes {
            Some(true)
        } else if no {
            Some(false)
        } else {
            None
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Remove {name}? It's in {profile}.")).strong());
            if ui.button(format!("Remove from {profile} too (y)")).clicked() {
                choice = Some(true);
            }
            if ui.button("Just for now (n)").clicked() {
                choice = Some(false);
            }
            if ui.button("Cancel (Esc)").clicked() {
                self.removing = None;
            }
        });
        if escape {
            self.removing = None;
        }
        if let Some(from_profile) = choice {
            self.removing = None;
            match what {
                Removal::Source(id) => self.remove_source(id, from_profile),
                Removal::Publisher(i) => self.remove_publisher(i, from_profile),
            }
        }
    }

    /// Makes a publisher's ROS publisher now, so the board's subscription has found it by the first Send
    fn advertise(&self, index: usize) {
        let publisher = &self.publishers[index];
        if let (Ok(link), Ok(_)) = (&self.ros, &publisher.value) {
            link.ros.advertise(&publisher.topic, &publisher.ty);
        }
    }

    /// Removes a publisher, asking first whether to take it out of the profile too when it's a `[[publish]]` there
    pub(crate) fn ask_remove_publisher(&mut self, index: usize) {
        let Some(publisher) = self.publishers.get(index).filter(|p| !p.removed) else { return };
        if publisher.in_profile && self.profile_path.is_some() {
            self.removing = Some((Removal::Publisher(index), publisher.name.clone()));
        } else {
            self.remove_publisher(index, false);
        }
    }

    /// Takes a publisher off the bench, and its `[[publish]]` out of the profile file with `from_profile`
    fn remove_publisher(&mut self, index: usize, from_profile: bool) {
        let publisher = &mut self.publishers[index];
        publisher.removed = true;
        publisher.repeating = false;
        let (name, in_profile) = (publisher.name.clone(), publisher.in_profile);
        let fallback = self.layout.views().iter().copied().find(|v| *v != View::Publish(index)).unwrap_or(
            match self.sup.live().next() {
                Some((id, _)) => View::Source(id),
                None => View::Welcome,
            },
        );
        self.layout.replace_everywhere(View::Publish(index), fallback);
        let mut note = format!("removed {name}");
        if from_profile && let Some(path) = self.profile_path.clone() {
            match Profile::remove_publish_from_file(&path, &name) {
                Ok(_) => note.push_str(&format!(" here and from {}", path.display())),
                Err(e) => {
                    return self
                        .flash(format!("removed {name} here, but couldn't change {}: {e:#}", path.display()), true);
                }
            }
        } else if in_profile {
            note.push_str("; it's still in the profile, so it comes back next time");
        }
        self.flash(note, false);
    }

    /// Lists the ports to run the agent on (serial, CAN and UDP), or to open a console on (serial)
    fn pick_ports(&mut self, agent: bool) {
        if agent && let Err(why) = self.find_agent() {
            self.flash(why, true);
            return;
        }
        let mut ports = bench::serial_ports();
        ports.extend(bench::simulated_ports());
        if agent {
            ports.extend(bench::can_ports());
            ports.push(Port::Udp(8888));
        }
        if ports.is_empty() {
            self.flash(
                "no serial ports are plugged in, and no simulated board is running (Space n f simulates one)",
                true,
            );
            return;
        }
        // The likely port first (the micro-ROS link for the agent, the console for a console), so Enter takes it
        let likely = bench::rank(&mut ports, agent, bench::usb_interface);
        let (prompt, hint) =
            if agent { ("Run the micro-ROS agent on…", "agent") } else { ("Open a console on…", "console") };
        let items =
            ports.iter().enumerate().map(|(i, p)| option(p, if i < likely { "likely" } else { hint })).collect();
        self.picker = Some(Picker::options(prompt, items));
        self.picking = Some(if agent { Pick::Agent(ports) } else { Pick::Serial(ports) });
    }

    fn find_simulator(&self) -> Result<bench::Simulator, String> {
        let home = dirs::home_dir().unwrap_or_default();
        bench::find_simulator(self.config.firmware_simulator.as_deref(), &home)
    }

    /// Lists the firmware builds to simulate, newest first
    fn pick_firmware(&mut self) {
        let simulator = match self.find_simulator().and_then(|s| self.find_agent().map(|_| s)) {
            Ok(simulator) => simulator,
            Err(why) => return self.flash(why, true),
        };
        let home = dirs::home_dir().unwrap_or_default();
        let robots = crate::robot::robots(&simulator, crate::robot::titan_firmware_dir(&home).as_deref());
        let builds = bench::firmware_builds(&home);
        if builds.is_empty() && robots.is_empty() {
            return self
                .flash("no titan_firmware builds found (looked for *_with_bl.uf2 under ~/dev/*/titan_firmware)", true);
        }
        let ago = |built: Option<std::time::SystemTime>, what: &str| {
            built.and_then(|t| t.elapsed().ok()).map(|age| {
                let secs = age.as_secs();
                match secs {
                    0..3600 => format!("{what} {} min ago", secs / 60),
                    3600..86400 => format!("{what} {} h ago", secs / 3600),
                    _ => format!("{what} {} days ago", secs / 86400),
                }
            })
        };
        let mut items = vec![OptionItem {
            label: "New robot…".to_owned(),
            detail: "several boards in one simulation: pick them, archon writes the file".to_owned(),
            hint: self.key_of(Command::NewRobot).unwrap_or_default(),
        }];
        let mut choices = vec![Simulatable::NewRobot];
        for robot in robots {
            let hint = match robot.problem() {
                Some(_) => "not built".to_owned(),
                None => ago(robot.modified, "robot, edited").unwrap_or_else(|| "robot".to_owned()),
            };
            items.push(OptionItem {
                label: match robot.boards.len() {
                    1 => format!("{} (1 board)", robot.name),
                    n => format!("{} ({n} boards)", robot.name),
                },
                detail: robot.board_names(),
                hint,
            });
            choices.push(Simulatable::Robot(robot));
        }
        for build in builds {
            let dir = build.image.parent().map(|d| d.display().to_string()).unwrap_or_default();
            items.push(OptionItem {
                label: build.name.clone(),
                detail: dir,
                hint: ago(build.built, "built").unwrap_or_default(),
            });
            choices.push(Simulatable::Build(build));
        }
        self.picker = Some(Picker::options("Simulate a robot file, or one firmware build", items));
        self.picking = Some(Pick::Simulate(choices));
    }

    fn flash_tool(&self) -> Result<crate::flash::Tool, String> {
        let home = dirs::home_dir().unwrap_or_default();
        crate::flash::find(self.config.upload_tool.as_deref(), crate::robot::titan_firmware_dir(&home).as_deref())
    }

    /// Lists the builds to flash, with what each one is
    fn pick_flash(&mut self) {
        let tool = match self.flash_tool() {
            Ok(tool) => tool,
            Err(why) => return self.flash(why, true),
        };
        let builds = bench::firmware_builds(&dirs::home_dir().unwrap_or_default());
        if builds.is_empty() {
            return self
                .flash("no titan_firmware builds found (looked for *_with_bl.uf2 under ~/dev/*/titan_firmware)", true);
        }
        let items = builds
            .iter()
            .map(|build| {
                let info = crate::flash::info(&tool, &build.image).unwrap_or_default();
                let ago = build.built.and_then(|t| t.elapsed().ok()).map(|age| match age.as_secs() {
                    s @ 0..3600 => format!("built {} min ago", s / 60),
                    s @ 3600..86400 => format!("built {} h ago", s / 3600),
                    s => format!("built {} days ago", s / 86400),
                });
                let detail =
                    [info.board, info.source].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                OptionItem {
                    label: build.name.clone(),
                    detail: if detail.is_empty() { build.image.display().to_string() } else { detail },
                    hint: ago.unwrap_or_default(),
                }
            })
            .collect();
        self.picker = Some(Picker::options(&format!("Flash which build? ({})", tool.name()), items));
        self.picking = Some(Pick::Flash(builds));
    }

    /// Asks whether to flash the application or the whole image
    fn pick_flash_mode(&mut self, firmware: &bench::Firmware) {
        let images = crate::flash::images(firmware);
        let items = images
            .iter()
            .map(|(mode, image)| {
                let file = image.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                match mode {
                    crate::flash::Mode::App => OptionItem {
                        label: "The application".to_owned(),
                        detail: format!("{file} · keeps the board's bootloader"),
                        hint: "usual".to_owned(),
                    },
                    crate::flash::Mode::Full => OptionItem {
                        label: "Everything, bootloader too".to_owned(),
                        detail: format!("{file} · a new board, or one without the titan bootloader"),
                        hint: "-f".to_owned(),
                    },
                }
            })
            .collect();
        self.picker = Some(Picker::options(&format!("Flash how much of {}?", firmware.name), items));
        self.picking = Some(Pick::FlashMode(firmware.name.clone(), images));
    }

    /// Flashes an image as a source of its own, whose output shows how it goes
    pub(crate) fn flash_board(&mut self, name: &str, image: &std::path::Path, mode: crate::flash::Mode) {
        let tool = match self.flash_tool() {
            Ok(tool) => tool,
            Err(why) => return self.flash(why, true),
        };
        // An earlier flash that's done makes way, so they don't pile up in the sidebar
        let done: Vec<SourceId> = self
            .flashes
            .iter()
            .copied()
            .filter(|id| {
                let entry = &self.sup.entries()[*id];
                !entry.removed && !entry.status.is_active()
            })
            .collect();
        for id in done {
            self.remove_source(id, false);
        }
        self.flashes.retain(|id| !self.sup.entries()[*id].removed);
        let cwd = match &tool {
            // upload_tool reads its device list from beside it
            crate::flash::Tool::UploadTool(binary) => binary.parent().map(std::path::Path::to_path_buf),
            crate::flash::Tool::Picotool(_) => None,
        };
        let spec = archon_core::profile::ProcessSpec {
            name: self.unique_name(&format!("flash {name}")),
            cmd: crate::flash::command(&tool, image, mode),
            cwd,
            env: Default::default(),
            autostart: true,
            restart: RestartPolicy::Never,
        };
        let source = archon_sources::process::ProcessSource::new(spec.clone(), &self.profile_env);
        let id = self.sup.add(&spec.name, Box::new(source), RestartPolicy::Never, true, "\n");
        self.grow_views();
        self.sup.start(id);
        self.flashes.push(id);
        self.show_view(View::Source(id), false);
        self.flash(format!("flashing {name} with {}; consoles reconnect after", tool.name()), false);
    }

    /// Puts a question a flash is waiting on (which board? which interface?) to the user as a picker
    fn answer_flash_questions(&mut self) {
        if self.picker.is_some() {
            return;
        }
        for id in self.flashes.clone() {
            let entry = &self.sup.entries()[id];
            if entry.removed || !entry.status.is_active() {
                continue;
            }
            let line = entry.buffer.total();
            if self.asked.get(&id) == Some(&line) {
                continue;
            }
            let lines: Vec<String> = entry.buffer.iter().map(|l| l.text.clone()).collect();
            let Some(question) = crate::flash::question(&lines) else { continue };
            self.asked.insert(id, line);
            let name = entry.name.clone();
            let items = question
                .choices
                .iter()
                .map(|c| OptionItem { label: c.name.clone(), detail: c.detail.clone(), hint: c.reply.clone() })
                .collect();
            let replies = question.choices.iter().map(|c| c.reply.clone()).collect();
            self.picker = Some(Picker::options(&format!("{name}: {} (Esc calls it off)", question.prompt), items));
            self.picking = Some(Pick::FlashAnswer(id, replies));
            return;
        }
    }

    /// Shows the robot file being written, starting one if there isn't one
    fn open_builder(&mut self) {
        let simulator = match self.find_simulator() {
            Ok(simulator) => simulator,
            Err(why) => return self.flash(why, true),
        };
        if self.builder.is_none() {
            self.builder = Some(crate::builder::Builder::new(&simulator));
        }
        self.show_view(View::RobotBuilder, false);
    }

    /// Lists the firmware builds to add to the robot being written
    fn pick_robot_board(&mut self) {
        let builds = bench::firmware_builds(&dirs::home_dir().unwrap_or_default());
        if builds.is_empty() {
            return self
                .flash("no titan_firmware builds found (looked for *_with_bl.uf2 under ~/dev/*/titan_firmware)", true);
        }
        let items = builds
            .iter()
            .map(|f| {
                let transport = match crate::robot::transport(&f.image) {
                    crate::robot::Transport::Usb => "micro-ROS over USB",
                    crate::robot::Transport::Can => "micro-ROS over CAN",
                    crate::robot::Transport::Other => "",
                };
                OptionItem {
                    label: f.name.clone(),
                    detail: f.image.parent().map(|d| d.display().to_string()).unwrap_or_default(),
                    hint: transport.to_owned(),
                }
            })
            .collect();
        self.picker = Some(Picker::options("Add which build to the robot?", items));
        self.picking = Some(Pick::RobotBoard(builds));
    }

    /// Saves the robot being written, and runs it with `run`
    fn save_robot(&mut self, run: bool) {
        let home = dirs::home_dir().unwrap_or_default();
        let Some(builder) = &mut self.builder else { return };
        match builder.save(&home) {
            Err(why) => self.flash(why, true),
            Ok(path) if run => match crate::robot::load(&path, crate::robot::titan_firmware_dir(&home).as_deref()) {
                Ok(robot) => self.simulate_robot(&robot),
                Err(why) => self.flash(why, true),
            },
            Ok(path) => self.flash(format!("saved {}; run it from Space n f", path.display()), false),
        }
    }

    /// Runs a build in the simulator, with the agent on its micro-ROS port and a console on its log port
    fn simulate(&mut self, firmware: &bench::Firmware) {
        let (simulator, agent) = match self.find_simulator().and_then(|s| self.find_agent().map(|a| (s, a))) {
            Ok(found) => found,
            Err(why) => return self.flash(why, true),
        };
        let Some(board) = bench::board_for(&simulator, &firmware.image) else {
            return self.flash(
                format!(
                    "couldn't tell which board {} is for: none of {}/boards matches it",
                    firmware.name,
                    simulator.checkout.display()
                ),
                true,
            );
        };
        // Another copy of a board publishes the same topics; on the same network the two would mix, so it gets a
        // window of its own
        let running_here = self.simulated.iter().any(|(id, other)| {
            let entry = &self.sup.entries()[*id];
            *other == board && !entry.removed && entry.status.is_active()
        });
        if running_here {
            self.open_isolated(Some(&firmware.image));
            return self.flash(
                format!(
                    "a {} is already simulated here, so this one opens in an isolated window: the same topics in one \
                     window would mix",
                    board.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
                ),
                false,
            );
        }
        let base: String =
            firmware.name.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
        let slug = (1..)
            .map(|n| if n == 1 { base.clone() } else { format!("{base}{n}") })
            .find(|s| !self.sup.entries().iter().any(|e| !e.removed && e.name == format!("sim {s}")))
            .expect("unbounded");
        let (sim_cmd, agent_cmd, log) = bench::simulation_commands(&simulator, firmware, &board, &agent, &slug);
        let process = |name: String, cmd: String, cwd: Option<PathBuf>, restart| archon_core::profile::ProcessSpec {
            name,
            cmd,
            cwd,
            env: Default::default(),
            autostart: true,
            restart,
        };
        let sim = process(format!("sim {slug}"), sim_cmd, Some(simulator.checkout.clone()), RestartPolicy::Never);
        let agent = process(format!("agent {slug}"), agent_cmd, None, RestartPolicy::OnFailure);
        let console = archon_core::profile::SerialSpec {
            name: format!("console {slug}"),
            port: log.display().to_string(),
            baud: 115_200,
            autostart: true,
            line_ending: Default::default(),
        };
        for (i, spec) in [sim, agent].into_iter().enumerate() {
            let source = archon_sources::process::ProcessSource::new(spec.clone(), &self.profile_env);
            let id = self.sup.add(&spec.name, Box::new(source), spec.restart, true, "\n");
            self.grow_views();
            self.sup.start(id);
            self.added.push(Added::Process(spec));
            if i == 0 {
                self.simulated.push((id, board.clone()));
            }
        }
        let id = self.sup.add(
            &console.name,
            Box::new(archon_sources::serial::SerialSource::new(console.clone())),
            RestartPolicy::Never,
            true,
            "\n",
        );
        self.grow_views();
        self.sup.start(id);
        self.added.push(Added::Serial(console));
        self.show_view(View::Source(id), false);
        self.flash(
            format!("simulating {} on {}; its topics show once the agent connects", firmware.name, board.display()),
            false,
        );
    }

    fn find_agent(&self) -> Result<bench::Agent, String> {
        let home = dirs::home_dir().unwrap_or_default();
        bench::find_agent(self.config.micro_ros_agent.as_deref(), &home)
    }

    /// A source name not taken yet: `agent`, then `agent 2`, ...
    fn unique_name(&self, base: &str) -> String {
        let taken = |name: &str| self.sup.entries().iter().any(|e| !e.removed && e.name == name);
        (1..)
            .map(|n| if n == 1 { base.to_owned() } else { format!("{base} {n}") })
            .find(|n| !taken(n))
            .expect("unbounded")
    }

    /// Opens what was picked: the agent on a port, a console, or a CAN dump
    fn picked(&mut self, index: usize) {
        let Some(pick) = self.picking.take() else { return };
        let (spec, source): (Added, Box<dyn archon_core::source::Source>) = match pick {
            Pick::Agent(ports) => {
                let Some(port) = ports.get(index) else { return };
                let agent = match self.find_agent() {
                    Ok(agent) => agent,
                    Err(why) => return self.flash(why, true),
                };
                let spec = archon_core::profile::ProcessSpec {
                    name: self.unique_name("agent"),
                    cmd: bench::agent_command(&agent, port),
                    cwd: None,
                    env: Default::default(),
                    autostart: true,
                    restart: RestartPolicy::OnFailure,
                };
                let source = archon_sources::process::ProcessSource::new(spec.clone(), &self.profile_env);
                (Added::Process(spec), Box::new(source))
            }
            Pick::Flash(builds) => {
                if let Some(firmware) = builds.get(index).cloned() {
                    self.pick_flash_mode(&firmware);
                }
                return;
            }
            Pick::FlashAnswer(id, replies) => {
                if let Some(reply) = replies.get(index) {
                    let _ = self.sup.write(id, format!("{reply}\n").as_bytes());
                }
                return;
            }
            Pick::FlashMode(name, images) => {
                if let Some((mode, image)) = images.get(index).cloned() {
                    self.flash_board(&name, &image, mode);
                }
                return;
            }
            Pick::RobotBoard(builds) => {
                let Some(firmware) = builds.get(index).cloned() else { return };
                let board = self.find_simulator().ok().and_then(|s| bench::board_for(&s, &firmware.image));
                if let Some(builder) = &mut self.builder {
                    builder.add(firmware, board);
                }
                self.show_view(View::RobotBuilder, false);
                return;
            }
            Pick::Simulate(choices) => {
                match choices.get(index) {
                    Some(Simulatable::Build(firmware)) => self.simulate(&firmware.clone()),
                    Some(Simulatable::Robot(robot)) => self.simulate_robot(&robot.clone()),
                    Some(Simulatable::NewRobot) => self.open_builder(),
                    None => {}
                }
                return;
            }
            Pick::Serial(ports) => {
                let Some(path) = ports.get(index).and_then(Port::path) else { return };
                let spec = archon_core::profile::SerialSpec {
                    name: self.unique_name("console"),
                    port: path.display().to_string(),
                    baud: 115_200,
                    autostart: true,
                    line_ending: Default::default(),
                };
                (Added::Serial(spec.clone()), Box::new(archon_sources::serial::SerialSource::new(spec)))
            }
            Pick::Can(ports) => {
                let Some(Port::Can(interface)) = ports.get(index) else { return };
                let spec = archon_core::profile::CanSpec {
                    name: self.unique_name(interface),
                    interface: interface.clone(),
                    autostart: true,
                };
                (Added::Can(spec.clone()), Box::new(archon_sources::can::CanSource::new(spec)))
            }
        };
        let (name, restart, ending) = match &spec {
            Added::Process(p) => (p.name.clone(), p.restart, "\n"),
            Added::Serial(s) => (s.name.clone(), RestartPolicy::Never, s.line_ending.as_str()),
            Added::Can(c) => (c.name.clone(), RestartPolicy::Never, ""),
        };
        let id = self.sup.add(&name, source, restart, true, ending);
        self.grow_views();
        self.sup.start(id);
        self.added.push(spec);
        self.show_view(View::Source(id), false);
        self.flash(format!("started {name}; Space n w saves the bench"), false);
    }

    /// Writes what's been opened into the profile, or into a new `archon.toml` here
    fn save_bench(&mut self) {
        if self.added.is_empty() {
            let place = self.profile_path.as_ref().map_or("a profile".to_owned(), |p| p.display().to_string());
            self.flash(format!("nothing new to save; everything open is already in {place}"), false);
            return;
        }
        #[derive(serde::Serialize, Default)]
        struct Entries<'a> {
            #[serde(rename = "process", skip_serializing_if = "Vec::is_empty")]
            processes: Vec<&'a archon_core::profile::ProcessSpec>,
            #[serde(rename = "serial", skip_serializing_if = "Vec::is_empty")]
            serials: Vec<&'a archon_core::profile::SerialSpec>,
            #[serde(rename = "can", skip_serializing_if = "Vec::is_empty")]
            cans: Vec<&'a archon_core::profile::CanSpec>,
        }
        let mut entries = Entries::default();
        for added in &self.added {
            match added {
                Added::Process(p) => entries.processes.push(p),
                Added::Serial(s) => entries.serials.push(s),
                Added::Can(c) => entries.cans.push(c),
            }
        }
        let text = match toml::to_string(&entries) {
            Ok(text) => text,
            Err(e) => return self.flash(format!("couldn't write the bench: {e}"), true),
        };
        let path = self.profile_path.clone().unwrap_or_else(|| PathBuf::from(FILE_NAME));
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let separator = if existing.is_empty() || existing.ends_with("\n\n") {
            ""
        } else if existing.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let header = if existing.is_empty() {
            "# An archon bench: run `archon` in this folder to bring it back\n\n"
        } else {
            ""
        };
        match std::fs::write(&path, format!("{existing}{separator}{header}{text}")) {
            Ok(()) => {
                let saved = self.added.len();
                self.added.clear();
                if self.profile_path.is_none() {
                    let absolute = path.canonicalize().unwrap_or(path.clone());
                    self.state_file = persist::state_file(Some(&absolute));
                    self.profile_path = Some(absolute);
                }
                self.flash(format!("saved {saved} to {}", path.display()), false);
            }
            Err(e) => self.flash(format!("couldn't write {}: {e}", path.display()), true),
        }
    }

    /// Takes a series off a plot, and stops recording it if no plot shows it
    fn unplot(&mut self, plot: usize, series: usize) {
        let (id, path) = self.plots[plot].series.remove(series);
        let still_shown = self.plots.iter().any(|p| p.series.contains(&(id, path.clone())));
        if !still_shown && let Some(state) = self.topic_states.get(&id) {
            state.lock().unwrap().untrace(&path);
        }
    }

    /// Opens the publish form for a topic, reusing one already open for it
    pub(crate) fn open_publish(&mut self, topic: &str, ty: &str) {
        let index =
            match self.publishers.iter().position(|p| p.topic == topic && p.ty == ty && !p.in_profile && !p.removed) {
                Some(i) => i,
                None => {
                    let publisher = Publisher::new(&mut self.registry.lock().unwrap(), topic, topic, ty);
                    self.publishers.push(publisher);
                    self.advertise(self.publishers.len() - 1);
                    self.publishers.len() - 1
                }
            };
        self.show_view(View::Publish(index), false);
    }

    /// Runs a profile action: sends it once, or for one with a rate, starts or stops sending
    pub(crate) fn run_action(&mut self, index: usize) {
        let Some(publisher) = self.publishers.get(index).filter(|p| !p.removed) else { return };
        if publisher.repeats || publisher.repeating {
            self.toggle_repeat(index);
            return;
        }
        let name = publisher.name.clone();
        let before = publisher.sent;
        self.send(index);
        if self.publishers[index].sent > before {
            self.flash(format!("sent {name}"), false);
        }
    }

    fn send(&mut self, index: usize) {
        let publisher = &self.publishers[index];
        let bytes = publisher.encode(&self.registry.lock().unwrap());
        match (&self.ros, bytes) {
            (Err(why), _) => self.flash(format!("ROS isn't running: {why}"), true),
            (_, Err(why)) => self.flash(format!("can't send {}: {why}", publisher.name), true),
            (Ok(link), Ok(bytes)) => {
                link.ros.publish(&publisher.topic, &publisher.ty, bytes);
                let publisher = &mut self.publishers[index];
                publisher.sent += 1;
                publisher.last_sent = Some(Instant::now());
            }
        }
    }

    fn toggle_repeat(&mut self, index: usize) {
        let publisher = &mut self.publishers[index];
        if publisher.repeating {
            publisher.repeating = false;
            let name = publisher.name.clone();
            self.flash(format!("stopped sending {name}"), false);
        } else if publisher.rate().is_none() {
            self.flash("set a rate above 0 Hz first", true);
        } else if self.ros.is_err() {
            self.flash("ROS isn't running", true);
        } else {
            publisher.repeating = true;
            publisher.last_sent = None;
        }
    }

    /// Sends what's repeating and due, and asks to be woken for the next
    fn send_repeating(&mut self, ctx: &Context) {
        for i in 0..self.publishers.len() {
            if !self.publishers[i].repeating {
                continue;
            }
            match self.publishers[i].due() {
                Some(wait) if wait.is_zero() => self.send(i),
                Some(wait) => ctx.request_repaint_after(wait),
                None => self.publishers[i].repeating = false,
            }
        }
        if let Ok(link) = &self.ros {
            for error in link.ros.take_errors() {
                self.flash(error, true);
            }
        }
    }

    /// Appends a publisher to the profile as a `[[publish]]` action
    fn save_to_profile(&mut self, index: usize) {
        let Some(path) = self.profile_path.clone() else {
            self.flash("there's no profile file to save into", true);
            return;
        };
        let entry = match self.publishers[index].profile_entry() {
            Ok(entry) => entry,
            Err(why) => return self.flash(why, true),
        };
        let appended = std::fs::read_to_string(&path).and_then(|text| {
            let separator = if text.is_empty() || text.ends_with("\n\n") {
                ""
            } else if text.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            std::fs::write(&path, format!("{text}{separator}{entry}"))
        });
        match appended {
            Ok(()) => {
                self.publishers[index].in_profile = true;
                let name = self.publishers[index].name.clone();
                self.flash(format!("saved {name} to {}", path.display()), false);
            }
            Err(e) => self.flash(format!("couldn't write {}: {e}", path.display()), true),
        }
    }

    pub(crate) fn action_items(&self) -> Vec<ActionItem> {
        self.publishers
            .iter()
            .enumerate()
            .filter(|(_, p)| p.in_profile && !p.removed)
            .map(|(index, p)| ActionItem { index, name: p.name.clone(), topic: p.topic.clone(), key: p.key.clone() })
            .collect()
    }

    fn after_log_command(&mut self, command: Command, flash: Option<String>) {
        if command == Command::Search {
            self.searching = true;
        }
        if command == Command::ClearSearch {
            self.searching = false;
        }
        if let Some(text) = flash {
            self.flash(text, false);
        }
    }

    /// Runs a command that acts on a source itself
    pub(crate) fn source_command(&mut self, command: Command, id: SourceId, _ctx: &Context) {
        match command {
            Command::Remove => self.ask_remove(id),
            Command::Start => self.sup.start(id),
            Command::Stop => self.sup.stop(id),
            Command::Restart => self.sup.restart(id),
            Command::ClearView => self.sup.clear(id),
            Command::FocusInput if self.sup.entries()[id].kind == SourceKind::Topic => {
                self.flash("publishing to a topic isn't supported yet", false);
            }
            Command::FocusInput => {
                self.input = Some(String::new());
                self.history_at = None;
            }
            _ => {}
        }
    }

    fn inspector_command(&mut self, command: Command, id: SourceId, ctx: &Context) {
        let topic = self.topic_states[&id].clone();
        let topic = topic.lock().unwrap();
        let Some(latest) = &topic.latest else { return };
        let state = &mut self.inspectors[id];
        let nodes = state.nodes(latest);
        match command {
            Command::ScrollDown => state.move_cursor(1, nodes.len()),
            Command::ScrollUp => state.move_cursor(-1, nodes.len()),
            Command::PageDown => state.move_cursor(PAGE, nodes.len()),
            Command::PageUp => state.move_cursor(-PAGE, nodes.len()),
            Command::Top => state.jump_to_end(nodes.len(), false),
            Command::Bottom => state.jump_to_end(nodes.len(), true),
            Command::Expand => state.expand(&nodes, false),
            Command::Activate => state.expand(&nodes, true),
            Command::Collapse => state.collapse(&nodes),
            Command::CopyOutput => {
                if let Some(node) = nodes.get(state.cursor) {
                    let text = format!("{}: {}", node.path, inspector::value_text(node.value, node.open));
                    ctx.copy_text(text.clone());
                    drop(topic);
                    self.flash(format!("copied {text}"), false);
                }
            }
            Command::Search => {
                drop(topic);
                self.flash("press v for the stream to filter it", false);
            }
            Command::Plot => {
                let target =
                    nodes.get(state.cursor).map(|n| (n.path.clone(), n.open.is_none() && n.value.as_f64().is_some()));
                drop(topic);
                match target {
                    Some((path, true)) => self.plot_field(id, &path),
                    Some(_) => self.flash("only numbers can be plotted; pick a field with a number", false),
                    None => {}
                }
            }
            _ => {}
        }
    }

    fn table_command(&mut self, command: Command, ctx: &Context) {
        let rows = self.table.arrange(self.topic_rows());
        match command {
            Command::ScrollDown => self.table.move_selection(1, rows.len()),
            Command::ScrollUp => self.table.move_selection(-1, rows.len()),
            Command::PageDown => self.table.move_selection(PAGE, rows.len()),
            Command::PageUp => self.table.move_selection(-PAGE, rows.len()),
            Command::Top => self.table.jump_to_end(rows.len(), false),
            Command::Bottom => self.table.jump_to_end(rows.len(), true),
            Command::Activate | Command::ActivateBeside | Command::Expand => {
                if let Some(row) = rows.get(self.table.selected) {
                    self.open_topic(&row.name, Some(&row.ty), command == Command::ActivateBeside);
                }
            }
            Command::Search => self.searching = true,
            Command::ClearSearch => self.table.filter.clear(),
            Command::CopyOutput => {
                if let Some(row) = rows.get(self.table.selected) {
                    ctx.copy_text(row.name.clone());
                    self.flash(format!("copied {}", row.name), false);
                }
            }
            _ => {}
        }
    }

    /// Every topic on the graph with what archon knows of its rate
    pub(crate) fn topic_rows(&self) -> Vec<TopicRow> {
        let echoes: HashMap<&str, SourceId> = self
            .sup
            .entries()
            .iter()
            .enumerate()
            .filter(|(id, _)| self.topic_states.contains_key(id))
            .map(|(id, e)| (e.name.as_str(), id))
            .collect();
        self.graph
            .topics
            .iter()
            .map(|topic| {
                let echo = echoes.get(topic.name.as_str()).and_then(|id| self.topic_states.get(id));
                let watch = self.watches.iter().find(|w| w.watcher.topic() == topic.name).map(|w| &w.watcher);
                let watcher = watch.or_else(|| self.measured.get(&topic.name));
                let (hz, last) = match (echo, watcher) {
                    (Some(state), _) => {
                        let state = state.lock().unwrap();
                        (state.rate.hz(), state.rate.last())
                    }
                    (None, Some(w)) => (w.hz(), w.last_message()),
                    (None, None) => (None, None),
                };
                TopicRow {
                    name: topic.name.clone(),
                    ty: topic.types.join(", "),
                    hz,
                    publishers: topic.publishers,
                    subscribers: topic.subscribers,
                    last,
                    echoed: echo.is_some(),
                    watched: watch.is_some(),
                }
            })
            .collect()
    }

    /// Subscribes the watches once their topics appear, and keeps "measure every rate" in step with the graph
    fn poll_watchers(&mut self) {
        let Ok(link) = &self.ros else { return };
        for watch in &mut self.watches {
            watch.watcher.poll(&link.ros, &self.graph);
        }
        if self.table.measure_all {
            for topic in &self.graph.topics {
                let watcher = self.measured.entry(topic.name.clone()).or_insert_with(|| Watcher::new(&topic.name));
                watcher.poll(&link.ros, &self.graph);
            }
        } else {
            for (_, mut watcher) in self.measured.drain() {
                watcher.stop(&link.ros);
            }
        }
    }

    fn mark_seen(&mut self) {
        let visible: Vec<SourceId> = self
            .layout
            .views()
            .iter()
            .filter_map(|v| match v {
                View::Source(id) => Some(*id),
                _ => None,
            })
            .collect();
        let merged_shown = self.layout.views().contains(&View::AllLogs);
        for (id, entry) in self.sup.entries().iter().enumerate() {
            if visible.contains(&id) || (merged_shown && entry.in_merged_logs()) {
                let buffer = &entry.buffer;
                self.seen[id] = Seen { total: buffer.total(), errors: buffer.errors(), warnings: buffer.warnings() };
            }
        }
    }

    /// Draws one pane's view
    fn draw_view(&mut self, ui: &mut Ui, pane: usize, view: View) {
        match view {
            View::Source(id) if id >= self.sup.entries().len() => {
                ui.label(RichText::new("This source is gone").weak());
            }
            View::Source(id) if self.showing_bytes(id) => {
                let state = self.topic_states[&id].clone();
                let state = state.lock().unwrap();
                let registry = self.registry.lock().unwrap();
                bytes::show(ui, pane, &self.sup.entries()[id], &state, &mut self.bytes[id], &registry);
            }
            View::Source(id) if self.inspecting(id) => {
                let entry = &self.sup.entries()[id];
                // Before locking the topic, since working out the rate locks it too
                let detail = entry.detail();
                let state = self.topic_states[&id].clone();
                let state = state.lock().unwrap();
                if let Some(inspector::Action::Toggle) =
                    inspector::show(ui, pane, entry, detail, &state, &mut self.inspectors[id])
                {
                    self.deferred.push(Deferred::ToggleBranch(id));
                }
            }
            View::Source(id) => {
                let entry = &self.sup.entries()[id];
                logview::source_header(ui, entry, &self.views[id]);
                ui.separator();
                logview::lines(ui, pane, &entry.buffer, &mut self.views[id], self.timestamps);
            }
            View::AllLogs => {
                let rows = Merged { timeline: self.sup.timeline(), entries: self.sup.entries() };
                let total = (rows.end() - rows.first()) as usize;
                logview::merged_header(ui, &self.merged, total);
                ui.separator();
                logview::lines(ui, pane, &rows, &mut self.merged, self.timestamps);
            }
            View::Publish(i) if i < self.publishers.len() => {
                let send = self.key_of(Command::Send).unwrap_or_default();
                let repeat = self.key_of(Command::ToggleRepeat).unwrap_or_default();
                let registry = self.registry.lock().unwrap();
                match publish::show(ui, pane, &mut self.publishers[i], &registry, (&send, &repeat)) {
                    Some(publish::Action::Send) => self.deferred.push(Deferred::Send(i)),
                    Some(publish::Action::ToggleRepeat) => self.deferred.push(Deferred::ToggleRepeat(i)),
                    Some(publish::Action::SaveToProfile) => self.deferred.push(Deferred::SaveToProfile(i)),
                    None => {}
                }
            }
            View::Publish(_) => {
                ui.label(RichText::new("This publisher is gone").weak());
            }
            View::Plot(i) if i < self.plots.len() => {
                self.current_plot = Some(i);
                let entries = self.sup.entries();
                let names = |id: SourceId| entries.get(id).map_or_else(String::new, |e| e.name.clone());
                if let Some(plot::Action::Remove(series)) =
                    plot::show(ui, pane, &mut self.plots[i], &self.topic_states, &names)
                {
                    self.deferred.push(Deferred::Unplot(i, series));
                }
            }
            View::Plot(_) => {
                ui.label(RichText::new("This plot is gone").weak());
            }
            View::RobotBuilder => {
                let keys = [
                    self.key_of(Command::Send).unwrap_or_default(),
                    self.key_of(Command::ToggleRepeat).unwrap_or_default(),
                    "a".to_owned(),
                ];
                let home = dirs::home_dir().unwrap_or_default();
                match &mut self.builder {
                    Some(builder) => {
                        let keys = [keys[0].as_str(), keys[1].as_str(), keys[2].as_str()];
                        if let Some(action) = crate::builder::show(ui, builder, &home, keys) {
                            self.deferred.push(Deferred::Builder(action));
                        }
                    }
                    None => _ = ui.label(RichText::new("No robot is being built (Space n r starts one)").weak()),
                }
            }
            View::Welcome => {
                let agent = self.find_agent();
                let (serial, can) = (bench::serial_ports(), bench::can_ports());
                let profile = self.profile_path.as_ref().map(|p| p.display().to_string());
                let facts = crate::welcome::Facts {
                    agent: &agent,
                    serial: &serial,
                    can: &can,
                    simulator: self.find_simulator().map(|s| s.checkout.display().to_string()),
                    ros: self.ros.as_ref().map(|_| ()).map_err(String::as_str),
                    profile: profile.as_deref(),
                    unsaved: self.added.len(),
                };
                let key = |c| self.key_of(c).unwrap_or_default();
                if let Some(command) = crate::welcome::show(ui, &facts, &key) {
                    self.deferred.push(Deferred::Run(command));
                }
            }
            View::Topics => {
                let rows = self.table.arrange(self.topic_rows());
                let note = self.ros.as_ref().err().map(|why| format!("ROS isn't running: {why}"));
                if let Some(topics::Action::Open { name, ty, beside }) =
                    topics::show(ui, pane, &rows, &mut self.table, note.as_deref())
                {
                    self.deferred.push(Deferred::OpenTopic { name, ty: Some(ty), beside });
                }
            }
        }
    }

    fn run_deferred(&mut self, ctx: &Context) {
        for deferred in std::mem::take(&mut self.deferred) {
            match deferred {
                Deferred::OpenTopic { name, ty, beside } => self.open_topic(&name, ty.as_deref(), beside),
                Deferred::Send(i) => self.send(i),
                Deferred::ToggleRepeat(i) => self.toggle_repeat(i),
                Deferred::SaveToProfile(i) => self.save_to_profile(i),
                Deferred::Unplot(plot, series) => self.unplot(plot, series),
                Deferred::Run(command) => self.run(command, ctx),
                Deferred::Builder(crate::builder::Action::AddBoard) => self.pick_robot_board(),
                Deferred::Builder(crate::builder::Action::Save) => self.save_robot(false),
                Deferred::Builder(crate::builder::Action::SaveAndRun) => self.save_robot(true),
                Deferred::ToggleBranch(id) => {
                    let _ = ctx;
                    let topic = self.topic_states[&id].clone();
                    let topic = topic.lock().unwrap();
                    if let Some(latest) = &topic.latest {
                        let state = &mut self.inspectors[id];
                        let nodes = state.nodes(latest);
                        state.expand(&nodes, true);
                    }
                }
            }
        }
    }

    /// The keys that matter right now, so nobody has to remember them
    pub(crate) fn hints(&self) -> Vec<(String, &'static str)> {
        let fixed = |pairs: &[(&str, &'static str)]| pairs.iter().map(|(k, l)| ((*k).to_owned(), *l)).collect();
        if let Some(picker) = &self.picker {
            if picker.is_publishing() {
                return fixed(&[
                    ("Up/Down", "select"),
                    ("Enter", "open the form"),
                    ("Ctrl+P", "sources"),
                    ("Esc", "close"),
                ]);
            }
            return if picker.is_commands() {
                fixed(&[("Up/Down", "select"), ("Enter", "run"), ("Ctrl+P", "sources"), ("Esc", "close")])
            } else {
                fixed(&[
                    ("Up/Down", "select"),
                    ("Enter", "open"),
                    ("Ctrl+Enter", "open beside"),
                    ("Shift+Enter", "publish to it"),
                    ("Ctrl+O", "publish"),
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
        if self.naming_layout.is_some() {
            return fixed(&[("Enter", "save"), ("Esc", "cancel")]);
        }
        let mut wanted = vec![(Command::QuickOpen, "sources"), (Command::OpenPalette, "commands")];
        let mut escape = None;
        match self.layout.active_view() {
            View::Source(id) if id < self.sup.entries().len() => {
                let entry = &self.sup.entries()[id];
                let running = entry.status.is_active() || entry.restart_at.is_some();
                if running {
                    wanted.extend([(Command::Stop, "stop"), (Command::Restart, "restart")]);
                    if entry.kind != SourceKind::Topic {
                        wanted.push((Command::FocusInput, "send"));
                    }
                } else {
                    wanted.push((Command::Start, "start"));
                }
                if self.inspecting(id) {
                    wanted.extend([
                        (Command::ScrollDown, "move"),
                        (Command::Activate, "expand"),
                        (Command::CopyOutput, "copy field"),
                        (Command::ToggleStream, "stream"),
                        (Command::ToggleWatch, "watch"),
                    ]);
                } else {
                    let view = &self.views[id];
                    wanted.push((Command::Search, "filter"));
                    wanted.push((Command::ToggleFollow, if view.follow { "pause" } else { "follow" }));
                    if entry.buffer.errors() + entry.buffer.warnings() > 0 {
                        wanted.push((Command::NextError, "next error"));
                    }
                    if self.topic_states.contains_key(&id) {
                        wanted.push((Command::ToggleStream, "inspector"));
                    }
                    escape = log_escape(view);
                }
            }
            View::AllLogs => {
                wanted.push((Command::Search, "filter"));
                wanted.push((Command::ToggleFollow, if self.merged.follow { "pause" } else { "follow" }));
                wanted.push((Command::NextError, "next error"));
                escape = log_escape(&self.merged);
            }
            View::Topics => {
                wanted.extend([
                    (Command::ScrollDown, "move"),
                    (Command::Activate, "echo"),
                    (Command::ActivateBeside, "beside"),
                    (Command::Search, "filter"),
                    (Command::ToggleWatch, "watch"),
                    (Command::MeasureAll, "measure all"),
                ]);
            }
            View::RobotBuilder => {
                wanted.extend([
                    (Command::Send, "save"),
                    (Command::ToggleRepeat, "save and run"),
                    (Command::ScrollDown, "move"),
                    (Command::Remove, "remove board"),
                ]);
            }
            View::Welcome => {
                wanted.extend([
                    (Command::StartAgent, "start the agent"),
                    (Command::OpenSerial, "serial console"),
                    (Command::OpenTopics, "topics"),
                    (Command::SaveBench, "save the bench"),
                ]);
            }
            View::Plot(_) => {
                wanted.extend([
                    (Command::PlotWider, "longer"),
                    (Command::PlotNarrower, "shorter"),
                    (Command::ToggleFollow, "pause"),
                    (Command::ClearView, "clear"),
                ]);
            }
            View::Publish(_) => {
                wanted.extend([
                    (Command::ScrollDown, "move"),
                    (Command::Activate, "edit"),
                    (Command::Send, "send"),
                    (Command::ToggleRepeat, "repeat"),
                ]);
            }
            View::Source(_) => {}
        }
        if self.layout.len() > 1 {
            wanted.push((Command::FocusRight, "move between panes"));
        }
        wanted.push((Command::ShowKeys, "all keys"));
        let mut hints: Vec<_> = wanted.into_iter().filter_map(|(c, l)| Some((self.key_of(c)?, l))).collect();
        if let Some(label) = escape.or_else(|| self.layout.can_go_back().then_some("back")) {
            hints.insert(2, ("Esc".to_owned(), label));
        }
        hints
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        // The right end first (the profile, then any message), so the hints take only the room that's left and
        // never run under a message
        let hints = self.hints();
        let pending = self.matcher.pending().to_vec();
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let profile = self.profile_path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy());
            let profile = profile.unwrap_or("no profile".into());
            ui.label(RichText::new(profile).small().weak())
                .on_hover_text(self.profile_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
            if self.timestamps {
                ui.label(RichText::new("TIME").small().weak());
            }
            if let Some(flash) = &self.flash
                && flash.at.elapsed() < FLASH_TIME
            {
                let color = if flash.bad { ui.visuals().error_fg_color } else { ui.visuals().strong_text_color() };
                let width = ui.available_width() * 0.6;
                ui.scope(|ui| {
                    ui.set_max_width(width);
                    ui.add(egui::Label::new(RichText::new(&flash.text).small().color(color)).truncate())
                        .on_hover_text(&flash.text);
                });
                ui.ctx().request_repaint_after(FLASH_TIME);
            }
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if !pending.is_empty() {
                    ui.label(RichText::new(format!("{} …", format_sequence(&pending))).monospace().strong());
                    ui.label(RichText::new("Esc cancels").small().weak());
                    return;
                }
                let chip = ui.visuals().widgets.inactive.bg_fill;
                let font = egui::TextStyle::Small.resolve(ui.style());
                let mono = egui::FontId::monospace(font.size);
                for (keys, label) in hints {
                    let wide = |text: String, font: egui::FontId| {
                        ui.fonts_mut(|f| f.layout_no_wrap(text, font, Color32::WHITE).size().x)
                    };
                    let needed = wide(format!(" {keys} "), mono.clone()) + wide(label.to_owned(), font.clone()) + 16.0;
                    if needed > ui.available_width() {
                        break;
                    }
                    ui.label(RichText::new(format!(" {keys} ")).monospace().small().background_color(chip));
                    ui.label(RichText::new(label).small().weak());
                    ui.add_space(6.0);
                }
            });
        });
    }

    fn input_bar(&mut self, ui: &mut Ui) {
        let Some(id) = self.current_source() else {
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
            let hint = if self.sup.entries()[id].kind == SourceKind::Can {
                "a frame as id#data, like 123#DEADBEEF or 12345678#00"
            } else {
                "type a line to send"
            };
            let edit = TextEdit::singleline(text).hint_text(hint).code_editor().desired_width(f32::INFINITY);
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
        let (enter, escape) =
            ui.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape)));
        let Some(filter) = self.active_filter() else {
            self.searching = false;
            return;
        };
        if escape {
            filter.clear();
        }
        if enter || escape {
            self.searching = false;
            return;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("/").monospace().strong());
            let edit = TextEdit::singleline(filter)
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

    /// The ROS link, for the right end of the health strip
    fn graph_label(&self) -> String {
        match &self.ros {
            Ok(_) if let Ok(server) = std::env::var("ROS_DISCOVERY_SERVER") => {
                let own = if self.discovery.is_some() { "isolated" } else { "discovery server" };
                format!("{own} {server}")
            }
            Ok(_) => {
                let domain = std::env::var("ROS_DOMAIN_ID").unwrap_or_else(|_| "0".to_owned());
                let rmw = std::env::var("RMW_IMPLEMENTATION").unwrap_or_else(|_| "default RMW".to_owned());
                format!("domain {domain} · {rmw}")
            }
            Err(_) => "ROS off".to_owned(),
        }
    }

    fn health_strip(&mut self, ui: &mut Ui) {
        let label = self.graph_label();
        let sending: Vec<(usize, String)> = self
            .publishers
            .iter()
            .enumerate()
            .filter(|(_, p)| p.repeating)
            .map(|(i, p)| (i, format!("sending {} {} Hz", p.name, p.rate_text.trim())))
            .collect();
        let click = health::show(ui, self.sup.entries(), &self.watches, &sending, self.unseen_errors(), &label);
        let ctx = ui.ctx().clone();
        match click {
            Some(health::Click::Source(id)) => self.show_view(View::Source(id), false),
            Some(health::Click::Topic(topic)) => {
                let ty = self.graph.topic(&topic).and_then(|t| t.types.first().cloned());
                self.open_topic(&topic, ty.as_deref(), false);
            }
            Some(health::Click::Publisher(i)) => self.show_view(View::Publish(i), false),
            Some(health::Click::Errors) => {
                self.show_view(View::AllLogs, false);
                self.run(Command::NextError, &ctx);
            }
            None => {}
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
            ("Views", &["open.", "view.", "topics."]),
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
                    ui.label(
                        RichText::new("Esc closes · Ctrl+= and Ctrl+- zoom · rebind in ~/.config/archon/keymap.toml")
                            .small()
                            .weak(),
                    );
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
        let actions = self.action_items();
        let Some(picker) = &mut self.picker else { return };
        match picker.show(ctx, self.sup.entries(), &self.graph.topics, &actions, &self.keymap, &self.recent) {
            picker::Outcome::Open => {}
            picker::Outcome::Closed => {
                self.picker = None;
                // Not answering upload_tool's question calls the flash off; nothing has been written yet
                if let Some(Pick::FlashAnswer(id, _)) = self.picking.take() {
                    self.sup.stop(id);
                    self.flash("flash called off before anything was written", false);
                }
            }
            picker::Outcome::Chose(choice) => {
                self.picker = None;
                match choice {
                    Choice::Command(command) => {
                        self.recent.retain(|c| *c != command);
                        self.recent.insert(0, command);
                        self.recent.truncate(RECENT_COMMANDS);
                        self.run(command, ctx);
                    }
                    Choice::View { view, beside } => self.show_view(view, beside),
                    Choice::Source { id, beside } => self.show_view(View::Source(id), beside),
                    Choice::Topic { name, ty, beside } => self.open_topic(&name, ty.as_deref(), beside),
                    Choice::Publish { topic, ty } => self.open_publish(&topic, &ty),
                    Choice::Action { index, run: true } => self.run_action(index),
                    Choice::Action { index, run: false } => self.show_view(View::Publish(index), false),
                    Choice::Layout(name) => self.load_layout(&name),
                    Choice::Option(index) => self.picked(index),
                }
            }
        }
    }

    fn save_layout(&mut self, name: &str) {
        let Some(state) = &self.state_file else { return };
        let path = persist::layout_file(&persist::layouts_dir(state), name);
        match persist::save(&path, &self.saved()) {
            Ok(()) => self.flash(format!("saved layout {}", name.trim()), false),
            Err(e) => self.flash(format!("couldn't save the layout: {e:#}"), true),
        }
    }

    fn load_layout(&mut self, name: &str) {
        let Some(state) = &self.state_file else { return };
        match persist::load(&persist::layout_file(&persist::layouts_dir(state), name)) {
            Some(saved) => {
                self.restore(saved);
                self.flash(format!("loaded layout {name}"), false);
            }
            None => self.flash(format!("couldn't read layout {name}"), true),
        }
    }

    /// The bar for naming a layout to save
    fn layout_name_bar(&mut self, ui: &mut Ui) {
        let (enter, escape) =
            ui.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape)));
        if escape {
            self.naming_layout = None;
            return;
        }
        if enter {
            let name = self.naming_layout.take().unwrap_or_default();
            if name.trim().is_empty() {
                self.flash("a layout needs a name", true);
            } else {
                self.save_layout(&name);
            }
            return;
        }
        let Some(name) = &mut self.naming_layout else { return };
        ui.horizontal(|ui| {
            ui.label(RichText::new("Save layout as").strong());
            let edit = TextEdit::singleline(name).hint_text("flash test, bench, …").desired_width(f32::INFINITY);
            ui.add(edit).request_focus();
        });
    }

    /// The layout as it's saved, by names
    fn saved(&self) -> Saved {
        let panes = self
            .layout
            .views()
            .iter()
            .map(|view| match *view {
                View::Source(id) => {
                    let entry = &self.sup.entries()[id];
                    let name = entry.name.clone();
                    if entry.kind == SourceKind::Topic { PaneSpec::Topic { name } } else { PaneSpec::Source { name } }
                }
                View::AllLogs => PaneSpec::AllLogs,
                View::Topics => PaneSpec::Topics,
                View::Welcome | View::RobotBuilder => PaneSpec::Welcome,
                View::Publish(i) => {
                    let p = &self.publishers[i];
                    PaneSpec::Publish { name: p.name.clone(), topic: p.topic.clone(), ty: p.ty.clone() }
                }
                View::Plot(i) => {
                    let plot = &self.plots[i];
                    let series = plot
                        .series
                        .iter()
                        .map(|(id, path)| (self.sup.entries()[*id].name.clone(), path.clone()))
                        .collect();
                    PaneSpec::Plot { series, seconds: plot.seconds }
                }
            })
            .collect();
        Saved {
            zoom: Some(self.zoom),
            sidebar: self.sidebar,
            timestamps: self.timestamps,
            active: self.layout.active(),
            root: self.layout.root().clone(),
            panes,
        }
    }

    /// Puts back a saved layout. A source no longer in the profile leaves its pane on the merged log; a topic echo
    /// opens again.
    fn restore(&mut self, saved: Saved) {
        let mut views = Vec::new();
        for spec in &saved.panes {
            let by_name = |name: &str| self.sup.entries().iter().position(|e| !e.removed && e.name == name);
            let view = match spec {
                PaneSpec::Source { name } => by_name(name).map_or(View::AllLogs, View::Source),
                PaneSpec::Topic { name } => match by_name(name) {
                    Some(id) => View::Source(id),
                    None => match self.add_echo(name, name, None, true) {
                        Ok(id) => View::Source(id),
                        Err(_) => View::Topics,
                    },
                },
                PaneSpec::AllLogs => View::AllLogs,
                PaneSpec::Topics => View::Topics,
                PaneSpec::Welcome => View::Welcome,
                PaneSpec::Plot { series, seconds } => {
                    let mut plot = Plot::new();
                    plot.seconds = *seconds;
                    for (name, path) in series {
                        let id = match self.sup.entries().iter().position(|e| !e.removed && e.name == *name) {
                            Some(id) => Some(id),
                            None => self.add_echo(name, name, None, true).ok(),
                        };
                        if let Some(id) = id.filter(|id| self.topic_states.contains_key(id)) {
                            self.topic_states[&id].lock().unwrap().trace(path);
                            plot.series.push((id, path.clone()));
                        }
                    }
                    self.plots.push(plot);
                    View::Plot(self.plots.len() - 1)
                }
                PaneSpec::Publish { name, topic, ty } => {
                    match self.publishers.iter().position(|p| !p.removed && p.name == *name && p.topic == *topic) {
                        Some(i) => View::Publish(i),
                        None => {
                            let publisher = Publisher::new(&mut self.registry.lock().unwrap(), name, topic, ty);
                            self.publishers.push(publisher);
                            self.advertise(self.publishers.len() - 1);
                            View::Publish(self.publishers.len() - 1)
                        }
                    }
                }
            };
            views.push(view);
        }
        if let Some(layout) = Layout::from_parts(saved.root, views, saved.active) {
            self.layout = layout;
        }
        self.sidebar = saved.sidebar;
        self.timestamps = saved.timestamps;
        if let Some(zoom) = saved.zoom {
            self.zoom = zoom;
            self.zoom_pending = true;
        }
    }
}

/// egui's defaults are small for a window read at arm's length all day; Ctrl+= and Ctrl+- zoom from here
/// A port as the picker lists it
fn option(port: &Port, hint: &str) -> OptionItem {
    let detail = match port {
        Port::Serial(path) => {
            let tty =
                std::fs::canonicalize(path).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            format!("{} · {}", port.kind(), tty.unwrap_or_else(|| path.display().to_string()))
        }
        _ => port.kind().to_owned(),
    };
    OptionItem { label: port.label(), detail, hint: hint.to_owned() }
}

fn set_text_sizes(ctx: &Context) {
    use egui::{FontFamily, FontId, TextStyle};
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace)),
            (TextStyle::Heading, FontId::new(21.0, FontFamily::Proportional)),
        ]
        .into();
        style.spacing.item_spacing.y = 5.0;
    });
}

/// What Escape does in a log, for the hints
fn log_escape(view: &ViewState) -> Option<&'static str> {
    match (view.filter.is_empty(), view.follow) {
        (false, _) => Some("clear filter"),
        (true, false) => Some("back to end"),
        (true, true) => None,
    }
}

/// Runs a command on a log view. Returns a message to flash.
fn log_command(command: Command, view: &mut ViewState, rows: &dyn Rows, ctx: &Context) -> Option<String> {
    match command {
        Command::ScrollDown => view.scroll(Scroll::Lines(3.0)),
        Command::ScrollUp => view.scroll(Scroll::Lines(-3.0)),
        Command::PageDown => view.scroll(Scroll::Pages(1.0)),
        Command::PageUp => view.scroll(Scroll::Pages(-1.0)),
        Command::Top => view.scroll(Scroll::Top),
        Command::Bottom => view.scroll(Scroll::Bottom),
        Command::ToggleFollow => view.toggle_follow(),
        Command::ClearSearch => view.filter.clear(),
        Command::CopyOutput => {
            let (text, lines) = view.shown_text(rows);
            ctx.copy_text(text);
            let what = if view.filter.is_empty() { "" } else { " matching the filter" };
            return Some(format!("copied {lines} lines{what}"));
        }
        Command::NextError | Command::PrevError => {
            let forward = command == Command::NextError;
            if !view.jump_to_problem(rows, forward) {
                let way = if forward { "below" } else { "above" };
                return Some(format!("no errors or warnings {way}"));
            }
        }
        _ => {}
    }
    None
}

impl eframe::App for ArchonApp {
    fn raw_input_hook(&mut self, ctx: &Context, raw_input: &mut egui::RawInput) {
        self.handle_keys(ctx, &mut raw_input.events);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.zoom_pending) {
            ctx.set_zoom_factor(self.zoom);
        }
        self.zoom = ctx.zoom_factor();
        self.sup.poll();
        if let Ok(link) = &self.ros {
            self.graph = link.ros.graph();
        }
        self.poll_watchers();
        self.send_repeating(&ctx);
        self.answer_flash_questions();
        if !self.watches.is_empty() || self.table.measure_all {
            ctx.request_repaint_after(Duration::from_millis(500));
        }

        Panel::top("health").show(ui, |ui| self.health_strip(ui));
        Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.input.is_some() {
            Panel::bottom("input").show(ui, |ui| self.input_bar(ui));
        }
        if self.searching {
            Panel::bottom("search").show(ui, |ui| self.search_bar(ui));
        }
        if self.naming_layout.is_some() {
            Panel::bottom("layout-name").show(ui, |ui| self.layout_name_bar(ui));
        }
        if self.removing.is_some() {
            Panel::bottom("removing").show(ui, |ui| self.removal_bar(ui));
        }
        if !self.problems.is_empty() {
            Panel::top("problems").show(ui, |ui| self.problems_bar(ui));
        }
        if self.sidebar {
            Panel::left("sources").resizable(true).default_size(260.0).show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.sidebar(ui));
            });
        }
        CentralPanel::default_margins().show(ui, |ui| {
            // The layout draws through the app, so it's taken out for the length of the draw
            let mut layout = std::mem::replace(&mut self.layout, Layout::new(View::AllLogs));
            let dropped = layout.show(ui, |ui, pane, view| self.draw_view(ui, pane, view));
            self.layout = layout;
            if let Some(drop) = dropped {
                self.dock(drop);
            }
        });
        self.run_deferred(&ctx);
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
        if let Some(path) = &self.state_file
            && let Err(e) = persist::save(path, &self.saved())
        {
            eprintln!("archon: couldn't save the layout to {}: {e:#}", path.display());
        }
        self.sup.shutdown(Duration::from_secs(3));
    }
}

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

    fn harness_with(profile: &str) -> Harness<'static, ArchonApp> {
        let profile = Profile::parse(profile).unwrap();
        let mut harness = Harness::builder().with_size(vec2(1200.0, 700.0)).build_eframe(move |cc| {
            let startup = Startup {
                profile,
                keymap: Keymap::defaults(),
                problems: Vec::new(),
                with_ros: false,
                state_file: None,
                config: Default::default(),
                discovery: None,
                simulate: None,
                attach: Vec::new(),
            };
            ArchonApp::new(cc, startup)
        });
        harness.run_steps(2);
        harness
    }

    fn harness() -> Harness<'static, ArchonApp> {
        harness_with(PROFILE)
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

    fn keys(harness: &mut Harness<'static, ArchonApp>, sequence: &str) {
        for chord in crate::keymap::parse_sequence(sequence).unwrap() {
            let mut modifiers = Modifiers::NONE;
            modifiers.ctrl = chord.ctrl;
            modifiers.alt = chord.alt;
            modifiers.shift = chord.shift;
            press(harness, modifiers, chord.key);
        }
    }

    fn type_text(harness: &mut Harness<'static, ArchonApp>, text: &str) {
        harness.event(Event::Text(text.to_owned()));
        harness.run_steps(2);
    }

    fn fake_graph() -> Arc<Graph> {
        let topic = |name: &str, ty: &str, publishers| archon_ros::Topic {
            name: name.to_owned(),
            types: vec![ty.to_owned()],
            publishers,
            subscribers: 1,
        };
        Arc::new(Graph {
            topics: vec![
                topic("/rosout", "rcl_interfaces/msg/Log", 3),
                topic("/imu/data", "sensor_msgs/msg/Imu", 1),
                topic("/thrusters/forces", "riptide_msgs2/msg/ThrusterForces", 0),
            ],
        })
    }

    /// Makes source `id` act as a topic with a message, so the inspector shows without ROS
    fn fake_topic(app: &mut ArchonApp, id: SourceId) {
        use archon_msg::Value;
        let field = |n: &str, v| (n.to_owned(), v);
        let mut state = TopicState::default();
        for z in [9.79, 9.81, 9.80] {
            state.update(Value::Struct(vec![
                field("header", Value::Struct(vec![field("frame_id", Value::String("imu_link".into()))])),
                field(
                    "linear_acceleration",
                    Value::Struct(vec![
                        field("x", Value::Float64(0.01)),
                        field("y", Value::Float64(-0.02)),
                        field("z", Value::Float64(z)),
                    ]),
                ),
                field("force", Value::Array(vec![Value::Float32(1.0), Value::Float32(2.5)])),
            ]));
            std::thread::sleep(Duration::from_millis(11));
        }
        app.topic_states.insert(id, Arc::new(Mutex::new(state)));
    }

    #[test]
    fn ctrl_p_picks_a_source() {
        let mut h = harness();
        press(&mut h, Modifiers::CTRL, Key::P);
        assert!(h.state().picker.as_ref().is_some_and(|p| !p.is_commands()));
        type_text(&mut h, "thr");
        press(&mut h, Modifiers::NONE, Key::Enter);
        assert!(h.state().picker.is_none());
        assert_eq!(h.state().current_source(), Some(2));
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
        assert_eq!(app.layout.views(), [View::Source(0), View::Source(1)]);
        assert_eq!(app.layout.active(), 1);
    }

    #[test]
    fn picker_opens_views() {
        let mut h = harness();
        press(&mut h, Modifiers::CTRL, Key::P);
        type_text(&mut h, "all logs");
        press(&mut h, Modifiers::NONE, Key::Enter);
        assert_eq!(h.state().layout.active_view(), View::AllLogs);
    }

    #[test]
    fn tab_cycles_sources_instead_of_widget_focus() {
        let mut h = harness();
        press(&mut h, Modifiers::NONE, Key::Tab);
        press(&mut h, Modifiers::NONE, Key::Tab);
        assert_eq!(h.state().current_source(), Some(2));
        press(&mut h, Modifiers::SHIFT, Key::Tab);
        assert_eq!(h.state().current_source(), Some(1));
        assert!(h.ctx.memory(|m| m.focused().is_none()), "no widget took focus");
    }

    #[test]
    fn panes_split_move_and_close_like_vim() {
        let mut h = harness();
        keys(&mut h, "ctrl+w v");
        assert_eq!(
            h.state().layout.views(),
            [View::Source(0), View::Source(1)],
            "a new pane shows a source not on screen"
        );
        keys(&mut h, "ctrl+w s");
        assert_eq!(h.state().layout.len(), 3);
        keys(&mut h, "ctrl+w h");
        assert_eq!(h.state().layout.active(), 0);
        keys(&mut h, "alt+l");
        assert_ne!(h.state().layout.active(), 0);
        keys(&mut h, "ctrl+w q");
        keys(&mut h, "ctrl+w q");
        assert_eq!(h.state().layout.len(), 1);
        keys(&mut h, "ctrl+w q");
        assert_eq!(h.state().flash.as_ref().map(|f| f.text.as_str()), Some("that's the last pane"));
    }

    #[test]
    fn number_keys_and_leader_sequences() {
        let mut h = harness();
        press(&mut h, Modifiers::NONE, Key::Num3);
        assert_eq!(h.state().current_source(), Some(2));
        press(&mut h, Modifiers::NONE, Key::Space);
        press(&mut h, Modifiers::NONE, Key::W);
        assert_eq!(h.state().matcher.pending().len(), 2, "waiting for the rest of Space w …");
        press(&mut h, Modifiers::NONE, Key::V);
        assert_eq!(h.state().layout.len(), 2);
    }

    #[test]
    fn views_open_by_key() {
        let mut h = harness();
        keys(&mut h, "L");
        assert_eq!(h.state().layout.active_view(), View::AllLogs);
        keys(&mut h, "T");
        assert_eq!(h.state().layout.active_view(), View::Topics);
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
    fn hints_follow_the_view() {
        let mut h = harness();
        let labels =
            |h: &Harness<'static, ArchonApp>| h.state().hints().into_iter().map(|(_, l)| l).collect::<Vec<_>>();
        assert!(labels(&h).contains(&"start"));
        press(&mut h, Modifiers::NONE, Key::S);
        assert!(labels(&h).contains(&"stop"));
        assert_eq!(h.state().hints()[0], ("Ctrl+P".to_owned(), "sources"));
        keys(&mut h, "T");
        assert!(labels(&h).contains(&"measure all"));
    }

    #[test]
    fn inspector_moves_expands_and_copies() {
        let mut h = harness();
        fake_topic(h.state_mut(), 1);
        keys(&mut h, "2");
        assert!(h.state().inspecting(1));
        keys(&mut h, "j j j");
        assert_eq!(h.state().inspectors[1].cursor, 3);
        keys(&mut h, "y");
        let copied = h.state().flash.as_ref().map(|f| f.text.clone()).unwrap_or_default();
        assert_eq!(copied, "copied linear_acceleration.x: 0.01");
        keys(&mut h, "h");
        assert_eq!(h.state().inspectors[1].cursor, 2, "back to the parent");
        keys(&mut h, "h");
        keys(&mut h, "j");
        assert_eq!(h.state().inspectors[1].cursor, 3, "the closed branch's children are gone");
        keys(&mut h, "v");
        assert!(!h.state().inspecting(1), "v shows the stream");
        keys(&mut h, "v");
        assert!(h.state().showing_bytes(1), "then the bytes");
        keys(&mut h, "v");
        assert!(h.state().inspecting(1), "then the inspector again");
    }

    #[test]
    fn topics_table_lists_sorts_and_opens() {
        let mut h = harness();
        h.state_mut().graph = fake_graph();
        keys(&mut h, "T");
        let rows = h.state().table.arrange(h.state().topic_rows());
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["/imu/data", "/rosout", "/thrusters/forces"]
        );
        keys(&mut h, "j");
        keys(&mut h, "enter");
        // Without ROS the echo can't open, and says why instead of doing nothing
        let flash = h.state().flash.as_ref().map(|f| f.text.clone()).unwrap_or_default();
        assert!(flash.starts_with("ROS isn't running"), "{flash}");
        keys(&mut h, "w");
        assert_eq!(h.state().watches.len(), 1);
        assert_eq!(h.state().watches[0].watcher.topic(), "/rosout");
    }

    #[test]
    fn merged_log_interleaves_sources() {
        let mut h = harness_with(
            r#"
            [[process]]
            name = "agent"
            cmd = "sleep 0.3; echo agent up; sleep 0.6; echo '[ERROR] agent lost the board'"
            autostart = true
            [[process]]
            name = "board"
            cmd = "sleep 0.6; echo board ready"
            autostart = true
            "#,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while h.state().sup.entries().iter().any(|e| e.status.is_active()) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
            h.run_steps(1);
        }
        keys(&mut h, "L");
        let app = h.state_mut();
        let rows = Merged { timeline: app.sup.timeline(), entries: app.sup.entries() };
        let (text, _) = app.merged.shown_text(&rows);
        // Something on the development machine writes "DISCOVER" into every new terminal, which isn't archon's
        let text = text.replace("DISCOVER", "");
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| !l.contains("» ") && !l.contains("started") && !l.contains("exited") && !l.ends_with(": "))
            .collect();
        assert_eq!(lines, ["agent: agent up", "board: board ready", "agent: [ERROR] agent lost the board"]);
        assert!(app.merged.jump_to_problem(&rows, true));
    }

    #[test]
    fn layout_round_trips_by_name() {
        let mut h = harness();
        keys(&mut h, "ctrl+w v");
        keys(&mut h, "T");
        keys(&mut h, "t");
        let saved = h.state().saved();
        assert_eq!(saved.panes, [PaneSpec::Source { name: "agent".into() }, PaneSpec::Topics]);

        let mut fresh = harness();
        fresh.state_mut().restore(saved);
        let app = fresh.state();
        assert_eq!(app.layout.views(), [View::Source(0), View::Topics]);
        assert_eq!(app.layout.active(), 1);
        assert!(app.timestamps);
    }

    #[test]
    fn escape_goes_back_through_a_pane() {
        let mut h = harness();
        keys(&mut h, "T");
        keys(&mut h, "L");
        keys(&mut h, "escape");
        assert_eq!(h.state().layout.active_view(), View::Topics);
        keys(&mut h, "escape");
        assert_eq!(h.state().layout.active_view(), View::Source(0));
    }

    const PUBLISHING: &str = r#"
        [[process]]
        name = "agent"
        cmd = "sleep 30"
        [[publish]]
        name = "kill"
        topic = "/talos/state/kill"
        type = "std_msgs/msg/Bool"
        value = { data = true }
        key = "F2"
        [[publish]]
        name = "spin"
        topic = "/cmd_vel"
        type = "geometry_msgs/msg/Twist"
        value = { angular = { z = 0.5 } }
        rate = 20
    "#;

    #[test]
    fn profile_actions_fill_bind_and_run() {
        if std::env::var_os("AMENT_PREFIX_PATH").is_none() {
            return;
        }
        let mut h = harness_with(PUBLISHING);
        let app = h.state();
        assert_eq!(app.publishers[0].value.as_ref().unwrap().get("data"), Some(&archon_msg::Value::Bool(true)));
        assert_eq!(
            app.publishers[1].value.as_ref().unwrap().get("angular.z").and_then(archon_msg::Value::as_f64),
            Some(0.5)
        );
        assert!(app.publishers[1].repeats);
        // F2 runs the first action; without ROS it says so rather than doing nothing
        keys(&mut h, "F2");
        let flash = h.state().flash.as_ref().map(|f| f.text.clone()).unwrap_or_default();
        assert!(flash.starts_with("ROS isn't running"), "{flash}");
        // The palette lists actions to send
        press(&mut h, Modifiers::CTRL | Modifiers::SHIFT, Key::P);
        type_text(&mut h, "send kill");
        press(&mut h, Modifiers::NONE, Key::Enter);
        assert!(h.state().picker.is_none());
    }

    #[test]
    fn publish_form_opens_from_the_topics_table() {
        if std::env::var_os("AMENT_PREFIX_PATH").is_none() {
            return;
        }
        let mut h = harness();
        h.state_mut().graph = fake_graph();
        keys(&mut h, "T");
        keys(&mut h, "j");
        keys(&mut h, "j");
        keys(&mut h, "P");
        // /thrusters/forces is riptide_msgs2, which isn't in every ROS environment; whichever, the form opens
        let View::Publish(i) = h.state().layout.active_view() else { panic!("{:?}", h.state().layout.active_view()) };
        assert_eq!(h.state().publishers[i].topic, "/thrusters/forces");
        keys(&mut h, "ctrl+s");
        assert!(h.state().flash.as_ref().is_some_and(|f| f.bad));
        keys(&mut h, "escape");
        assert_eq!(h.state().layout.active_view(), View::Topics, "Esc goes back to the table");
    }

    #[test]
    fn ctrl_o_publishes_to_any_topic_without_echoing_it() {
        if std::env::var_os("AMENT_PREFIX_PATH").is_none() {
            return;
        }
        let mut h = harness();
        h.state_mut().graph = fake_graph();
        let sources = h.state().sup.entries().len();
        keys(&mut h, "ctrl+o");
        assert!(h.state().picker.as_ref().is_some_and(|p| p.is_publishing()));
        type_text(&mut h, "imu/data");
        press(&mut h, Modifiers::NONE, Key::Enter);
        let View::Publish(i) = h.state().layout.active_view() else { panic!("{:?}", h.state().layout.active_view()) };
        assert_eq!(h.state().publishers[i].topic, "/imu/data");
        assert_eq!(h.state().sup.entries().len(), sources, "nothing was subscribed to");

        // From Ctrl+P, Shift+Enter publishes to the topic instead of echoing it
        keys(&mut h, "ctrl+p");
        type_text(&mut h, "rosout");
        press(&mut h, Modifiers::SHIFT, Key::Enter);
        let View::Publish(i) = h.state().layout.active_view() else { panic!("{:?}", h.state().layout.active_view()) };
        assert_eq!(h.state().publishers[i].topic, "/rosout");
        assert_eq!(h.state().sup.entries().len(), sources);

        // Ctrl+O and Ctrl+P switch the picker between publishing and going somewhere
        keys(&mut h, "ctrl+p");
        keys(&mut h, "ctrl+o");
        assert!(h.state().picker.as_ref().is_some_and(|p| p.is_publishing()));
        keys(&mut h, "ctrl+p");
        assert!(h.state().picker.as_ref().is_some_and(|p| !p.is_publishing()));
    }

    #[test]
    fn delete_removes_a_publisher_and_ctrl_l_resets_it() {
        if std::env::var_os("AMENT_PREFIX_PATH").is_none() {
            return;
        }
        let mut h = harness();
        h.state_mut().graph = fake_graph();
        h.state_mut().open_publish("/imu/data", "sensor_msgs/msg/Imu");
        h.state_mut().open_publish("/kill", "std_msgs/msg/Bool");
        h.run_steps(2);
        let View::Publish(i) = h.state().layout.active_view() else { panic!() };
        let fresh = format!("{:?}", h.state().publishers[i].value);
        {
            let app = h.state_mut();
            let registry = app.registry.clone();
            app.publishers[i].edit_at_cursor(&registry.lock().unwrap());
        }
        assert_ne!(format!("{:?}", h.state().publishers[i].value), fresh, "the edit flipped data");
        keys(&mut h, "ctrl+l");
        assert_eq!(format!("{:?}", h.state().publishers[i].value), fresh, "back to the defaults");

        keys(&mut h, "delete");
        assert!(h.state().publishers[i].removed);
        assert_ne!(h.state().layout.active_view(), View::Publish(i), "the pane moved on");
        assert!(h.state().layout.views().iter().all(|v| *v != View::Publish(i)));
        // The other publisher keeps its place
        assert!(!h.state().publishers[0].removed);
        h.state_mut().open_publish("/kill", "std_msgs/msg/Bool");
        let View::Publish(again) = h.state().layout.active_view() else { panic!() };
        assert_ne!(again, i, "a removed publisher isn't reused");
    }

    #[test]
    fn removing_a_profile_publisher_can_take_it_out_of_the_file() {
        if std::env::var_os("AMENT_PREFIX_PATH").is_none() {
            return;
        }
        let path = std::env::temp_dir().join(format!("archon-remove-publish-{}.toml", std::process::id()));
        std::fs::write(
            &path,
            "# bench\n[[process]]\nname = \"agent\"\ncmd = \"sleep 30\"\n\n[[publish]]\nname = \"kill\"\ntopic = \"/kill\"\ntype = \"std_msgs/msg/Bool\"\nkey = \"F2\"\n",
        )
        .unwrap();
        let profile = Profile::load(&path).unwrap();
        let mut h = Harness::builder().with_size(vec2(1200.0, 700.0)).build_eframe(move |cc| {
            let startup = Startup {
                profile,
                keymap: Keymap::defaults(),
                problems: Vec::new(),
                with_ros: false,
                state_file: None,
                config: Default::default(),
                discovery: None,
                simulate: None,
                attach: Vec::new(),
            };
            ArchonApp::new(cc, startup)
        });
        h.run_steps(2);
        h.state_mut().show_view(View::Publish(0), false);
        keys(&mut h, "delete");
        assert!(h.state().removing.is_some(), "asks first");
        press(&mut h, Modifiers::NONE, Key::Y);
        assert!(h.state().publishers[0].removed);
        assert!(h.state().action_items().is_empty(), "gone from the picker's actions");
        let saved = Profile::load(&path).unwrap();
        assert!(saved.publishes.is_empty());
        assert_eq!(saved.processes.len(), 1);
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# bench"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saves_a_publisher_into_the_profile() {
        if std::env::var_os("AMENT_PREFIX_PATH").is_none() {
            return;
        }
        let path = std::env::temp_dir().join(format!("archon-save-test-{}.toml", std::process::id()));
        std::fs::write(&path, "# my bench\n[[process]]\nname = \"agent\"\ncmd = \"sleep 30\"\n").unwrap();
        let profile = Profile::load(&path).unwrap();
        let mut h = Harness::builder().with_size(vec2(1200.0, 700.0)).build_eframe(move |cc| {
            let startup = Startup {
                profile,
                keymap: Keymap::defaults(),
                problems: Vec::new(),
                with_ros: false,
                state_file: None,
                config: Default::default(),
                discovery: None,
                simulate: None,
                attach: Vec::new(),
            };
            ArchonApp::new(cc, startup)
        });
        h.run_steps(2);
        {
            let app = h.state_mut();
            app.graph = Arc::new(Graph {
                topics: vec![archon_ros::Topic {
                    name: "/kill".into(),
                    types: vec!["std_msgs/msg/Bool".into()],
                    publishers: 0,
                    subscribers: 1,
                }],
            });
            app.open_publish("/kill", "std_msgs/msg/Bool");
            let registry = app.registry.clone();
            app.publishers[0].edit_at_cursor(&registry.lock().unwrap());
            app.save_to_profile(0);
        }
        let saved = Profile::load(&path).unwrap();
        assert_eq!(saved.publishes.len(), 1);
        assert_eq!(saved.publishes[0].topic, "/kill");
        assert_eq!(saved.publishes[0].value["data"].as_bool(), Some(true));
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# my bench"), "the file's own text is kept");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn delete_removes_a_source_and_the_rest_renumber() {
        let mut h = harness();
        press(&mut h, Modifiers::NONE, Key::Num2);
        assert_eq!(h.state().current_source(), Some(1));
        keys(&mut h, "delete");
        let app = h.state();
        assert!(app.sup.entries()[1].removed);
        assert_ne!(app.layout.active_view(), View::Source(1), "the pane moved on");
        assert_eq!(app.sup.live().map(|(_, e)| e.name.as_str()).collect::<Vec<_>>(), ["agent", "thrusters"]);
        press(&mut h, Modifiers::NONE, Key::Num2);
        assert_eq!(h.state().current_source(), Some(2), "2 is now thrusters");
        keys(&mut h, "tab");
        assert_eq!(h.state().current_source(), Some(0), "tab skips the removed one");
        // Its name is free again
        assert_eq!(h.state().unique_name("imu"), "imu");
    }

    #[test]
    fn removing_a_profile_source_asks_whether_to_edit_the_file() {
        let path = std::env::temp_dir().join(format!("archon-remove-test-{}.toml", std::process::id()));
        std::fs::write(&path, "# my bench\n[[process]]\nname = \"agent\"\ncmd = \"sleep 30\"\n\n[[process]]\nname = \"imu\"\ncmd = \"sleep 30\"\n").unwrap();
        let profile = Profile::load(&path).unwrap();
        let mut h = Harness::builder().with_size(vec2(1200.0, 700.0)).build_eframe(move |cc| {
            let startup = Startup {
                profile,
                keymap: Keymap::defaults(),
                problems: Vec::new(),
                with_ros: false,
                state_file: None,
                config: Default::default(),
                discovery: None,
                simulate: None,
                attach: Vec::new(),
            };
            ArchonApp::new(cc, startup)
        });
        h.run_steps(2);
        press(&mut h, Modifiers::NONE, Key::Num1);
        keys(&mut h, "delete");
        assert!(h.state().removing.is_some(), "asks first");
        use egui_kittest::kittest::Queryable;
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        h.get_by_label(&format!("Remove agent? It's in {file}."));
        press(&mut h, Modifiers::NONE, Key::Escape);
        assert!(h.state().removing.is_none());
        assert!(!h.state().sup.entries()[0].removed, "Esc keeps it");

        keys(&mut h, "delete");
        press(&mut h, Modifiers::NONE, Key::Y);
        assert!(h.state().sup.entries()[0].removed);
        let left = std::fs::read_to_string(&path).unwrap();
        assert!(left.starts_with("# my bench"), "{left}");
        let saved = Profile::load(&path).unwrap();
        assert_eq!(saved.processes.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["imu"]);
        let _ = std::fs::remove_file(&path);
    }

    /// A mouse drag from one point to another, with modifiers held throughout
    fn drag(h: &mut Harness<'static, ArchonApp>, from: egui::Pos2, to: egui::Pos2, modifiers: Modifiers) {
        use egui::PointerButton::Primary;
        h.event(Event::ModifiersChanged(modifiers));
        h.event(Event::PointerMoved(from));
        h.run_steps(1);
        h.event(Event::PointerButton { pos: from, button: Primary, pressed: true, modifiers });
        h.run_steps(1);
        for step in 1..=10 {
            h.event(Event::PointerMoved(from + (to - from) * (step as f32 / 10.0)));
            h.run_steps(1);
        }
        h.event(Event::PointerButton { pos: to, button: Primary, pressed: false, modifiers });
        h.run_steps(2);
        h.event(Event::ModifiersChanged(Modifiers::NONE));
        h.run_steps(1);
    }

    #[test]
    fn alt_drag_moves_a_pane_below_another() {
        let mut h = harness();
        keys(&mut h, "ctrl+w v");
        let (left, right) = (h.state().layout.rect(0).unwrap(), h.state().layout.rect(1).unwrap());
        drag(&mut h, left.center(), egui::pos2(right.center().x, right.bottom() - 10.0), Modifiers::ALT);
        let app = h.state();
        assert!(matches!(app.layout.root(), crate::layout::Tile::Split { side_by_side: false, .. }), "now stacked");
        assert_eq!(app.layout.active_view(), View::Source(0), "the moved pane keeps its view and the focus");
    }

    #[test]
    fn alt_drag_onto_the_middle_swaps() {
        let mut h = harness();
        keys(&mut h, "ctrl+w v");
        let (left, right) = (h.state().layout.rect(0).unwrap(), h.state().layout.rect(1).unwrap());
        drag(&mut h, left.center(), right.center(), Modifiers::ALT);
        assert_eq!(h.state().layout.views(), [View::Source(1), View::Source(0)]);
    }

    #[test]
    fn dragging_from_the_sidebar_docks_a_new_pane() {
        use egui_kittest::kittest::Queryable;
        let mut h = harness();
        let from = h.get_by_label("Topics").rect().center();
        let pane = h.state().layout.rect(0).unwrap();
        drag(&mut h, from, egui::pos2(pane.left() + 10.0, pane.center().y), Modifiers::NONE);
        let app = h.state();
        assert_eq!(app.layout.len(), 2);
        assert_eq!(app.layout.active_view(), View::Topics);
    }

    #[test]
    fn saves_and_loads_named_layouts() {
        let state = std::env::temp_dir().join(format!("archon-named-{}", std::process::id())).join("bench.toml");
        let profile = Profile::parse(PROFILE).unwrap();
        let state_file = Some(state.clone());
        let mut h = Harness::builder().with_size(vec2(1200.0, 700.0)).build_eframe(move |cc| {
            ArchonApp::new(
                cc,
                Startup {
                    profile,
                    keymap: Keymap::defaults(),
                    problems: Vec::new(),
                    with_ros: false,
                    state_file,
                    config: Default::default(),
                    discovery: None,
                    simulate: None,
                    attach: Vec::new(),
                },
            )
        });
        h.run_steps(2);
        keys(&mut h, "ctrl+w v");
        keys(&mut h, "T");
        keys(&mut h, "space l s");
        type_text(&mut h, "flash test");
        press(&mut h, Modifiers::NONE, Key::Enter);
        assert_eq!(h.state().flash.as_ref().map(|f| f.text.as_str()), Some("saved layout flash test"));

        keys(&mut h, "ctrl+w q");
        assert_eq!(h.state().layout.len(), 1);
        keys(&mut h, "space l l");
        press(&mut h, Modifiers::NONE, Key::Enter);
        assert_eq!(h.state().layout.views(), [View::Source(0), View::Topics]);
        let _ = std::fs::remove_dir_all(state.parent().unwrap());
    }

    #[test]
    fn p_plots_the_field_at_the_cursor() {
        let mut h = harness();
        fake_topic(h.state_mut(), 1);
        keys(&mut h, "2");
        keys(&mut h, "j j j");
        keys(&mut h, "p");
        let app = h.state();
        assert_eq!(app.plots.len(), 1);
        assert_eq!(app.plots[0].series, [(1, "linear_acceleration.x".to_owned())]);
        assert_eq!(app.layout.views(), [View::Source(1), View::Plot(0)], "beside the inspector");
        assert_eq!(app.layout.active_view(), View::Source(1), "the focus stays to add more");
        keys(&mut h, "j");
        keys(&mut h, "p");
        assert_eq!(h.state().plots[0].series.len(), 2, "into the same plot");
        // Records every value from now on
        h.state().topic_states[&1].lock().unwrap().update(archon_msg::Value::Struct(vec![(
            "linear_acceleration".into(),
            archon_msg::Value::Struct(vec![("x".into(), archon_msg::Value::Float64(0.5))]),
        )]));
        assert_eq!(h.state().topic_states[&1].lock().unwrap().traced("linear_acceleration.x").unwrap().len(), 1);
        // Back on a struct, p says why not
        keys(&mut h, "g g");
        keys(&mut h, "p");
        assert!(h.state().flash.as_ref().is_some_and(|f| f.text.starts_with("only numbers")));
    }

    #[test]
    fn bytes_view_picks_and_copies_messages() {
        let mut h = harness();
        fake_topic(h.state_mut(), 1);
        {
            let state = h.state().topic_states[&1].clone();
            let mut state = state.lock().unwrap();
            state.record(&[0, 1, 0, 0, 3, 0, 0, 0, b'h', b'i', 0], None);
            state.record(&[0, 1, 0, 0, 9], Some("message ends early".into()));
        }
        keys(&mut h, "2");
        keys(&mut h, "v v");
        assert!(h.state().showing_bytes(1));
        keys(&mut h, "k");
        assert_eq!(h.state().bytes[1].selected, Some(1));
        keys(&mut h, "y");
        assert_eq!(h.state().flash.as_ref().map(|f| f.text.as_str()), Some("copied message #1 as hex"));
        keys(&mut h, "G");
        assert_eq!(h.state().bytes[1].selected, None, "G follows the newest again");
    }

    #[test]
    fn an_empty_bench_starts_on_getting_started() {
        let mut h = harness_with("");
        assert_eq!(h.state().layout.active_view(), View::Welcome);
        keys(&mut h, "T");
        keys(&mut h, "F1");
        assert_eq!(h.state().layout.active_view(), View::Welcome, "F1 brings it back");
    }

    #[test]
    fn starts_the_agent_and_a_console_then_saves_the_bench() {
        let dir = std::env::temp_dir().join(format!("archon-bench-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("archon.toml");
        std::fs::write(&path, "").unwrap();
        let profile = Profile { path: Some(path.clone()), ..Profile::default() };
        // A stand-in agent that says what it was asked to do
        let config =
            bench::Config { micro_ros_agent: Some("echo agent".into()), firmware_simulator: None, upload_tool: None };
        let mut h = Harness::builder().with_size(vec2(1200.0, 700.0)).build_eframe(move |cc| {
            ArchonApp::new(
                cc,
                Startup {
                    profile,
                    keymap: Keymap::defaults(),
                    problems: Vec::new(),
                    with_ros: false,
                    state_file: None,
                    config,
                    discovery: None,
                    simulate: None,
                    attach: Vec::new(),
                },
            )
        });
        h.run_steps(2);
        {
            let app = h.state_mut();
            app.picking =
                Some(Pick::Agent(vec![Port::Serial("/dev/serial/by-id/usb-board-if02".into()), Port::Udp(8888)]));
            app.picked(0);
            app.picking = Some(Pick::Serial(vec![Port::Serial("/dev/archon-test-missing".into())]));
            app.picked(0);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !h.state().sup.entries()[0].buffer.iter().any(|l| l.text.contains("agent serial --dev")) {
            assert!(Instant::now() < deadline, "the agent didn't run");
            std::thread::sleep(Duration::from_millis(20));
            h.run_steps(1);
        }
        let names: Vec<String> = h.state().sup.entries().iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, ["agent", "console"]);

        h.state_mut().save_bench();
        let saved = Profile::load(&path).unwrap();
        assert_eq!(saved.processes[0].cmd, "echo agent serial --dev /dev/serial/by-id/usb-board-if02");
        assert_eq!(saved.processes[0].restart, RestartPolicy::OnFailure);
        assert!(saved.processes[0].autostart);
        assert_eq!(saved.serials[0].port, "/dev/archon-test-missing");
        h.state_mut().save_bench();
        assert!(h.state().flash.as_ref().is_some_and(|f| f.text.starts_with("nothing new")), "saved once, not twice");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// With a micro-ROS board plugged in, starts the real agent on it the way the button does, and waits for the
    /// firmware's topics: `cargo test -p archon real_agent -- --ignored`
    #[test]
    #[ignore]
    fn starts_the_real_agent_on_the_board() {
        let profile = Profile::default();
        let mut h = Harness::builder().with_size(vec2(1200.0, 700.0)).build_eframe(move |cc| {
            let startup = Startup {
                profile,
                keymap: Keymap::defaults(),
                problems: Vec::new(),
                with_ros: true,
                state_file: None,
                config: Default::default(),
                discovery: None,
                simulate: None,
                attach: Vec::new(),
            };
            ArchonApp::new(cc, startup)
        });
        h.run_steps(2);
        let ports = bench::serial_ports();
        let port = ports.iter().position(|p| p.label().ends_with("if02")).expect("a board's micro-ROS port (if02)");
        println!("agent: {:?}", h.state().find_agent().map(|a| a.found));
        h.state_mut().picking = Some(Pick::Agent(ports));
        h.state_mut().picked(port);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            h.run_steps(1);
            let topics: Vec<String> = h.state().graph.topics.iter().map(|t| t.name.clone()).collect();
            if topics.iter().any(|t| t.ends_with("state/firmware")) {
                println!("firmware topics: {topics:?}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no firmware topics; agent said:\n{}",
                h.state().sup.entries()[0].buffer.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        h.state_mut().sup.stop_all();
    }

    /// Runs IVCBoard2 in firmware_simulator from archon, the way Space n f does, until its topics show; then removes
    /// the simulated board and checks the simulator went with it
    #[test]
    #[ignore]
    fn simulates_a_board_until_its_topics_show() {
        let profile = Profile::default();
        let mut h = Harness::builder().with_size(vec2(1200.0, 700.0)).build_eframe(move |cc| {
            let startup = Startup {
                profile,
                keymap: Keymap::defaults(),
                problems: Vec::new(),
                with_ros: true,
                state_file: None,
                config: Default::default(),
                discovery: None,
                simulate: None,
                attach: Vec::new(),
            };
            ArchonApp::new(cc, startup)
        });
        h.run_steps(2);
        let builds = bench::firmware_builds(&dirs::home_dir().unwrap());
        let ivc = builds.into_iter().find(|f| f.name == "IVCBoard2").expect("an IVCBoard2 build");
        h.state_mut().simulate(&ivc);
        let names: Vec<String> = h.state().sup.entries().iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, ["sim ivcboard2", "agent ivcboard2", "console ivcboard2"]);
        let said = |h: &Harness<'static, ArchonApp>| {
            h.state()
                .sup
                .entries()
                .iter()
                .map(|e| format!("--- {}\n{}", e.name, e.buffer.iter().map(|l| l.text.as_str()).collect::<String>()))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            h.run_steps(1);
            let topics: Vec<String> = h.state().graph.topics.iter().map(|t| t.name.clone()).collect();
            if topics.iter().any(|t| t.contains("/ivc/")) {
                println!("firmware topics: {topics:?}");
                break;
            }
            assert!(Instant::now() < deadline, "no IVC topics; the sources said:\n{}", said(&h));
            std::thread::sleep(Duration::from_millis(100));
        }
        let console = h.state().sup.entries()[2].buffer.total();
        println!("console lines: {console}");

        for id in 0..3 {
            h.state_mut().remove_source(id, false);
        }
        h.run_steps(2);
        assert_eq!(h.state().sup.live().count(), 0);
        assert_eq!(h.state().layout.active_view(), View::Welcome, "nothing left, so back to getting started");
        let deadline = Instant::now() + Duration::from_secs(5);
        while bench::simulated_ports()
            .iter()
            .any(|p| p.path().is_some_and(|p| p.to_string_lossy().contains("archon-ivcboard2")))
        {
            assert!(Instant::now() < deadline, "the simulator is still running");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// A second copy of a board running here would publish the same topics, so it goes to an isolated window
    #[test]
    fn a_second_copy_of_a_board_opens_isolated() {
        let home = dirs::home_dir().unwrap();
        let (Ok(simulator), Some(ivc)) = (
            bench::find_simulator(None, &home),
            bench::firmware_builds(&home).into_iter().find(|f| f.name == "IVCBoard2"),
        ) else {
            return;
        };
        let board = bench::board_for(&simulator, &ivc.image).unwrap();
        let mut h = harness();
        h.state_mut().config.micro_ros_agent = Some("echo agent".into());
        // The agent source stands in for a running simulation of the same board
        h.state_mut().sup.start(0);
        h.run_steps(2);
        h.state_mut().simulated.push((0, board));
        let before = h.state().sup.entries().len();
        h.state_mut().simulate(&ivc);
        let app = h.state();
        assert_eq!(app.sup.entries().len(), before, "nothing new starts here");
        assert_eq!(
            app.spawned,
            [vec!["--isolated".to_owned(), "--simulate".to_owned(), ivc.image.display().to_string()]]
        );

        // Space n i opens an empty one
        keys(&mut h, "space n i");
        assert_eq!(h.state().spawned.last().unwrap(), &["--isolated"]);
    }

    /// Two IVCBoard2s at once: one in an isolated archon, one with its own agent on the usual network. Each side
    /// sees only its own board's heartbeat. Run alone, since it sets this process's ROS environment:
    /// `cargo test -p archon two_copies -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn two_copies_of_a_board_stay_apart() {
        let port = bench::free_discovery_port().unwrap();
        for (key, value) in bench::isolated_env(port) {
            // SAFETY: run alone, before ROS or anything else reads the environment
            unsafe { std::env::set_var(key, value) };
        }
        let mut h = Harness::builder().with_size(vec2(1200.0, 700.0)).build_eframe(move |cc| {
            let startup = Startup {
                profile: Profile::default(),
                keymap: Keymap::defaults(),
                problems: Vec::new(),
                with_ros: true,
                state_file: None,
                config: Default::default(),
                discovery: Some(port),
                simulate: None,
                attach: Vec::new(),
            };
            ArchonApp::new(cc, startup)
        });
        h.run_steps(2);
        let home = dirs::home_dir().unwrap();
        let ivc =
            bench::firmware_builds(&home).into_iter().find(|f| f.name == "IVCBoard2").expect("an IVCBoard2 build");
        h.state_mut().simulate(&ivc);

        // The other copy, on the usual network, the way a terminal would run it
        let simulator = bench::find_simulator(None, &home).unwrap();
        let board = bench::board_for(&simulator, &ivc.image).unwrap();
        let agent = h.state().find_agent().unwrap();
        let (sim, agent, _) = bench::simulation_commands(&simulator, &ivc, &board, &agent, "plaintest");
        let usual = |cmd: &str| {
            use std::os::unix::process::CommandExt;
            std::process::Command::new("bash")
                .args(["-c", cmd])
                .env_remove("ROS_DISCOVERY_SERVER")
                .env_remove("ROS_SUPER_CLIENT")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .process_group(0)
                .spawn()
                .unwrap()
        };
        let others = [usual(&sim), usual(&agent)];
        let heartbeats_outside = || {
            let out = std::process::Command::new("ros2")
                .args(["topic", "info", "/talos/heartbeat", "--no-daemon"])
                .env_remove("ROS_DISCOVERY_SERVER")
                .env_remove("ROS_SUPER_CLIENT")
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.lines().find_map(|l| l.strip_prefix("Publisher count: ")).and_then(|n| n.trim().parse::<usize>().ok())
        };
        let heartbeats_here =
            |h: &Harness<'static, ArchonApp>| h.state().graph.topic("/talos/heartbeat").map(|t| t.publishers);

        let deadline = Instant::now() + Duration::from_secs(90);
        let (mut here, mut outside) = (None, None);
        while Instant::now() < deadline && !(here.is_some() && outside.is_some_and(|n| n > 0)) {
            h.run_steps(1);
            here = heartbeats_here(&h);
            outside = heartbeats_outside();
            std::thread::sleep(Duration::from_millis(500));
        }
        // Give each side a moment to hear about the other, if it's going to
        for _ in 0..10 {
            h.run_steps(1);
            std::thread::sleep(Duration::from_millis(300));
        }
        let (here, outside) = (heartbeats_here(&h), heartbeats_outside());
        let names: Vec<String> = h.state().graph.topics.iter().map(|t| t.name.clone()).collect();
        println!("isolated archon: {here:?} heartbeat publishers, topics {names:?}");
        println!("usual network: {outside:?} heartbeat publishers");
        for child in &others {
            // SAFETY: signalling the process groups this test started
            unsafe { libc::killpg(child.id() as i32, libc::SIGTERM) };
        }
        h.state_mut().sup.stop_all();
        assert_eq!(here, Some(1), "the isolated window sees its own board, and only that one");
        assert_eq!(outside, Some(1), "the usual network sees the other board, and not the isolated one");
    }

    /// Two IVCBoard2s in one simulated pool: archon runs board A's agent on the usual network and sends bytes from
    /// the publish form, one Send each; board B's agent is on a discovery server of its own, with `ros2 topic echo`
    /// listening there. Run alone: `cargo test -p archon ivc_bytes -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn ivc_bytes_cross_the_pool_from_the_publish_form() {
        use std::os::unix::process::CommandExt;
        let home = dirs::home_dir().unwrap();
        let simulator = bench::find_simulator(None, &home).unwrap();
        let ivc =
            bench::firmware_builds(&home).into_iter().find(|f| f.name == "IVCBoard2").expect("an IVCBoard2 build");
        let dir = std::env::temp_dir().join(format!("archon-ivc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a"), dir.join("b"));
        let board = simulator.checkout.join("boards/talos/ivc_board.toml");
        let robot = format!(
            "[environment]\nfile = \"{env}\"\n\n[[bus]]\nname = \"talos\"\n\n[[bus]]\nname = \"surface\"\n\n\
             [[board]]\nname = \"talos_ivc\"\nboard = \"{board}\"\nelf = \"{elf}\"\nrom_boot = true\nbus = \"talos\"\n\
             position_m = [2.0, 6.0, 1.0]\nusb_cdc2 = \"pty:{a}-ros\"\n\n\
             [[board]]\nname = \"surface_ivc\"\nboard = \"{board}\"\nelf = \"{elf}\"\nrom_boot = true\nbus = \"surface\"\n\
             power_on_ms = 37\nposition_m = [7.0, 6.0, 1.0]\nusb_cdc2 = \"pty:{b}-ros\"\n",
            env = simulator.checkout.join("environments/pool.toml").display(),
            board = board.display(),
            elf = ivc.image.display(),
            a = a.display(),
            b = b.display(),
        );
        let robot_file = dir.join("robot.toml");
        std::fs::write(&robot_file, robot).unwrap();
        let port = bench::free_discovery_port().unwrap();
        let agent = bench::find_agent(None, &home).unwrap();
        let b_agent = bench::agent_command(&agent, &Port::Simulated(PathBuf::from(format!("{}-ros", b.display()))));
        let script = format!(
            "{sim} robot {robot} --realtime --time 600s > {dir}/robot.out 2>&1 &\n\
             fast-discovery-server -i 0 -l 127.0.0.1 -p {port} > /dev/null 2>&1 &\n\
             while [ ! -e {b}-ros ]; do sleep 0.2; done\n\
             (export ROS_DISCOVERY_SERVER=127.0.0.1:{port}; {b_agent}) > {dir}/agent_b.out 2>&1 &\n\
             sleep 8\n\
             ROS_DISCOVERY_SERVER=127.0.0.1:{port} ROS_SUPER_CLIENT=TRUE PYTHONUNBUFFERED=1 \
             ros2 topic echo /talos/ivc/rx_new std_msgs/msg/Int8 --no-daemon > {dir}/echo_b.out 2>&1 &\n\
             wait\n",
            sim = bench::quote(&simulator.binary),
            robot = robot_file.display(),
            dir = dir.display(),
            b = b.display(),
        );
        let mut others = std::process::Command::new("bash")
            .args(["-c", &script])
            .env_remove("ROS_DISCOVERY_SERVER")
            .env_remove("ROS_SUPER_CLIENT")
            .process_group(0)
            .spawn()
            .unwrap();

        let mut h = Harness::builder().with_size(vec2(1200.0, 700.0)).build_eframe(move |cc| {
            let startup = Startup {
                profile: Profile::default(),
                keymap: Keymap::defaults(),
                problems: Vec::new(),
                with_ros: true,
                state_file: None,
                config: Default::default(),
                discovery: None,
                simulate: None,
                attach: Vec::new(),
            };
            ArchonApp::new(cc, startup)
        });
        h.run_steps(2);
        let a_ros = PathBuf::from(format!("{}-ros", a.display()));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !a_ros.exists() {
            assert!(Instant::now() < deadline, "the robot didn't start");
            std::thread::sleep(Duration::from_millis(200));
        }
        // Board A's agent, started the way Space n a starts it on a simulated port
        h.state_mut().picking = Some(Pick::Agent(vec![Port::Simulated(a_ros)]));
        h.state_mut().picked(0);
        let deadline = Instant::now() + Duration::from_secs(60);
        while h.state().graph.topic("/talos/ivc/enqueue_data").is_none_or(|t| t.subscribers == 0) {
            assert!(Instant::now() < deadline, "board A's enqueue_data never showed up");
            h.run_steps(1);
            std::thread::sleep(Duration::from_millis(200));
        }
        std::thread::sleep(Duration::from_secs(4));

        h.state_mut().open_publish("/talos/ivc/enqueue_data", "std_msgs/msg/Int8");
        let View::Publish(form) = h.state().layout.active_view() else { panic!("no form") };
        let mut sent = Vec::new();
        for byte in [90i8, 17, 66, 33] {
            {
                let app = h.state_mut();
                let registry = app.registry.clone();
                let table: toml::Table = toml::from_str(&format!("data = {byte}")).unwrap();
                app.publishers[form].apply_table(&registry.lock().unwrap(), &table).unwrap();
            }
            keys(&mut h, "ctrl+s");
            sent.push(byte);
            // The C firmware drops a byte queued while another is in the water, so one at a time
            for _ in 0..40 {
                h.run_steps(1);
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        let errors = h.state().ros.as_ref().map(|l| l.ros.take_errors()).unwrap_or_default();
        let heard = std::fs::read_to_string(dir.join("echo_b.out")).unwrap_or_default();
        println!("sent {sent:?}; board B published:\n{heard}\nerrors: {errors:?}");
        h.state_mut().sup.stop_all();
        // SAFETY: signalling the process group this test started
        unsafe { libc::killpg(others.id() as i32, libc::SIGTERM) };
        let _ = others.wait();
        let got: Vec<i8> =
            heard.lines().filter_map(|l| l.strip_prefix("data: ")).filter_map(|n| n.trim().parse().ok()).collect();
        assert!(errors.is_empty(), "{errors:?}");
        assert!(!got.is_empty(), "nothing crossed the pool");
    }

    /// A robot file with two IVCBoard2s in one pool: archon runs it, with talos_ivc's agent and console here and
    /// surface_ivc's sent to an isolated window, which this test plays by running what that window would. Each
    /// network then sees one board. Run alone: `cargo test -p archon runs_a_robot -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn runs_a_robot_with_a_second_copy_isolated() {
        use std::os::unix::process::CommandExt;
        let home = dirs::home_dir().unwrap();
        let simulator = bench::find_simulator(None, &home).unwrap();
        let ivc =
            bench::firmware_builds(&home).into_iter().find(|f| f.name == "IVCBoard2").expect("an IVCBoard2 build");
        let dir = std::env::temp_dir().join(format!("archon-robot-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let board = simulator.checkout.join("boards/talos/ivc_board.toml");
        let file = dir.join("two_ivc.toml");
        std::fs::write(
            &file,
            format!(
                "[environment]\nfile = \"{env}\"\n\n[[bus]]\nname = \"talos\"\n\n[[bus]]\nname = \"surface\"\n\n\
                 [[board]]\nname = \"talos_ivc\"\nboard = \"{board}\"\nelf = \"{elf}\"\nrom_boot = true\nbus = \"talos\"\n\
                 position_m = [2.0, 6.0, 1.0]\n\n\
                 [[board]]\nname = \"surface_ivc\"\nboard = \"{board}\"\nelf = \"{elf}\"\nrom_boot = true\n\
                 bus = \"surface\"\npower_on_ms = 37\nposition_m = [7.0, 6.0, 1.0]\n",
                env = simulator.checkout.join("environments/pool.toml").display(),
                board = board.display(),
                elf = ivc.image.display(),
            ),
        )
        .unwrap();
        let robot = crate::robot::load(&file, None).unwrap();

        let mut h = Harness::builder().with_size(vec2(1200.0, 700.0)).build_eframe(move |cc| {
            let startup = Startup {
                profile: Profile::default(),
                keymap: Keymap::defaults(),
                problems: Vec::new(),
                with_ros: true,
                state_file: None,
                config: Default::default(),
                discovery: None,
                simulate: None,
                attach: Vec::new(),
            };
            ArchonApp::new(cc, startup)
        });
        h.run_steps(2);
        h.state_mut().simulate_robot(&robot);
        let names: Vec<String> = h.state().sup.entries().iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, ["robot twoivc", "agent talos_ivc", "console talos_ivc"]);
        let window = h.state().spawned.last().cloned().expect("an isolated window for surface_ivc");
        println!("isolated window: archon {}", window.join(" "));
        assert_eq!(window[0], "--isolated");
        let attach: Vec<crate::robot::Attach> =
            window[1..].chunks(2).map(|pair| crate::robot::Attach::parse(&pair[0], &pair[1]).unwrap()).collect();
        let Some(crate::robot::Attach::Agent { name, path }) = attach.first() else { panic!("{attach:?}") };
        assert_eq!(name, "surface_ivc");

        // What the isolated window runs: surface_ivc's agent on a discovery server of its own
        let port = bench::free_discovery_port().unwrap();
        let agent = bench::find_agent(None, &home).unwrap();
        let script = format!(
            "fast-discovery-server -i 0 -l 127.0.0.1 -p {port} > /dev/null 2>&1 &\n\
             while [ ! -e {p} ]; do sleep 0.2; done\n\
             export ROS_DISCOVERY_SERVER=127.0.0.1:{port}; {agent} > {dir}/agent_b.out 2>&1 &\nwait\n",
            p = bench::quote(path),
            agent = bench::agent_command(&agent, &Port::Simulated(path.clone())),
            dir = dir.display(),
        );
        let mut other = std::process::Command::new("bash")
            .args(["-c", &script])
            .env_remove("ROS_DISCOVERY_SERVER")
            .env_remove("ROS_SUPER_CLIENT")
            .process_group(0)
            .spawn()
            .unwrap();
        let heartbeats_isolated = || {
            let out = std::process::Command::new("ros2")
                .args(["topic", "info", "/talos/heartbeat", "--no-daemon"])
                .env("ROS_DISCOVERY_SERVER", format!("127.0.0.1:{port}"))
                .env("ROS_SUPER_CLIENT", "TRUE")
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.lines().find_map(|l| l.strip_prefix("Publisher count: ")).and_then(|n| n.trim().parse::<usize>().ok())
        };
        let deadline = Instant::now() + Duration::from_secs(90);
        let (mut here, mut there) = (None, None);
        while Instant::now() < deadline && !(here == Some(1) && there == Some(1)) {
            h.run_steps(1);
            here = h.state().graph.topic("/talos/heartbeat").map(|t| t.publishers);
            there = heartbeats_isolated();
            std::thread::sleep(Duration::from_millis(500));
        }
        let console = h.state().sup.entries()[2].buffer.total();
        println!("here: {here:?} heartbeat publishers; isolated: {there:?}; talos_ivc console lines: {console}");
        h.state_mut().sup.stop_all();
        // SAFETY: signalling the process group this test started
        unsafe { libc::killpg(other.id() as i32, libc::SIGTERM) };
        let _ = other.wait();
        assert_eq!(here, Some(1), "this window sees talos_ivc only");
        assert_eq!(there, Some(1), "the isolated network sees surface_ivc only");
    }

    /// Space n r, a twice, Ctrl+S: a robot file with two IVCBoard2s that the simulator runs
    #[test]
    fn builds_a_robot_file_the_simulator_runs() {
        let home = dirs::home_dir().unwrap();
        let (Ok(simulator), true) =
            (bench::find_simulator(None, &home), bench::firmware_builds(&home).iter().any(|f| f.name == "IVCBoard2"))
        else {
            return;
        };
        // A checkout of links to the real boards and environments, so the file lands somewhere disposable
        let checkout = std::env::temp_dir().join(format!("archon-built-robot-{}", std::process::id()));
        std::fs::create_dir_all(checkout.join("robots")).unwrap();
        for dir in ["boards", "environments"] {
            let _ = std::os::unix::fs::symlink(simulator.checkout.join(dir), checkout.join(dir));
        }
        let mut h = harness();
        keys(&mut h, "space n r");
        assert_eq!(h.state().layout.active_view(), View::RobotBuilder);
        h.state_mut().builder.as_mut().unwrap().checkout = checkout.clone();
        for _ in 0..2 {
            keys(&mut h, "a");
            assert!(h.state().picker.is_some(), "a lists the builds");
            type_text(&mut h, "IVCBoard2");
            press(&mut h, Modifiers::NONE, Key::Enter);
        }
        let builder = h.state().builder.as_ref().unwrap();
        assert_eq!(builder.boards.len(), 2);
        assert!(builder.boards.iter().all(|b| b.transport == crate::robot::Transport::Usb && b.board.is_some()));
        keys(&mut h, "ctrl+s");
        let file = checkout.join("robots/my_ivcboard2_test.toml");
        assert!(file.is_file(), "{:?}", h.state().flash.as_ref().map(|f| f.text.clone()));

        // The simulator takes it: both boards boot in the pool
        let out = std::process::Command::new(&simulator.binary)
            .args(["robot", &file.display().to_string(), "--time", "20ms"])
            .current_dir(&checkout)
            .output()
            .unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "{text}");
        let _ = std::fs::remove_dir_all(&checkout);
    }

    /// Flashing runs the tool as a source with the image and flags asked for; a finished flash makes way for the next
    #[test]
    fn flashes_with_upload_tool_as_a_source() {
        let dir = std::env::temp_dir().join(format!("archon-flash-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("upload_tool");
        std::fs::write(&tool, "#!/bin/sh\necho \"upload_tool $*\"\n").unwrap();
        std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let (full, ota) = (dir.join("esc_board_with_bl.uf2"), dir.join("esc_board_ota.uf2"));
        std::fs::write(&full, b"").unwrap();
        std::fs::write(&ota, b"").unwrap();

        let mut h = harness_with("");
        h.state_mut().config.upload_tool = Some(tool.clone());
        let firmware = bench::Firmware { image: full.clone(), name: "esc_board".into(), built: None };
        h.state_mut().pick_flash_mode(&firmware);
        assert!(h.state().picker.is_some(), "asks how much to flash");
        press(&mut h, Modifiers::NONE, Key::Enter);
        let said = |h: &Harness<'static, ArchonApp>, id: SourceId| {
            h.state().sup.entries()[id].buffer.iter().map(|l| l.text.clone()).collect::<String>()
        };
        let first = h.state().flashes[0];
        let deadline = Instant::now() + Duration::from_secs(5);
        while !said(&h, first).contains("upload_tool") {
            assert!(Instant::now() < deadline, "{}", said(&h, first));
            h.run_steps(1);
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(said(&h, first).contains(&format!("upload_tool {}", ota.display())), "the app image, no -f");
        assert_eq!(h.state().layout.active_view(), View::Source(first), "its output shows");
        while h.state().sup.entries()[first].status.is_active() {
            h.run_steps(1);
            std::thread::sleep(Duration::from_millis(50));
        }

        h.state_mut().flash_board("esc_board", &full, crate::flash::Mode::Full);
        assert!(h.state().sup.entries()[first].removed, "the finished flash made way");
        let second = *h.state().flashes.last().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !said(&h, second).contains("upload_tool") {
            assert!(Instant::now() < deadline);
            h.run_steps(1);
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(said(&h, second).contains(&format!("upload_tool -f {}", full.display())), "{}", said(&h, second));
    }

    /// Flashes the RP2040 on the desk with what it already runs (titan_firmware's build/archon_test), through
    /// archon's flash and upload_tool, with its console open; the console has to come back by itself.
    /// `cargo test -p archon flashes_the_real_board -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn flashes_the_real_board() {
        let home = dirs::home_dir().unwrap();
        let titan = crate::robot::titan_firmware_dir(&home).unwrap();
        let image = titan.join("build/archon_test/out/template_project.uf2");
        let console = bench::serial_ports()
            .into_iter()
            .filter_map(|p| p.path().map(std::path::Path::to_path_buf))
            .find(|p| p.to_string_lossy().ends_with("-if00"))
            .expect("the board's console (if00)");
        let mut h = harness_with(&format!("[[serial]]\nname = \"board\"\nport = \"{}\"\n", console.display()));
        let deadline = Instant::now() + Duration::from_secs(10);
        while h.state().sup.entries()[0].status != archon_core::source::Status::Running {
            assert!(Instant::now() < deadline, "the console didn't open");
            h.run_steps(1);
            std::thread::sleep(Duration::from_millis(100));
        }

        h.state_mut().flash_board("archon test", &image, crate::flash::Mode::Full);
        let flash = *h.state().flashes.last().unwrap();
        let output = |h: &Harness<'static, ArchonApp>| -> String {
            h.state().sup.entries()[flash].buffer.iter().map(|l| format!("{}\n", l.text)).collect()
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut answered = false;
        while h.state().sup.entries()[flash].status.is_active() {
            assert!(Instant::now() < deadline, "the flash didn't finish:\n{}", output(&h));
            h.run_steps(1);
            // archon asks which board, as upload_tool does for one it doesn't know; answer with this board
            if let Some(Pick::FlashAnswer(_, _)) = &h.state().picking {
                let lines: Vec<String> = h.state().sup.entries()[flash].buffer.iter().map(|l| l.text.clone()).collect();
                let asked = crate::flash::question(&lines).unwrap();
                println!("archon asked: {} {:?}", asked.prompt, asked.choices);
                // This board, and yes to its not being a known pico: it's the image it already runs
                let index = asked
                    .choices
                    .iter()
                    .position(|c| c.detail.contains("E6613852838B7C32") || c.reply == "y")
                    .expect("an answer for this board");
                let app = h.state_mut();
                app.picker = None;
                app.picked(index);
                answered = true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        println!("answered a question: {answered}");
        let entry = &h.state().sup.entries()[flash];
        let said = output(&h);
        println!("--- {}\n{said}--- {:?}", entry.name, entry.status);
        assert_eq!(entry.status, archon_core::source::Status::Exited(Some(0)), "{said}");

        // The console notices the board went away, then picks it up again
        let mut saw_it_go = false;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            h.run_steps(1);
            let status = h.state().sup.entries()[0].status.clone();
            saw_it_go |= status != archon_core::source::Status::Running;
            if saw_it_go && status == archon_core::source::Status::Running {
                break;
            }
            assert!(Instant::now() < deadline, "the console didn't come back: {status:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
        let console_said: Vec<String> = h.state().sup.entries()[0].buffer.iter().map(|l| l.text.clone()).collect();
        println!("--- console\n{}", console_said.join("\n"));
    }

    #[test]
    fn formats_uptime() {
        use crate::sidebar::uptime;
        assert_eq!(uptime(Duration::from_secs(42)), "42s");
        assert_eq!(uptime(Duration::from_secs(252)), "4m12s");
        assert_eq!(uptime(Duration::from_secs(3840)), "1h04m");
    }

    /// Renders the main states to target/screenshots, to look at the UI without a window. Needs a GPU (or
    /// lavapipe), so it only runs when asked: `cargo test -p archon screenshots -- --ignored`
    #[test]
    #[ignore]
    fn screenshots() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = harness_with(
            r#"
            [[process]]
            name = "agent"
            cmd = "echo '[INFO] [1.0] [agent]: session established'; sleep 0.2; echo '[WARN] [1.2] [agent]: slow heartbeat'; sleep 30"
            autostart = true
            [[process]]
            name = "board"
            cmd = "echo 'imu init ok'; sleep 0.1; echo '[ERROR] ESC 3 fault'; sleep 30"
            autostart = true
            [[process]]
            name = "imu"
            cmd = "sleep 30"
            [[watch]]
            topic = "/imu/data"
            min_hz = 100
            "#,
        );
        std::thread::sleep(Duration::from_millis(500));
        {
            let app = h.state_mut();
            app.graph = fake_graph();
            fake_topic(app, 2);
        }
        h.run_steps(3);
        let save = |h: &mut Harness<'static, ArchonApp>, name: &str| {
            h.run_steps(2);
            h.render().unwrap().save(dir.join(format!("{name}.png"))).unwrap();
        };
        save(&mut h, "main");
        {
            let mut welcome = harness_with("");
            welcome.run_steps(2);
            save(&mut welcome, "welcome");
            keys(&mut welcome, "space n a");
            save(&mut welcome, "agent-picker");
            keys(&mut welcome, "escape");
            keys(&mut welcome, "space n s");
            save(&mut welcome, "console-picker");
            keys(&mut welcome, "escape");
            keys(&mut welcome, "space n f");
            save(&mut welcome, "firmware-picker");
            keys(&mut welcome, "escape");
            keys(&mut welcome, "space n u");
            save(&mut welcome, "flash-picker");
            keys(&mut welcome, "escape");
            // The robot builder with two IVCs and an ESC board, when there are builds to show
            let home = dirs::home_dir().unwrap();
            let builds = bench::firmware_builds(&home);
            if let Ok(simulator) = bench::find_simulator(None, &home) {
                keys(&mut welcome, "space n r");
                for name in ["IVCBoard2", "IVCBoard2", "esc_board"] {
                    if let Some(build) = builds.iter().find(|b| b.name == name) {
                        let board = bench::board_for(&simulator, &build.image);
                        welcome.state_mut().builder.as_mut().unwrap().add(build.clone(), board);
                    }
                }
                save(&mut welcome, "robot-builder");
            }
        }
        {}
        // The question a removal asks when the source is in the profile
        h.state_mut().profile_path = Some("archon.toml".into());
        h.state_mut().removing = Some((Removal::Source(1), "board".into()));
        save(&mut h, "remove");
        keys(&mut h, "escape");
        h.state_mut().profile_path = None;
        // All logs on the left, the inspector above the topics table on the right
        keys(&mut h, "L");
        keys(&mut h, "ctrl+w v");
        keys(&mut h, "3");
        keys(&mut h, "j j j");
        keys(&mut h, "ctrl+w s");
        keys(&mut h, "T");
        save(&mut h, "tiled");
        press(&mut h, Modifiers::CTRL, Key::P);
        type_text(&mut h, "im");
        save(&mut h, "picker-sources");
        press(&mut h, Modifiers::NONE, Key::Escape);
        press(&mut h, Modifiers::NONE, Key::Questionmark);
        save(&mut h, "keys");
        press(&mut h, Modifiers::NONE, Key::Escape);

        // A plot of a field that moves, beside its inspector
        keys(&mut h, "ctrl+w q");
        keys(&mut h, "ctrl+w q");
        keys(&mut h, "3");
        {
            let state = h.state().topic_states[&2].clone();
            state.lock().unwrap().trace("linear_acceleration.z");
            state.lock().unwrap().trace("linear_acceleration.x");
            for i in 0..150 {
                let z = 9.8 + (f64::from(i) / 12.0).sin() * 0.4;
                let x = 9.7 + (f64::from(i) / 7.0).cos() * 0.25;
                let field = |n: &str, v| (n.to_owned(), v);
                let accel = archon_msg::Value::Struct(vec![
                    field("x", archon_msg::Value::Float64(x)),
                    field("y", archon_msg::Value::Float64(-0.02)),
                    field("z", archon_msg::Value::Float64(z)),
                ]);
                let mut state = state.lock().unwrap();
                state.update(archon_msg::Value::Struct(vec![field("linear_acceleration", accel)]));
                state.record(&[0, 1, 0, 0, i as u8, 0x40, 0x23, 0x9a, 0x99, 0x99, 0x99, 0x99, 0x99], None);
                drop(state);
                std::thread::sleep(Duration::from_millis(6));
            }
            state.lock().unwrap().record(&[0, 1, 0, 0, 0xff], Some("message ends early, at byte 1 of 8".into()));
        }
        keys(&mut h, "j j j j j");
        keys(&mut h, "p");
        keys(&mut h, "alt+l");
        keys(&mut h, "[");
        keys(&mut h, "[");
        keys(&mut h, "[");
        save(&mut h, "plot");
        // A second field joins the same plot
        keys(&mut h, "alt+h");
        keys(&mut h, "k k");
        keys(&mut h, "p");
        keys(&mut h, "alt+l");
        save(&mut h, "plot-two");

        // The bytes of the last message, which didn't decode
        keys(&mut h, "alt+h");
        keys(&mut h, "v v");
        save(&mut h, "bytes");

        // The publish form
        if std::env::var_os("AMENT_PREFIX_PATH").is_some() {
            h.state_mut().open_publish("/talos/state/kill", "std_msgs/msg/Bool");
            keys(&mut h, "enter");
            save(&mut h, "publish");
        }
        h.state_mut().sup.stop_all();
    }
}
