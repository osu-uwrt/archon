//! Key sequences to commands. A binding is one or more chords, such as `ctrl+p` or `space s r`. The user's
//! `~/.config/archon/keymap.toml` adds to and overrides the defaults.

use std::path::Path;

use anyhow::{Context, anyhow, bail};
use archon_core::command::Command;
use egui::{Key, Modifiers};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Chord {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Keys whose shifted form is a different key (`?` is shift+/), so shift isn't part of their chord
fn shift_is_in_key(key: Key) -> bool {
    let symbol = key.symbol_or_name();
    symbol.len() == 1 && symbol.chars().all(|c| c.is_ascii_punctuation())
}

/// The key's symbol when it's plain ASCII (`/`, `?`), else its name (`Down`, not an arrow glyph the fonts lack)
fn display_name(key: Key) -> &'static str {
    let symbol = key.symbol_or_name();
    if symbol.is_ascii() { symbol } else { key.name() }
}

impl Chord {
    pub fn new(key: Key, modifiers: Modifiers) -> Self {
        Self {
            key,
            ctrl: modifiers.ctrl || modifiers.mac_cmd,
            alt: modifiers.alt,
            shift: modifiers.shift && !shift_is_in_key(key),
        }
    }

    /// Parses `ctrl+shift+p`, `G` (shift+g), `?`, `space`, `pagedown` and the like
    pub fn parse(text: &str) -> anyhow::Result<Chord> {
        let mut parts: Vec<&str> = text.split('+').collect();
        // `+` itself, and chords ending in it like `ctrl++`
        if text.ends_with("++") || text == "+" {
            parts.retain(|p| !p.is_empty());
            parts.push("+");
        }
        let (key_name, modifiers) = parts.split_last().ok_or_else(|| anyhow!("empty chord"))?;
        let mut chord = Chord { key: Key::Space, ctrl: false, alt: false, shift: false };
        for modifier in modifiers {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" => chord.ctrl = true,
                "alt" | "option" => chord.alt = true,
                "shift" => chord.shift = true,
                other => bail!("unknown modifier {other:?} in {text:?}"),
            }
        }

        let single = key_name.chars().next().filter(|_| key_name.chars().count() == 1);
        chord.key = match single {
            Some(c) if c.is_ascii_alphabetic() => {
                chord.shift |= c.is_ascii_uppercase();
                Key::from_name(&c.to_ascii_uppercase().to_string())
            }
            Some(_) => Key::from_name(key_name),
            None => named_key(key_name),
        }
        .ok_or_else(|| anyhow!("unknown key {key_name:?} in {text:?}"))?;
        if shift_is_in_key(chord.key) {
            chord.shift = false;
        }
        Ok(chord)
    }
}

fn named_key(name: &str) -> Option<Key> {
    let lower = name.to_ascii_lowercase();
    let key = match lower.as_str() {
        "space" => Key::Space,
        "tab" => Key::Tab,
        "enter" | "return" => Key::Enter,
        "esc" | "escape" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "home" => Key::Home,
        "end" => Key::End,
        _ => return Key::from_name(name).or_else(|| Key::from_name(&lower.to_ascii_uppercase())),
    };
    Some(key)
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        let name = display_name(self.key);
        let is_letter = name.len() == 1 && name.chars().all(|c| c.is_ascii_alphabetic());
        let modified = self.ctrl || self.alt;
        match (is_letter && !modified, self.shift) {
            // A bare letter shows as typed, g or G; with Ctrl or Alt it shows the usual way, Ctrl+P
            (true, false) => f.write_str(&name.to_ascii_lowercase()),
            (true, true) => f.write_str(name),
            (false, true) => write!(f, "Shift+{name}"),
            (false, false) => f.write_str(name),
        }
    }
}

pub type Sequence = Vec<Chord>;

pub fn parse_sequence(text: &str) -> anyhow::Result<Sequence> {
    let sequence = text.split_whitespace().map(Chord::parse).collect::<anyhow::Result<Sequence>>()?;
    if sequence.is_empty() {
        bail!("empty key sequence");
    }
    Ok(sequence)
}

pub fn format_sequence(sequence: &[Chord]) -> String {
    sequence.iter().map(Chord::to_string).collect::<Vec<_>>().join(" ")
}

