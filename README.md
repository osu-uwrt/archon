# archon

A keyboard-driven native GUI for ROS 2 firmware testing, so a test session is one window instead of eleven terminals.
It runs the micro-ROS agent and your other long-running commands, keeps your boards' serial consoles open across
reflashes, shows every topic with its rate, and inspects messages as they change, firmware messages included, without
generated code.

## Running it

```sh
nix develop            # or let direnv load the flake
cargo run --release
```

With nothing set up, archon opens on **Getting started**:

1. **Start the micro-ROS agent** (`Space n a`): pick the board's port, a CAN bus, or UDP, and archon runs the agent
   there. It finds the agent on the PATH or in a built workspace like `~/dev/uwrt/micro-ros-agent`; to run it some
   other way, set `micro_ros_agent = "ros2 run micro_ros_agent micro_ros_agent"` in `~/.config/archon/config.toml`.
2. **Open the board's serial console** (`Space n s`), or **dump a CAN bus** (`Space n c`).
   - **No board on the desk? Simulate one** (`Space n f`): pick a titan_firmware build (`*_with_bl.uf2`, newest first)
     and archon runs it in firmware_simulator with the agent on its micro-ROS terminal and its console open, like a
     board on USB. It finds the simulator in `~/dev/*/firmware_simulator` (a release build); set
     `firmware_simulator = "/path/to/firmware_simulator"` in `~/.config/archon/config.toml` otherwise. A simulator
     you started yourself shows up in the agent's port list as `simulated: …`.
3. **Look at the topics** (`T`): Enter on one inspects it, `p` plots a field, `P` publishes to it.
4. **Save the bench** (`Space n w`): writes what you opened into `archon.toml`. From then on, `archon` in that folder
   (or any folder below it) brings it all back, with the layout you left.

`F1` shows that page again. `archon path/to/bench.toml` runs a particular profile, and
[examples/demo.toml](examples/demo.toml) is a bench that needs no hardware.

## Profiles

A profile is one test bench: the processes, ports and topics to bring up together. Save the bench from inside archon
to make one, or write it by hand from [archon.example.toml](archon.example.toml).

- `[[process]]` runs `cmd` with `bash -c` on its own PTY, so programs flush line by line as they would in a terminal.
  Stopping sends Ctrl+C to the whole process group (as a terminal does, so `ros2 launch` shuts its nodes down), then
  SIGTERM after 3 s and SIGKILL after 2 more. `restart = "on-failure"` or `"always"` restarts it, backing off when it
  keeps crashing.
- `[[serial]]` opens a port and reconnects whenever the device comes back, so flashing a board doesn't end the session
  the way it ends `screen`. Use `/dev/serial/by-id/...` paths, which stay the same when the board re-enumerates.
- `[[echo]]` opens a topic echo with the profile (`topic`, and optionally `name` and `type`). It waits for the topic
  to appear on the graph, so the firmware can come up after archon. `/rosout` reads as log lines from every node.
- `[[can]]` dumps a SocketCAN bus (`interface = "can0"`) like `candump`, waiting for the interface and coming back
  when it does. `i` sends a frame in `cansend` syntax: `123#DEADBEEF`, `12345678#00`, `123#R`.
- `[[publish]]` is a message ready to send (`name`, `topic`, `type`, `value` as a table shaped like the message,
  optionally `rate` and `key`). It's in the sidebar and the palette, and its key sends it, or with a `rate` starts and
  stops sending at that rate.
- `[[watch]]` puts a topic's rate in the health strip along the top (`topic`, and optionally `min_hz`): green when it
  keeps up, yellow when it's slower than `min_hz`, red when nothing arrives.
- `env` applies to every process. `RMW_*` and `ROS_*` variables also apply to archon's own node, so it joins the same
  graph. Set `RMW_IMPLEMENTATION` and `ROS_DOMAIN_ID` there.

Every topic on the graph is listed in the sidebar and in `Ctrl+P`; choosing one opens its echo. `--no-ros` leaves ROS
out entirely. [examples/ros-demo.toml](examples/ros-demo.toml) is a ROS bench that needs no hardware.

Closing archon stops everything it started, and remembers each profile's layout for next time (under
`~/.local/state/archon/`). Name a layout to keep it (`Space l s`) and bring it back any time (`Space l l`).
`Ctrl+=` and `Ctrl+-` zoom, remembered too.

