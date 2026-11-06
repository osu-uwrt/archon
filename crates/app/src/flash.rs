//! Flashing a board: titan_firmware's upload_tool (which finds the board over USB, reboots it into its bootloader,
//! and knows boards by serial number), or picotool when there's no upload_tool. It runs as a source, so its output
//! shows as it goes and its questions (which board?) are answered with `i`.

use std::path::{Path, PathBuf};

use crate::bench::{Firmware, quote};

#[derive(Clone, Debug, PartialEq)]
pub enum Tool {
    UploadTool(PathBuf),
    Picotool(PathBuf),
}

impl Tool {
    pub fn name(&self) -> &'static str {
        match self {
            Tool::UploadTool(_) => "upload_tool",
            Tool::Picotool(_) => "picotool",
        }
    }
}

/// How much of the flash to write
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// The application (`_ota.uf2`), leaving the board's bootloader as it is: the usual way
    App,
    /// The whole image, bootloader too (`_with_bl.uf2`): a new board, or one without the titan bootloader
    Full,
}

/// upload_tool: the configured one, on the PATH, or built in the titan_firmware checkout; else picotool
pub fn find(configured: Option<&Path>, titan: Option<&Path>) -> Result<Tool, String> {
    if let Some(path) = configured {
        return if path.is_file() {
            Ok(Tool::UploadTool(path.to_owned()))
        } else {
            Err(format!("upload_tool = {} in ~/.config/archon/config.toml isn't there", path.display()))
        };
    }
    let on_path = |name: &str| {
        let paths = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file())
    };
    if let Some(path) = on_path("upload_tool") {
        return Ok(Tool::UploadTool(path));
    }
    if let Some(path) = titan.map(|t| t.join("tools/upload_tool/build/upload_tool")).filter(|p| p.is_file()) {
        return Ok(Tool::UploadTool(path));
    }
    on_path("picotool").map(Tool::Picotool).ok_or_else(|| {
        "nothing to flash with: build titan_firmware's tools/upload_tool, put upload_tool or picotool on the PATH, \
         or set upload_tool = \"...\" in ~/.config/archon/config.toml"
            .to_owned()
    })
}

/// The images a build can be flashed with, the usual one first
pub fn images(firmware: &Firmware) -> Vec<(Mode, PathBuf)> {
    let mut found = Vec::new();
    let name = firmware.image.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if let Some(stem) = name.strip_suffix("_with_bl.uf2") {
        let ota = firmware.image.with_file_name(format!("{stem}_ota.uf2"));
        if ota.is_file() {
            found.push((Mode::App, ota));
        }
    }
    found.push((Mode::Full, firmware.image.clone()));
    found
}

/// The command that flashes `image`
pub fn command(tool: &Tool, image: &Path, mode: Mode) -> String {
    match tool {
        Tool::UploadTool(binary) => {
            let full = if mode == Mode::Full { " -f" } else { "" };
            format!("exec {}{full} {}", quote(binary), quote(image))
        }
        // picotool writes whole images: -f reboots a running board into BOOTSEL first, -x starts it after
        Tool::Picotool(binary) => format!("exec {} load -v -x -f {}", quote(binary), quote(image)),
    }
}

/// What a build is, from the binary info upload_tool reads out of it
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Info {
    pub name: String,
    /// The branch and commit it was built from, like "blake/ivc a3463bd-dirty"
    pub source: String,
    pub board: String,
}

pub fn info(tool: &Tool, image: &Path) -> Option<Info> {
    let Tool::UploadTool(binary) = tool else { return None };
    let out = std::process::Command::new(binary).arg("-i").arg(image).output().ok()?;
    Some(parse_info(&String::from_utf8_lossy(&out.stdout)))
}

/// Reads `upload_tool -i`'s report, colours and all
pub fn parse_info(report: &str) -> Info {
    let mut info = Info::default();
    let mut plain = String::new();
    // Take out the colour escapes
    let mut chars = report.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    for line in plain.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match key.trim() {
            "Name" if info.name.is_empty() => info.name = value.to_owned(),
            "Board Name" if info.board.is_empty() => info.board = value.to_owned(),
            // "1.0-talos/talos/ivc_board blake/ivc/a3463bd-dirty atk@atk 2026-11-03T12:34:22-05:00"
            "Version" if info.source.is_empty() => {
                if let Some(at) = value.split_whitespace().nth(1)
                    && let Some((branch, commit)) = at.rsplit_once('/')
                {
                    info.source = format!("{branch} {commit}");
                }
            }
            _ => {}
        }
    }
    info
}