const DEFAULTS: &[(&str, &str)] = &[
    ("ctrl+p", "picker.open"),
    ("space space", "picker.open"),
    ("ctrl+shift+p", "palette.open"),
    (":", "palette.open"),
    ("?", "help.keys"),
    ("ctrl+q", "app.quit"),
    ("ctrl+b", "sidebar.toggle"),
    // Sources
    ("tab", "source.next"),
    ("shift+tab", "source.prev"),
    ("J", "source.next"),
    ("K", "source.prev"),
    ("1", "source.focus.1"),
    ("2", "source.focus.2"),
    ("3", "source.focus.3"),
    ("4", "source.focus.4"),
    ("5", "source.focus.5"),
    ("6", "source.focus.6"),
    ("7", "source.focus.7"),
    ("8", "source.focus.8"),
    ("9", "source.focus.9"),
    ("s", "source.start"),
    ("x", "source.stop"),
    ("r", "source.restart"),
    ("i", "source.input"),
    ("space s s", "source.start"),
    ("space s x", "source.stop"),
    ("space s r", "source.restart"),
    ("space s d", "source.remove"),
    ("delete", "source.remove"),
    ("space s a", "source.start_all"),
    ("space s q", "source.stop_all"),
    ("space s i", "source.input"),
    // The view
    ("j", "view.down"),
    ("down", "view.down"),
    ("k", "view.up"),
    ("up", "view.up"),
    ("ctrl+d", "view.page_down"),
    ("pagedown", "view.page_down"),
    ("ctrl+u", "view.page_up"),
    ("pageup", "view.page_up"),
    ("g g", "view.top"),
    ("home", "view.top"),
    ("G", "view.bottom"),
    ("end", "view.bottom"),
    ("f", "view.follow"),
    ("/", "view.search"),
    ("ctrl+l", "view.clear"),
    ("t", "view.timestamps"),
    ("y", "view.copy"),
    ("ctrl+shift+c", "view.copy"),
    ("e", "view.next_error"),
    ("E", "view.prev_error"),
    ("space v f", "view.follow"),
    ("space v c", "view.clear"),
    ("space v t", "view.timestamps"),
    ("space v /", "view.search"),
    ("space v n", "view.search_clear"),
    ("space v y", "view.copy"),
    ("v", "view.mode"),
    ("enter", "view.activate"),
    ("ctrl+enter", "view.activate_beside"),
    ("l", "view.expand"),
    ("right", "view.expand"),
    ("h", "view.collapse"),
    ("left", "view.collapse"),
    // Views
    ("L", "open.logs"),
    ("T", "open.topics"),
    ("space o l", "open.logs"),
    ("space o t", "open.topics"),
    ("N", "open.nodes"),
    ("B", "open.boards"),
    ("space o b", "open.boards"),
    ("space o n", "open.nodes"),
    ("m", "topics.measure"),
    ("w", "topics.watch"),
    ("P", "topics.publish"),
    ("ctrl+o", "topics.publish_any"),
    ("space o p", "topics.publish_any"),
    ("ctrl+s", "publish.send"),
    ("ctrl+r", "publish.repeat"),
    ("p", "view.plot"),
    ("]", "plot.wider"),
    ("[", "plot.narrower"),
    ("space b", "bag.record"),
    ("space B", "bag.play"),
    ("space t", "test.run"),
    ("space e", "session.export"),
    ("space c", "settings.paths"),
    ("space n a", "bench.agent"),
    ("space n s", "bench.serial"),
    ("space n f", "bench.simulate"),
    ("space n r", "bench.new_robot"),
    ("space n u", "bench.flash"),
    ("space n i", "bench.isolated"),
    ("space n c", "bench.can"),
    ("space n w", "bench.save"),
    ("F1", "help.start"),
    ("space l s", "layout.save"),
    ("space l l", "layout.load"),
    // Panes, as vim windows
    ("ctrl+w v", "pane.split"),
    ("ctrl+w s", "pane.split_down"),
    ("ctrl+w q", "pane.close"),
    ("ctrl+w c", "pane.close"),
    ("ctrl+w w", "pane.other"),
    ("ctrl+w h", "pane.left"),
    ("ctrl+w j", "pane.down"),
    ("ctrl+w k", "pane.up"),
    ("ctrl+w l", "pane.right"),
    ("alt+h", "pane.left"),
    ("alt+j", "pane.down"),
    ("alt+k", "pane.up"),
    ("alt+l", "pane.right"),
    ("space w v", "pane.split"),
    ("space w s", "pane.split_down"),
    ("space w q", "pane.close"),
    ("space w w", "pane.other"),
    ("space w h", "pane.left"),
    ("space w j", "pane.down"),
    ("space w k", "pane.up"),
    ("space w l", "pane.right"),
];