## Views

The main area tiles into panes like vim windows, each showing one view:

- **A source's log**: a process, a serial console, or a topic's raw stream.
- **A topic inspector**: the latest message as a tree that updates in place. Changed fields flash and numbers get a
  sparkline. `v` cycles to the raw stream, then to the **bytes**: the last 500 messages as they arrived, the selected
  one in hex beside what it decodes to, or why it doesn't. `p` plots the selected field.
- **A plot**: fields over time, live. `[` and `]` change how much time shows, `f` pauses (then drag and scroll to look
  around), `Ctrl+L` clears.
- **A publish form**: any topic's message as fields to fill in. `Ctrl+S` sends it, `Ctrl+R` sends it at the rate
  given, and Save to profile adds it as a `[[publish]]` action. A message waits (up to 2 s) for the topic's subscribers to find archon's
  publisher before it goes, so the first Send to a board isn't lost to discovery; if nothing subscribes, archon says
  so. `P` opens it for the topic you're on; `Ctrl+O` picks any
  topic on the graph, without echoing it first (so does `Shift+Enter` on a topic in `Ctrl+P`, or Publish in a sidebar
  topic's right-click menu). `Ctrl+L` puts the form back to its defaults; `Delete` removes the publisher.
- **All logs**: every source's lines merged in the order they arrived, each tagged with its source, so the board's
  console, the agent and `/rosout` line up in time.
- **Topics**: every topic with its type, rate, publishers, subscribers and time since the last message. Sortable by
  clicking a heading, filterable with `/`. Rates show for topics archon listens to; `m` measures every topic's.

The health strip along the top shows each process and port, each watched topic's rate, anything being sent on repeat,
and errors you haven't seen. Click a chip to go to it.

Drag a source, topic or view from the sidebar onto a pane to show it there, or onto a pane's edge to open it in a new
pane on that side. Alt+drag a pane to move it the same way, or onto another pane's middle to swap them. Dividers
drag too. `Esc` goes back to what a pane showed before.

`Space b` records every topic to a bag in `bags/` beside the profile, as a source of its own; stopping it (`x`)
closes the bag properly.

## Keys

The bottom bar always shows the keys that matter right now. `?` lists them all, and pressing `Space` waits for the
rest of a sequence with a menu of what can follow.

| Keys | |
|---|---|
| `Ctrl+P` (or `Space Space`) | Go to a source, topic or view by name. `Ctrl+Enter` opens it beside the current one, `Shift+Enter` publishes to the topic |
| `Ctrl+O` (or `Space o p`) | Publish to any topic, or run a profile action; `Ctrl+P` and `Ctrl+O` switch between the two |
| `Ctrl+Shift+P` (or `:`) | Run any command by name, with its keys shown beside it. Typing `>` in `Ctrl+P` does the same |
| `1`–`9`, `Tab`/`Shift+Tab`, `J`/`K` | Focus a source |
| `s` / `x` / `r` | Start / stop / restart it |
| `Space s a` / `Space s q` | Start / stop everything |
| `i` | Type into it (Enter sends, Up/Down for history) |
| `j`/`k`, `Ctrl+D`/`Ctrl+U`, `g g`/`G` | Scroll, page, top / bottom |
| `e` / `E` | Jump to the next / previous error or warning |
| `/` | Filter lines; matches are highlighted (a capital letter makes it case-sensitive) |
| `Esc` | Back out: cancel a key sequence, clear the filter, return to the end, then go back to what the pane showed |
| `f` | Pause or resume following |
| `y` | Copy the output shown (just the filtered lines while filtering) |
| `t` | Timestamps |
| `Ctrl+L` | Clear the output |
| `L` / `T` | Show all logs / the topics table |
| `v` | A topic: inspector, then raw stream, then bytes |
| `p` | Plot the inspector's selected field |
| `P` | Publish to the topic you're on (its echo, or the topics table's row); elsewhere, the same as `Ctrl+O` |
| `Ctrl+S` / `Ctrl+R` | In the publish form: send / send at the rate |
| `j`/`k`, `l`/`h`, `Enter` | In the inspector and the table: move, expand / collapse, open |
| `w` / `m` | Watch the selected topic's rate / measure every topic's rate |
| `Ctrl+W v` / `Ctrl+W s` / `Ctrl+W q` | Split right / split down / close the pane (also `Space w …`) |
| `Ctrl+W h/j/k/l` or `Alt+h/j/k/l` | Move to the pane in that direction; `Ctrl+W w` cycles |
| `Space n a` / `Space n s` / `Space n c` | Start the micro-ROS agent / open a serial console / dump a CAN bus |
| `Space n f` | Simulate a board in firmware_simulator, with its agent and console |
| `Space n i` | Open an isolated window, on a ROS network of its own (see [Several boards at once](#several-boards-at-once)) |
| `Space n w` | Save what's open to `archon.toml` |
| `Delete` / `Space s d` | Remove the source or publisher (also in its right-click menu); if it's in the profile, `y` takes it out of the file too, `n` just for now |
| `F1` | Getting started |
| `Space l s` / `Space l l` | Save the layout under a name / load one |
| `Space b` | Record a bag of every topic |
| `Ctrl+=` / `Ctrl+-` | Zoom |
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

## Several boards at once

Different boards (an IVC and an ESC board, say) can share one window: start an agent for each. Two copies of the
*same* board can't: titan_firmware's micro-ROS clients always ask for DDS domain 0, so both publish the same topics on
the same network (`ROS_DOMAIN_ID` on the agent changes nothing), and every subscriber hears both boards mixed.

So the second copy goes in an **isolated window**: `archon --isolated`, or `Space n i` from a window. It starts a Fast
DDS discovery server of its own (the `discovery` source, on the first free port from 11811), and everything it runs
(its agents, simulators, `ros2 bag`) and its own topic view join that server's network and no other. Simulating a
board that's already simulated in the window (`Space n f`) opens the copy in an isolated window by itself
(`archon --isolated --simulate <image>`). The window's title and the right end of the health strip show the network.

To use `ros2` on an isolated window's network from a terminal, run `Copy this window's ROS environment` from the
palette and paste it there; then use `ros2 ... --no-daemon` (the daemon stays on the network it started on). An
isolated window with no profile given starts empty rather than loading this folder's `archon.toml`.

## Layout

| Crate | |
|---|---|
| `crates/core` | No UI and no ROS: the command set, the `Source` trait, line buffers, ANSI/UTF-8 decoding, profiles |
| `crates/sources` | The PTY process, serial and SocketCAN sources, and the supervisor that runs them and applies restart policy |
| `crates/msg` | ROS message definitions read from `.msg` files at runtime, a CDR decoder and encoder, and building messages from text |
| `crates/ros` | A small rcl client on its own thread: the graph, and raw subscriptions to any installed message type, whose type support is loaded at runtime. Also the topic echo source. |
| `crates/app` | The egui app: tiled layout, log views, inspector, topics table, health strip, keymap, picker, headless UI tests |

Every source runs on its own threads and reports through a channel. The UI drains the channel each frame, so it never
waits on a source. Keys go through the keymap in `raw_input_hook`, before egui sees them, so bound keys never move
widget focus or press a focused button.

The UI tests drive the app headlessly with egui_kittest. To look at the UI without a window, render the main states to
`target/screenshots/`:

```sh
nix develop -c cargo test -p archon screenshots -- --ignored
```

Building needs the ROS environment, which the flake provides. `crates/ros/tests/live.rs` checks the client against a
real graph, publishing with `ros2 topic pub` on domain 77, localhost only.

## Roadmap

1. ~~Process supervisor, serial consoles, palette and keymap~~
2. ~~ROS: topic list, echo and rate, through a small rcl client, so `RMW_IMPLEMENTATION` picks Fast DDS (micro-ROS
   today) or zenoh. Messages are decoded at runtime from `.msg` files, so new firmware messages need no rebuild.~~
3. ~~Topic inspector, merged logs, health strip with rate watches, topics table, tiled layouts saved per profile~~
4. ~~Publishing, with actions saved in the profile and bound to keys; live plots; message bytes; CAN; bag recording;
   drag-and-drop panes; named layouts~~
5. Service calls and parameters, the node graph, bag playback, decoding UWRT's CANmore frames
