//! A session written to a folder, for a bug report or a later look: every source's log, the merged log, each echoed
//! topic's recent messages, the graph, the boards, and the bench file, with a README saying when and why.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use archon_core::source::SourceId;
use archon_msg::{Registry, TypeName, cdr};
use archon_ros::{Graph, TopicState};
use archon_sources::Supervisor;

use crate::boards::Board;
use crate::logview::{clock, file_stamp};

/// What goes into an export
pub struct Session<'a> {
    pub sup: &'a Supervisor,
    pub topics: &'a HashMap<SourceId, Arc<Mutex<TopicState>>>,
    pub registry: &'a Arc<Mutex<Registry>>,
    pub graph: &'a Graph,
    pub boards: &'a [Board],
    pub profile: Option<&'a Path>,
}

/// Writes the session into a new folder in `dir`; returns the folder
pub fn export(dir: &Path, why: &str, session: &Session<'_>) -> std::io::Result<PathBuf> {
    let now = SystemTime::now();
    let mut folder = dir.join(format!("archon-{}", file_stamp(now)));
    // Two exports in a second (a trigger going off twice) get folders of their own
    let mut n = 2;
    while folder.exists() {
        folder = dir.join(format!("archon-{}-{n}", file_stamp(now)));
        n += 1;
    }
    std::fs::create_dir_all(folder.join("topics"))?;

    let mut readme = format!("archon session exported at {}\nwhy: {why}\n\nsources:\n", clock(now));
    for (id, entry) in session.sup.live() {
        let status = crate::logview::status_text(&entry.status);
        let _ = writeln!(readme, "  {}  {status}  {} lines", entry.name, entry.buffer.total());
        if session.topics.contains_key(&id) {
            continue;
        }
        let mut log = String::new();
        for line in entry.buffer.iter() {
            let _ = writeln!(log, "{}  {}", clock(line.time), line.text);
        }
        std::fs::write(folder.join(format!("{}.log", file_name(&entry.name))), log)?;
    }

    // Every source's lines in the order they arrived
    let mut merged = String::new();
    let timeline = session.sup.timeline();
    let entries = session.sup.entries();
    for row in timeline.first()..timeline.end() {
        let Some((id, line)) = timeline.get(row) else { continue };
        let Some(entry) = entries.get(id).filter(|e| !e.removed && e.in_merged_logs()) else { continue };
        if let Some(line) = entry.buffer.get_abs(line) {
            let _ = writeln!(merged, "{}  {:<16} {}", clock(line.time), entry.name, line.text);
        }
    }
    std::fs::write(folder.join("merged.log"), merged)?;

    // Each echoed topic's recent messages, decoded where they decode
    let now_instant = Instant::now();
    for (id, state) in session.topics {
        let Some(entry) = entries.get(*id).filter(|e| !e.removed) else { continue };
        let state = state.lock().unwrap();
        let ty = state.ty.clone().and_then(|t| TypeName::parse(&t));
        let mut text = format!("# {} ({})\n", entry.name, state.ty.as_deref().unwrap_or("type not known"));
        let mut registry = session.registry.lock().unwrap();
        for message in &state.raw {
            let at = now - now_instant.saturating_duration_since(message.at);
            let decoded = match (&message.error, &ty) {
                (Some(why), _) => format!("doesn't decode: {why}"),
                (None, Some(ty)) => match registry.load(ty).and_then(|_| cdr::decode(&registry, ty, &message.bytes)) {
                    Ok(value) => value.to_text(),
                    Err(e) => format!("doesn't decode: {e:#}"),
                },
                (None, None) => "type not known".to_owned(),
            };
            let _ = writeln!(text, "--- #{} at {}\n{}", message.number, clock(at), decoded.trim_end());
        }
        std::fs::write(folder.join("topics").join(format!("{}.txt", file_name(&entry.name))), text)?;
    }

    let mut graph = String::from("topics:\n");
    for topic in &session.graph.topics {
        let _ = writeln!(
            graph,
            "  {}  {}  {} pub, {} sub",
            topic.name,
            topic.types.join(", "),
            topic.publishers,
            topic.subscribers
        );
    }
    graph.push_str("\nnodes:\n");
    for node in &session.graph.nodes {
        let names = |list: &[(String, String)]| list.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ");
        let _ = writeln!(
            graph,
            "  {}\n    publishes: {}\n    subscribes: {}",
            node.name,
            names(&node.publishes),
            names(&node.subscribes)
        );
    }
    std::fs::write(folder.join("graph.txt"), graph)?;

    if !session.boards.is_empty() {
        let mut boards = String::new();
        for b in session.boards {
            let _ = writeln!(
                boards,
                "{}  {}  up {}  faults {:#x}  kill asserting {:#x}  reboots {}  version {}",
                b.name,
                b.state(),
                crate::boards::uptime(b.uptime_ms),
                b.faults,
                b.kill_asserting,
                b.reboots,
                b.version
            );
        }
        std::fs::write(folder.join("boards.txt"), boards)?;
    }
    if let Some(profile) = session.profile.filter(|p| p.is_file()) {
        std::fs::copy(profile, folder.join("bench.toml"))?;
    }
    std::fs::write(folder.join("README.txt"), readme)?;
    Ok(folder)
}

/// A source or topic name as a file name: `/talos/state/kill` is `talos_state_kill`
fn file_name(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
    cleaned.trim_matches('_').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_each_log_and_the_readme() {
        let profile = archon_core::profile::Profile::parse(
            "[[process]]\nname = \"agent\"\ncmd = \"echo session established\"\nautostart = true\n[[process]]\nname = \"board\"\ncmd = \"echo 'Clean Boot'\"\nautostart = true\n",
        )
        .unwrap();
        let mut sup = Supervisor::new(&profile, Arc::new(|| {}));
        sup.start_autostart();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while sup.entries().iter().any(|e| e.buffer.total() < 2) && Instant::now() < deadline {
            sup.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let dir = std::env::temp_dir().join(format!("archon-export-{}", std::process::id()));
        let topics = HashMap::new();
        let registry = Arc::new(Mutex::new(Registry::default()));
        let graph = Graph::default();
        let session =
            Session { sup: &sup, topics: &topics, registry: &registry, graph: &graph, boards: &[], profile: None };
        let folder = export(&dir, "a test", &session).unwrap();
        let board = std::fs::read_to_string(folder.join("board.log")).unwrap();
        assert!(board.contains("Clean Boot"), "{board}");
        let merged = std::fs::read_to_string(folder.join("merged.log")).unwrap();
        assert!(merged.contains("agent") && merged.contains("board"), "{merged}");
        assert!(std::fs::read_to_string(folder.join("README.txt")).unwrap().contains("why: a test"));
        // A second export in the same second gets its own folder
        assert_ne!(export(&dir, "again", &session).unwrap(), folder);
        assert_eq!(file_name("/talos/state/kill"), "talos_state_kill");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
