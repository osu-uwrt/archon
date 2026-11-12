//! Triggers: a profile's `[[trigger]]`s watch every source's output for a pattern (a watchdog reset, a hard fault),
//! and when one shows up, archon says so in the health strip, and can stop following everywhere so the moment stays
//! on screen, or export the session.

use std::collections::{HashMap, HashSet};

use archon_core::line::Origin;
use archon_core::profile::TriggerSpec;
use archon_core::source::SourceId;
use archon_sources::Supervisor;
use regex::Regex;

pub struct Trigger {
    pub spec: TriggerSpec,
    regex: Regex,
    pub hits: u32,
    /// The source and line it last fired on
    pub last: Option<(SourceId, String)>,
    /// Each source's next line to look at
    scanned: HashMap<SourceId, u64>,
    /// Lines already fired on, since the last one of a source is looked at again until it's finished
    fired: HashSet<(SourceId, u64)>,
}

/// A trigger going off
pub struct Hit {
    pub trigger: usize,
    pub source: SourceId,
    pub text: String,
}

impl Trigger {
    pub fn new(spec: TriggerSpec) -> Result<Trigger, String> {
        let regex = Regex::new(&spec.matches).map_err(|e| format!("trigger {:?} isn't a regex: {e}", spec.matches))?;
        Ok(Trigger { spec, regex, hits: 0, last: None, scanned: HashMap::new(), fired: HashSet::new() })
    }

    pub fn label(&self) -> String {
        self.spec.name.clone().unwrap_or_else(|| self.spec.matches.clone())
    }
}

/// Looks through what's arrived since the last scan
pub fn scan(triggers: &mut [Trigger], sup: &Supervisor) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (index, trigger) in triggers.iter_mut().enumerate() {
        for (id, entry) in sup.live() {
            if trigger.spec.source.as_deref().is_some_and(|s| !entry.name.contains(s)) {
                continue;
            }
            let total = entry.buffer.total();
            let from = trigger.scanned.get(&id).copied().unwrap_or(0).max(entry.buffer.evicted());
            for row in from..total {
                let Some(line) = entry.buffer.get_abs(row) else { continue };
                // What the source said, not archon's notes about it (which quote its command)
                if line.origin != Origin::Source {
                    continue;
                }
                if trigger.regex.is_match(&line.text) && trigger.fired.insert((id, row)) {
                    trigger.hits += 1;
                    trigger.last = Some((id, line.text.clone()));
                    hits.push(Hit { trigger: index, source: id, text: line.text.trim().to_owned() });
                }
            }
            // The last line may still be arriving, so it's looked at again next time
            trigger.scanned.insert(id, total.saturating_sub(1));
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn fires_once_per_matching_line_even_while_it_arrives() {
        let profile = archon_core::profile::Profile::parse(
            "[[process]]\nname = \"board\"\ncmd = \"printf 'ok\\\\nwatch'; sleep 0.2; printf 'dog reset\\\\nok\\\\nwatchdog reset\\\\n'; sleep 5\"\nautostart = true\n",
        )
        .unwrap();
        let mut sup = Supervisor::new(&profile, Arc::new(|| {}));
        sup.start_autostart();
        let spec =
            TriggerSpec { matches: "watchdog reset".into(), name: Some("watchdog".into()), ..Default::default() };
        let mut triggers = vec![Trigger::new(spec).unwrap()];
        let mut fired = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fired.len() < 2 && Instant::now() < deadline {
            sup.poll();
            fired.extend(scan(&mut triggers, &sup).into_iter().map(|h| h.text));
            std::thread::sleep(Duration::from_millis(20));
        }
        for _ in 0..10 {
            sup.poll();
            fired.extend(scan(&mut triggers, &sup).into_iter().map(|h| h.text));
        }
        assert_eq!(fired, ["watchdog reset", "watchdog reset"], "the split line once, the whole one once");
        assert_eq!(triggers[0].hits, 2);
        assert!(Trigger::new(TriggerSpec { matches: "(".into(), ..Default::default() }).is_err());
        sup.stop_all();
    }
}
