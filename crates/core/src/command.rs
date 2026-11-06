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

    ToggleSplit,
    OtherPane,
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
    c(ToggleSplit, "pane.split", "Toggle split view"),
    c(OtherPane, "pane.other", "Focus other pane"),
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