/// A question upload_tool is waiting on: which board, which interface to catch a booting board on, or whether to
/// go on despite a warning
#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    /// What it asks, in its words
    pub prompt: String,
    pub choices: Vec<Answer>,
}

/// One answer: what to type, and what it is
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub reply: String,
    pub name: String,
    /// The rest it says about it: interface, board type, serial, mode
    pub detail: String,
}

/// The question at the end of upload_tool's output, if it's waiting on one, with the choices listed above it
pub fn question(lines: &[String]) -> Option<Question> {
    let plain: Vec<String> = lines.iter().map(|l| strip_colors(l)).collect();
    let last = plain.iter().rev().find(|l| !l.trim().is_empty())?;
    let asked = last.trim();
    // Yes or no, with no as the tool's own default
    if let Some(prompt) = asked.strip_suffix("[y/N]:") {
        let prompt = prompt.trim().trim_start_matches("[WARNING]").trim().to_owned();
        let answer = |reply: &str, name: &str, detail: &str| Answer {
            reply: reply.to_owned(),
            name: name.to_owned(),
            detail: detail.to_owned(),
        };
        return Some(Question {
            prompt,
            choices: vec![
                answer("n", "No", "stop here; nothing has been written"),
                answer("y", "Yes", "go on and flash"),
            ],
        });
    }
    let prompt = if asked.starts_with("Please Select a Device") {
        "Which board?"
    } else if asked.starts_with("Please Select a Interface") {
        "Which interface?"
    } else {
        return None;
    };
    // The listing since the question before (or since the start)
    let end = plain.len() - 1 - plain.iter().rev().position(|l| l == last).unwrap_or(0);
    let start = plain[..end].iter().rposition(|l| l.contains("Please Select a")).map_or(0, |at| at + 1);
    let mut choices: Vec<Answer> = Vec::new();
    for line in &plain[start..end] {
        let trimmed = line.trim();
        let entry = trimmed.strip_prefix("Dev ").or_else(|| trimmed.strip_prefix("Itf "));
        if let Some(rest) = entry
            && let Some((number, after)) = rest.split_once(':')
            && let Ok(number) = number.trim().parse::<u32>()
        {
            let name = after.trim().strip_prefix("Name:").unwrap_or(after).trim().to_owned();
            choices.push(Answer { reply: number.to_string(), name, detail: String::new() });
        } else if let Some(answer) = choices.last_mut()
            && line.starts_with('\t')
            && let Some((key, value)) = trimmed.split_once(':')
        {
            // A device printed without a number (one that can't be flashed) has its own Name: line; it ends the last
            if key.trim() == "Name" {
                continue;
            }
            if !answer.detail.is_empty() {
                answer.detail.push_str(" · ");
            }
            let value = value.trim();
            answer.detail.push_str(&match key.trim() {
                "Unique ID" => format!("serial {value}"),
                "Board Type" => format!("type {value}"),
                _ => value.to_owned(),
            });
        }
    }
    Some(Question { prompt: prompt.to_owned(), choices })
}

