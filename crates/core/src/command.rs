//! Every action in archon is a [`Command`]. Keybinds, the command palette and the mouse all dispatch the same
//! commands, and the keymap file names them by id.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Command {
    Quit,
    /// Pick a source (and later a topic) by name
    QuickOpen,
    OpenPalette,
    ShowKeys,
    ToggleSidebar,

    NextSource,
    PrevSource,
    /// Focus the source with this 1-based number in the sidebar
    FocusSource(u8),
    Start,
    Stop,
    Restart,
    /// Take the source off the bench (and out of the profile, if asked)
    Remove,
    StartAll,
    StopAll,
    FocusInput,

    ScrollDown,
    ScrollUp,
    PageDown,
    PageUp,
    Top,
    /// Jump to the end and follow new output
    Bottom,
    ToggleFollow,
    Search,
    ClearSearch,
    ClearView,
    ToggleTimestamps,
    /// Copy the lines shown (all of them, or those matching the filter)
    CopyOutput,
    NextError,
    PrevError,

    /// Split the active pane, the new one to the right
    ToggleSplit,
    SplitDown,
    ClosePane,
    OtherPane,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,

    OpenAllLogs,
    OpenTopics,
    OpenNodes,
    /// A topic: switch between the inspector and the raw stream
    ToggleStream,
    /// Open what's selected: an inspector branch, a topic in the table
    Activate,
    ActivateBeside,
    Expand,
    Collapse,
    MeasureAll,
    ToggleWatch,

    /// Open the publish form for the topic at hand, or pick one
    /// Publish to the topic at hand: its echo, or the topics table's row
    OpenPublish,
    /// Pick any topic on the graph to publish to, without echoing it first
    PublishAny,
    /// In the publish form: send the message
    Send,
    /// In the publish form: keep sending at its rate
    ToggleRepeat,
    /// Run the profile's publish action with this 1-based number
    RunAction(u8),

    SaveLayout,
    LoadLayout,

    /// Plot the inspector's field at the cursor
    Plot,
    PlotWider,
    PlotNarrower,

    /// Record every topic to a bag, as a source of its own
    RecordBag,
    /// Play a recorded bag back onto the graph
    PlayBag,

    /// Run the micro-ROS agent on a port picked from a list
    StartAgent,
    /// Run a firmware build in firmware_simulator, with the agent and a console on it
    Simulate,
    /// Write a firmware_simulator robot file from the builds
    NewRobot,
    /// Flash a firmware build onto a board
    Flash,
    OpenSerial,
    OpenCan,
    /// Write what's been opened into the profile, making one if there isn't one
    SaveBench,
    ShowWelcome,
    /// Open another archon on a ROS network of its own, for a second board whose topics would mix with this one's
    OpenIsolated,
    /// Copy what a terminal needs to join this window's ROS network
    CopyRosEnv,
}

pub struct CommandInfo {
    pub command: Command,
    pub id: &'static str,
    pub title: &'static str,
    /// Whether the palette lists it. Scrolling one line from the palette is pointless.
    pub in_palette: bool,
}

const fn c(command: Command, id: &'static str, title: &'static str) -> CommandInfo {
    CommandInfo { command, id, title, in_palette: true }
}

const fn hidden(command: Command, id: &'static str, title: &'static str) -> CommandInfo {
    CommandInfo { command, id, title, in_palette: false }
}

use Command::*;

