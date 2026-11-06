# archon

A keyboard-driven native GUI for ROS 2 firmware testing, so a test session is one window instead of eleven terminals.
It runs the micro-ROS agent and your other long-running commands, keeps your boards' serial consoles open across
reflashes, and (next) echoes, lists and publishes ROS topics.

## Running it

```sh
nix develop            # or let direnv load the flake
cargo run --release -- examples/demo.toml
```

The demo profile needs no hardware and no ROS. Without an argument, archon uses the nearest `archon.toml` in the
current directory or above.

## Profiles

A profile is one test bench: the processes and serial ports to bring up together. Copy
[archon.example.toml](archon.example.toml) into the project you're testing as `archon.toml`.

- `[[process]]` runs `cmd` with `bash -c` on its own PTY, so programs flush line by line as they would in a terminal.
  Stopping sends Ctrl+C to the whole process group (as a terminal does, so `ros2 launch` shuts its nodes down), then
  SIGTERM after 3 s and SIGKILL after 2 more. `restart = "on-failure"` or `"always"` restarts it, backing off when it
  keeps crashing.
- `[[serial]]` opens a port and reconnects whenever the device comes back, so flashing a board doesn't end the session
  the way it ends `screen`. Use `/dev/serial/by-id/...` paths, which stay the same when the board re-enumerates.
- `env` applies to every process. Set `RMW_IMPLEMENTATION` there.

Closing archon stops everything it started.

## Keys

The bottom bar always shows the keys that matter right now. `?` lists them all, and pressing `Space` waits for the
rest of a sequence with a menu of what can follow.

| Keys | |
|---|---|
| `Ctrl+P` (or `Space Space`) | Go to a source by name. `Ctrl+Enter` opens it beside the current one |
| `Ctrl+Shift+P` (or `:`) | Run any command by name, with its keys shown beside it. Typing `>` in `Ctrl+P` does the same |
| `1`–`9`, `Tab`/`Shift+Tab`, `J`/`K` | Focus a source |
| `s` / `x` / `r` | Start / stop / restart it |
| `Space s a` / `Space s q` | Start / stop everything |
| `i` | Type into it (Enter sends, Up/Down for history) |
| `j`/`k`, `Ctrl+D`/`Ctrl+U`, `g g`/`G` | Scroll, page, top / bottom |
| `e` / `E` | Jump to the next / previous error or warning |
| `/` | Filter lines; matches are highlighted (a capital letter makes it case-sensitive) |
| `Esc` | Back out: cancel a key sequence, clear the filter, return to the end |
| `f` | Pause or resume following |
| `y` | Copy the output shown (just the filtered lines while filtering) |
| `t` | Timestamps |
| `Ctrl+L` | Clear the output |
| `Space w v` / `Space w w` | Split / switch pane |
| `Ctrl+B` | Toggle the sidebar |
| `Ctrl+Q` | Quit |

`Ctrl+P`, `Ctrl+Shift+P`, `Ctrl+B` and `Ctrl+Q` also work while typing in the input or filter bar.

In the sidebar, a name in bold has output you haven't seen, and a red or yellow count is new errors or warnings.
Ctrl+click opens a source beside the current one; right-click has the rest.

Rebind in `~/.config/archon/keymap.toml`, where `""` unbinds:

```toml
[keys]
"ctrl+r" = "source.restart"
"x" = ""
```

## Layout

| Crate | |
|---|---|
| `crates/core` | No UI and no ROS: the command set, the `Source` trait, line buffers, ANSI/UTF-8 decoding, profiles |
| `crates/sources` | The PTY process and serial sources, and the supervisor that runs them and applies restart policy |
| `crates/app` | The egui app: keymap, picker, log views, headless UI tests |

Every source runs on its own threads and reports through a channel. The UI drains the channel each frame, so it never
waits on a source. Keys go through the keymap in `raw_input_hook`, before egui sees them, so bound keys never move
widget focus or press a focused button.

The UI tests drive the app headlessly with egui_kittest. To look at the UI without a window, render the main states to
`target/screenshots/`:

```sh
nix develop -c cargo test -p archon screenshots -- --ignored
```

## Roadmap

1. ~~Process supervisor, serial consoles, palette and keymap~~
2. ROS: topic list, echo and hz, through an rcl backend (r2r) behind a `RosBackend` trait, so `RMW_IMPLEMENTATION`
   picks Fast DDS (micro-ROS today) or zenoh. Messages are decoded at runtime from `.msg` files, so new firmware
   messages need no rebuild.
3. Publish and service-call actions saved in the profile and bound to keys
4. Live plots
5. Parameters, the node graph, bags; CAN as a source