/// Names for prefixes, shown in the which-key popup
const GROUPS: &[(&str, &str)] = &[
    ("space", "leader"),
    ("space s", "source"),
    ("space v", "view"),
    ("space w", "pane"),
    ("space o", "open"),
    ("space l", "layout"),
    ("space n", "new"),
    ("ctrl+w", "pane"),
    ("g", "go"),
];

pub struct Keymap {
    bindings: Vec<(Sequence, Command)>,
    groups: Vec<(Sequence, &'static str)>,
}

/// What can follow a pending prefix
pub enum Next {
    Command(Command),
    Group(&'static str),
}

impl Keymap {
    pub fn defaults() -> Keymap {
        let bindings = DEFAULTS
            .iter()
            .map(|(keys, id)| {
                let command = Command::from_id(id).unwrap_or_else(|| panic!("default binding to unknown {id}"));
                (parse_sequence(keys).unwrap_or_else(|e| panic!("default binding {keys}: {e}")), command)
            })
            .collect();
        let groups = GROUPS.iter().map(|(keys, name)| (parse_sequence(keys).unwrap(), *name)).collect();
        Keymap { bindings, groups }
    }

    /// Applies a keymap file over these bindings: a table of `"keys" = "command.id"`, where `""` unbinds the keys
    pub fn apply_file(&mut self, path: &Path) -> anyhow::Result<()> {
        let text = std::fs::read_to_string(path)?;
        self.apply(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn apply(&mut self, text: &str) -> anyhow::Result<()> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct File {
            #[serde(default)]
            keys: std::collections::BTreeMap<String, String>,
        }
        let file: File = toml::from_str(text)?;
        for (keys, id) in file.keys {
            let sequence = parse_sequence(&keys)?;
            self.bindings.retain(|(bound, _)| *bound != sequence);
            if !id.is_empty() {
                let command = Command::from_id(&id).ok_or_else(|| anyhow!("unknown command {id:?} for {keys:?}"))?;
                self.bindings.push((sequence, command));
            }
        }
        Ok(())
    }

    /// Binds keys to a command in addition to what's bound already, replacing what those keys did
    pub fn bind(&mut self, sequence: Sequence, command: Command) {
        self.bindings.retain(|(bound, _)| *bound != sequence);
        self.bindings.push((sequence, command));
    }

    pub fn lookup(&self, sequence: &[Chord]) -> Option<Command> {
        self.bindings.iter().find(|(bound, _)| bound == sequence).map(|(_, command)| *command)
    }

    pub fn is_prefix(&self, sequence: &[Chord]) -> bool {
        self.bindings.iter().any(|(bound, _)| bound.len() > sequence.len() && bound.starts_with(sequence))
    }

    /// The keys bound to a command, for showing next to it
    pub fn keys_for(&self, command: Command) -> Vec<&Sequence> {
        self.bindings.iter().filter(|(_, c)| *c == command).map(|(keys, _)| keys).collect()
    }

    /// Each distinct next chord after `prefix`, and what it leads to
    pub fn continuations(&self, prefix: &[Chord]) -> Vec<(Chord, Next)> {
        let mut out: Vec<(Chord, Next)> = Vec::new();
        for (bound, command) in &self.bindings {
            if bound.len() <= prefix.len() || !bound.starts_with(prefix) {
                continue;
            }
            let chord = bound[prefix.len()];
            if out.iter().any(|(c, _)| *c == chord) {
                continue;
            }
            let next = if bound.len() == prefix.len() + 1 {
                Next::Command(*command)
            } else {
                let group = &bound[..=prefix.len()];
                Next::Group(self.groups.iter().find(|(g, _)| g == group).map_or("more", |(_, name)| *name))
            };
            out.push((chord, next));
        }
        out
    }
}

/// Tracks a key sequence in progress
#[derive(Default)]
pub struct Matcher {
    pending: Sequence,
}

pub enum Outcome {
    Run(Command),
    /// The keys so far start a longer binding
    Pending,
    Unbound,
}

impl Matcher {
    pub fn press(&mut self, chord: Chord, keymap: &Keymap) -> Outcome {
        self.pending.push(chord);
        if let Some(command) = keymap.lookup(&self.pending) {
            self.pending.clear();
            return Outcome::Run(command);
        }
        if keymap.is_prefix(&self.pending) {
            return Outcome::Pending;
        }
        // A wrong key in the middle of a sequence abandons it, then counts on its own
        let abandoned = self.pending.len() > 1;
        self.pending.clear();
        if abandoned { self.press(chord, keymap) } else { Outcome::Unbound }
    }