pub const COMMANDS: &[CommandInfo] = &[
    c(Quit, "app.quit", "Quit"),
    c(QuickOpen, "picker.open", "Go to source"),
    c(OpenPalette, "palette.open", "Show all commands"),
    c(ShowKeys, "help.keys", "Show keybindings"),
    c(ToggleSidebar, "sidebar.toggle", "Toggle sidebar"),
    hidden(NextSource, "source.next", "Next source"),
    hidden(PrevSource, "source.prev", "Previous source"),
    hidden(FocusSource(1), "source.focus.1", "Focus source 1"),
    hidden(FocusSource(2), "source.focus.2", "Focus source 2"),
    hidden(FocusSource(3), "source.focus.3", "Focus source 3"),
    hidden(FocusSource(4), "source.focus.4", "Focus source 4"),
    hidden(FocusSource(5), "source.focus.5", "Focus source 5"),
    hidden(FocusSource(6), "source.focus.6", "Focus source 6"),
    hidden(FocusSource(7), "source.focus.7", "Focus source 7"),
    hidden(FocusSource(8), "source.focus.8", "Focus source 8"),
    hidden(FocusSource(9), "source.focus.9", "Focus source 9"),
    c(Start, "source.start", "Start source"),
    c(Stop, "source.stop", "Stop source"),
    c(Restart, "source.restart", "Restart source"),
    c(Remove, "source.remove", "Remove source"),
    c(StartAll, "source.start_all", "Start all sources"),
    c(StopAll, "source.stop_all", "Stop all sources"),
    c(FocusInput, "source.input", "Send input to source"),
    hidden(ScrollDown, "view.down", "Scroll down"),
    hidden(ScrollUp, "view.up", "Scroll up"),
    hidden(PageDown, "view.page_down", "Page down"),
    hidden(PageUp, "view.page_up", "Page up"),
    c(Top, "view.top", "Jump to top"),
    c(Bottom, "view.bottom", "Jump to bottom and follow"),
    c(ToggleFollow, "view.follow", "Toggle follow"),
    c(Search, "view.search", "Filter lines"),
    c(ClearSearch, "view.search_clear", "Clear filter"),
    c(ClearView, "view.clear", "Clear output"),
    c(ToggleTimestamps, "view.timestamps", "Toggle timestamps"),
    c(CopyOutput, "view.copy", "Copy output"),
    c(NextError, "view.next_error", "Next error or warning"),
    c(PrevError, "view.prev_error", "Previous error or warning"),
    c(ToggleSplit, "pane.split", "Split right"),
    c(SplitDown, "pane.split_down", "Split down"),
    c(ClosePane, "pane.close", "Close pane"),
    c(OtherPane, "pane.other", "Next pane"),
    hidden(FocusLeft, "pane.left", "Pane to the left"),
    hidden(FocusRight, "pane.right", "Pane to the right"),
    hidden(FocusUp, "pane.up", "Pane above"),
    hidden(FocusDown, "pane.down", "Pane below"),
    c(OpenAllLogs, "open.logs", "Show all logs merged"),
    c(OpenTopics, "open.topics", "Show topics table"),
    c(OpenNodes, "open.nodes", "Show the nodes on the graph"),
    c(ToggleStream, "view.mode", "Toggle topic inspector and stream"),
    hidden(Activate, "view.activate", "Open selected"),
    hidden(ActivateBeside, "view.activate_beside", "Open selected beside"),
    hidden(Expand, "view.expand", "Expand"),
    hidden(Collapse, "view.collapse", "Collapse"),
    c(MeasureAll, "topics.measure", "Measure every topic's rate"),
    c(ToggleWatch, "topics.watch", "Watch this topic's rate"),
    c(OpenPublish, "topics.publish", "Publish to this topic"),
    c(PublishAny, "topics.publish_any", "Publish to any topic"),
    c(Send, "publish.send", "Send the message"),
    c(ToggleRepeat, "publish.repeat", "Start or stop sending at the rate"),
    c(SaveLayout, "layout.save", "Save layout as"),
    c(LoadLayout, "layout.load", "Load a saved layout"),
    c(Plot, "view.plot", "Plot the selected field"),
    c(RecordBag, "bag.record", "Record a bag of every topic"),
    c(PlayBag, "bag.play", "Play a bag back"),
    c(StartAgent, "bench.agent", "Start the micro-ROS agent"),
    c(Simulate, "bench.simulate", "Simulate a board or robot in firmware_simulator"),
    c(NewRobot, "bench.new_robot", "Build a robot file (several simulated boards)"),
    c(Flash, "bench.flash", "Flash a board"),
    c(OpenSerial, "bench.serial", "Open a serial console"),
    c(OpenCan, "bench.can", "Dump a CAN bus"),
    c(SaveBench, "bench.save", "Save the bench to archon.toml"),
    c(ShowWelcome, "help.start", "Getting started"),
    c(OpenIsolated, "bench.isolated", "Open an isolated window (a ROS network of its own)"),
    c(CopyRosEnv, "bench.copy_env", "Copy this window's ROS environment for a terminal"),
    hidden(PlotWider, "plot.wider", "Plot a longer time"),
    hidden(PlotNarrower, "plot.narrower", "Plot a shorter time"),
    hidden(RunAction(1), "action.1", "Run action 1"),
    hidden(RunAction(2), "action.2", "Run action 2"),
    hidden(RunAction(3), "action.3", "Run action 3"),
    hidden(RunAction(4), "action.4", "Run action 4"),
    hidden(RunAction(5), "action.5", "Run action 5"),
    hidden(RunAction(6), "action.6", "Run action 6"),
    hidden(RunAction(7), "action.7", "Run action 7"),
    hidden(RunAction(8), "action.8", "Run action 8"),
    hidden(RunAction(9), "action.9", "Run action 9"),
];

impl Command {
    pub fn from_id(id: &str) -> Option<Command> {
        COMMANDS.iter().find(|info| info.id == id).map(|info| info.command)
    }

    pub fn info(self) -> &'static CommandInfo {
        COMMANDS.iter().find(|info| info.command == self).expect("every command is in COMMANDS")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_round_trip() {
        for info in COMMANDS {
            assert_eq!(Command::from_id(info.id), Some(info.command), "{}", info.id);
        }
    }
}