fn strip_colors(text: &str) -> String {
    let mut plain = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_board_question_as_upload_tool_asks_it() {
        // As upload_tool printed it for the RP2040 on the desk
        let lines: Vec<String> = [
            "Waiting for devices...",
            "",
            "\x1b[1;35m=============Other Targets=============\x1b[0m",
            "\x1b[1;36mDev 1:\t\x1b[1;90mName:\t\t\x1b[0;33mUnknown Board\x1b[0m",
            "\x1b[1;90m\tInterface:\t\x1b[0;37mUSB\x1b[0m",
            "\x1b[1;90m\tBoard Type:\t\x1b[0;37munknown\x1b[0m",
            "\x1b[1;90m\tUnique ID:\t\x1b[0;37mE6613852838B7C32\x1b[0m",
            "\x1b[1;90m\tMode:\t\t\x1b[0;37mApplication\x1b[0m",
            "",
            "Please Select a Device [none to abort]: ",
        ]
        .map(str::to_owned)
        .to_vec();
        let asked = question(&lines).unwrap();
        assert_eq!(asked.prompt, "Which board?");
        assert_eq!(
            asked.choices,
            [Answer {
                reply: "1".into(),
                name: "Unknown Board".into(),
                detail: "USB · type unknown · serial E6613852838B7C32 · Application".into()
            }]
        );
        // Not waiting: nothing to answer
        assert_eq!(question(&lines[..8]), None);
    }

    #[test]
    fn reads_the_interface_question_after_an_earlier_one() {
        let lines: Vec<String> = [
            "Dev 1:\tName:\t\tA",
            "Please Select a Device [none to abort]: 1",
            "===========Available Interfaces===========",
            "Itf 1:\tName:\tcan0",
            "Itf 2:\tName:\tvcan0",
            "",
            "Please Select a Interface: ",
        ]
        .map(str::to_owned)
        .to_vec();
        let asked = question(&lines).unwrap();
        assert_eq!(asked.prompt, "Which interface?");
        let names: Vec<&str> = asked.choices.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["can0", "vcan0"], "only what's listed since the last question");
    }

    #[test]
    fn reads_a_yes_or_no_with_no_first() {
        let lines = vec![
            "Please Select a Device [none to abort]: 1".to_owned(),
            "[WARNING] The selected board does not match the expected type 'pico'. Continue? [y/N]: ".to_owned(),
        ];
        let asked = question(&lines).unwrap();
        assert_eq!(asked.prompt, "The selected board does not match the expected type 'pico'. Continue?");
        let replies: Vec<&str> = asked.choices.iter().map(|c| c.reply.as_str()).collect();
        assert_eq!(replies, ["n", "y"]);
    }

    #[test]
    fn reads_upload_tools_report() {
        let report = "\x1b[1;35m==========Application Image==========\x1b[0m\n\
                      \x1b[1;90mName:\t\t\x1b[0;33mIVCBoard2\x1b[0m\n\
                      \x1b[1;90mVersion:\t\x1b[0;33m1.0-talos/talos/ivc_board blake/ivc/a3463bd-dirty atk@atk 2026-11-03T12:34:22-05:00\x1b[0m\n\
                      \x1b[1;90mBoard Name:\t\x1b[0;33mtalos/ivc_board\x1b[0m\n";
        assert_eq!(
            parse_info(report),
            Info {
                name: "IVCBoard2".into(),
                source: "blake/ivc a3463bd-dirty".into(),
                board: "talos/ivc_board".into()
            }
        );
    }

    #[test]
    fn offers_the_app_image_first_and_flashes_as_asked() {
        let dir = std::env::temp_dir().join(format!("archon-flash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let full = dir.join("esc_board_with_bl.uf2");
        let ota = dir.join("esc_board_ota.uf2");
        std::fs::write(&full, b"").unwrap();
        std::fs::write(&ota, b"").unwrap();
        let firmware = Firmware { image: full.clone(), name: "esc_board".into(), built: None };
        assert_eq!(images(&firmware), [(Mode::App, ota.clone()), (Mode::Full, full.clone())]);

        let tool = Tool::UploadTool("/t/upload_tool".into());
        assert_eq!(command(&tool, &ota, Mode::App), format!("exec /t/upload_tool {}", ota.display()));
        assert_eq!(command(&tool, &full, Mode::Full), format!("exec /t/upload_tool -f {}", full.display()));
        let picotool = Tool::Picotool("/t/picotool".into());
        assert_eq!(command(&picotool, &full, Mode::Full), format!("exec /t/picotool load -v -x -f {}", full.display()));
    }

    #[test]
    fn finds_the_checkouts_upload_tool() {
        let dir = std::env::temp_dir().join(format!("archon-flash-find-{}", std::process::id()));
        let tool = dir.join("tools/upload_tool/build/upload_tool");
        std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
        std::fs::write(&tool, b"").unwrap();
        // Unless one's on the PATH, the checkout's is used
        if !std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("upload_tool").is_file()))
        {
            assert_eq!(find(None, Some(&dir)), Ok(Tool::UploadTool(tool)));
        }
        assert!(find(Some(Path::new("/no/such/upload_tool")), None).is_err());
    }
}