    pub fn pending(&self) -> &[Chord] {
        &self.pending
    }

    pub fn cancel(&mut self) {
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> Chord {
        Chord::parse(text).unwrap()
    }

    #[test]
    fn parses_chords() {
        assert_eq!(chord("ctrl+p"), Chord { key: Key::P, ctrl: true, alt: false, shift: false });
        assert_eq!(chord("G"), Chord { key: Key::G, ctrl: false, alt: false, shift: true });
        assert_eq!(chord("shift+g"), chord("G"));
        assert_eq!(chord("?").key, Key::Questionmark);
        assert_eq!(chord("pagedown").key, Key::PageDown);
        assert!(Chord::parse("hyper+x").is_err());
    }

    #[test]
    fn shifted_symbols_match_their_event() {
        // Typing `?` arrives as the `?` key with shift held
        assert_eq!(Chord::new(Key::Questionmark, Modifiers::SHIFT), chord("?"));
        assert_eq!(Chord::new(Key::G, Modifiers::SHIFT), chord("G"));
        assert_eq!(Chord::new(Key::Tab, Modifiers::SHIFT), chord("shift+tab"));
        assert!(Chord::new(Key::ArrowDown, Modifiers::SHIFT).shift, "arrows keep shift");
    }

    #[test]
    fn displays_chords() {
        assert_eq!(chord("ctrl+p").to_string(), "Ctrl+P");
        assert_eq!(chord("G").to_string(), "G");
        assert_eq!(chord("g").to_string(), "g");
        assert_eq!(chord("down").to_string(), "Down");
        assert_eq!(chord("?").to_string(), "?");
        assert_eq!(format_sequence(&parse_sequence("space s r").unwrap()), "Space s r");
    }

    #[test]
    fn defaults_parse() {
        let keymap = Keymap::defaults();
        assert_eq!(keymap.lookup(&parse_sequence("space s r").unwrap()), Some(Command::Restart));
    }

    #[test]
    fn matches_sequences() {
        let keymap = Keymap::defaults();
        let mut m = Matcher::default();
        assert!(matches!(m.press(chord("g"), &keymap), Outcome::Pending));
        assert!(matches!(m.press(chord("g"), &keymap), Outcome::Run(Command::Top)));
        assert!(matches!(m.press(chord("space"), &keymap), Outcome::Pending));
        assert!(matches!(m.press(chord("s"), &keymap), Outcome::Pending));
        assert!(matches!(m.press(chord("r"), &keymap), Outcome::Run(Command::Restart)));
    }

    #[test]
    fn wrong_key_mid_sequence_counts_alone() {
        let keymap = Keymap::defaults();
        let mut m = Matcher::default();
        m.press(chord("g"), &keymap);
        assert!(matches!(m.press(chord("j"), &keymap), Outcome::Run(Command::ScrollDown)));
        assert!(m.pending().is_empty());
    }

    #[test]
    fn user_file_overrides_and_unbinds() {
        let mut keymap = Keymap::defaults();
        keymap.apply("[keys]\n\"x\" = \"\"\n\"ctrl+r\" = \"source.restart\"\n").unwrap();
        assert_eq!(keymap.lookup(&[chord("x")]), None);
        assert_eq!(keymap.lookup(&[chord("ctrl+r")]), Some(Command::Restart));
        assert!(keymap.apply("[keys]\n\"x\" = \"no.such\"\n").is_err());
    }

    #[test]
    fn continuations_name_groups() {
        let keymap = Keymap::defaults();
        let next = keymap.continuations(&[chord("space")]);
        assert!(next.iter().any(|(c, n)| *c == chord("s") && matches!(n, Next::Group("source"))));
        assert!(next.iter().any(|(c, n)| *c == chord("space") && matches!(n, Next::Command(Command::QuickOpen))));
    }
}
