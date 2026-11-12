//! Bench tests: a file of steps (hear a topic, publish, expect a value, see a line on a console, see no errors for a
//! while) run against the bench. The same runner works in the window, where it shows step by step, and headless
//! (`archon test FILE`), where it prints and exits 1 on a failure, for CI with firmware_simulator.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use archon_core::line::Origin;
use archon_msg::{Registry, TypeName, Value, cdr};
use archon_ros::{Delivery, Ros, SubscriptionId};
use archon_sources::Supervisor;
use regex::Regex;
use serde::Deserialize;

/// How long a step waits when neither it nor the file says
const WITHIN: Duration = Duration::from_secs(10);

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct TestFile {
    pub name: Option<String>,
    /// A profile to bring up first, for a headless run (relative to the test file)
    pub bench: Option<PathBuf>,
    /// A firmware build to simulate first: a name like `IVCBoard2`, or a `.uf2`
    pub simulate: Option<String>,
    /// A firmware_simulator robot file to run first
    pub robot: Option<PathBuf>,
    /// How long each step may take, unless it says (`30s`, `500ms`)
    pub within: Option<String>,
    #[serde(rename = "step", default)]
    pub steps: Vec<Step>,
    #[serde(skip)]
    pub path: PathBuf,
}

/// One step. It does one thing (the first key below), with the rest as its details.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Hear a message on this topic
    pub wait: Option<String>,
    /// Send `value` to this topic
    pub publish: Option<String>,
    /// Hear a message on this topic that meets the condition
    pub expect: Option<String>,
    /// See a line matching this (a regex) in a source's output
    pub log: Option<String>,
    /// See no line matching this for `for`
    pub no_log: Option<String>,
    /// Wait this long
    pub sleep: Option<String>,

    #[serde(rename = "type")]
    pub ty: Option<String>,
    pub value: Option<toml::Table>,
    /// The field `expect` checks, like `kill_switches_asserting_kill` or `linear_acceleration.z`
    pub field: Option<String>,
    pub equals: Option<toml::Value>,
    pub above: Option<f64>,
    pub below: Option<f64>,
    /// A regex the field's (or the message's) text matches
    pub matches: Option<String>,
    /// The sources `log` and `no_log` look at, by name (part of it is enough); all of them if left out
    pub source: Option<String>,
    pub within: Option<String>,
    #[serde(rename = "for")]
    pub for_: Option<String>,
}

impl TestFile {
    pub fn load(path: &Path) -> Result<TestFile, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut test: TestFile = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        test.path = path.to_owned();
        if test.steps.is_empty() {
            return Err(format!("{}: no [[step]]s", path.display()));
        }
        for (i, step) in test.steps.iter().enumerate() {
            step.action().map_err(|why| format!("{}: step {}: {why}", path.display(), i + 1))?;
        }
        if let Some(within) = &test.within {
            parse_duration(within).map_err(|why| format!("{}: within: {why}", path.display()))?;
        }
        Ok(test)
    }

    pub fn title(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "test".to_owned())
        })
    }

    /// A path in the file, which is relative to the file
    pub fn resolve(&self, path: &Path) -> PathBuf {
        self.path.parent().unwrap_or(Path::new(".")).join(path)
    }

    fn within(&self) -> Duration {
        self.within.as_deref().and_then(|w| parse_duration(w).ok()).unwrap_or(WITHIN)
    }
}

/// What a step does, checked once when the file loads
#[derive(Clone, Debug)]
enum Action {
    Wait(String),
    Publish(String),
    Expect(String),
    Log(Regex),
    NoLog(Regex),
    Sleep(Duration),
}

impl Step {
    fn action(&self) -> Result<Action, String> {
        let given = [&self.wait, &self.publish, &self.expect, &self.log, &self.no_log, &self.sleep];
        if given.iter().filter(|g| g.is_some()).count() != 1 {
            return Err("give one of wait, publish, expect, log, no_log, sleep".to_owned());
        }
        let regex = |text: &str| Regex::new(text).map_err(|e| format!("{text:?} isn't a regex: {e}"));
        for duration in [&self.within, &self.for_].into_iter().flatten() {
            parse_duration(duration)?;
        }
        if let Some(matches) = &self.matches {
            regex(matches)?;
        }
        Ok(if let Some(topic) = &self.wait {
            Action::Wait(topic.clone())
        } else if let Some(topic) = &self.publish {
            if self.value.is_none() {
                return Err("publish needs a value, like value = { data = true }".to_owned());
            }
            Action::Publish(topic.clone())
        } else if let Some(topic) = &self.expect {
            Action::Expect(topic.clone())
        } else if let Some(pattern) = &self.log {
            Action::Log(regex(pattern)?)
        } else if let Some(pattern) = &self.no_log {
            if self.for_.is_none() {
                return Err("no_log needs for, like for = \"3s\"".to_owned());
            }
            Action::NoLog(regex(pattern)?)
        } else {
            Action::Sleep(parse_duration(self.sleep.as_deref().unwrap_or_default())?)
        })
    }

    /// The step in words, like "hear /talos/heartbeat"
    pub fn describe(&self) -> String {
        let condition = || {
            let field = self.field.as_deref().map(|f| format!(" {f}")).unwrap_or_default();
            let mut parts = Vec::new();
            if let Some(value) = &self.equals {
                parts.push(format!("= {value}"));
            }
            if let Some(n) = self.above {
                parts.push(format!("> {n}"));
            }
            if let Some(n) = self.below {
                parts.push(format!("< {n}"));
            }
            if let Some(m) = &self.matches {
                parts.push(format!("matching {m:?}"));
            }
            if parts.is_empty() { field } else { format!(" with{field} {}", parts.join(", ")) }
        };
        let from = || self.source.as_deref().map(|s| format!(" on {s}")).unwrap_or_default();
        match self.action() {
            Ok(Action::Wait(topic)) => format!("hear {topic}"),
            Ok(Action::Publish(topic)) => {
                let value = self.value.as_ref().map(|v| v.to_string().replace('\n', ", ")).unwrap_or_default();
                format!("publish {} to {topic}", value.trim_end_matches(", "))
            }
            Ok(Action::Expect(topic)) => format!("hear {topic}{}", condition()),
            Ok(Action::Log(r)) => format!("see {:?}{}", r.as_str(), from()),
            Ok(Action::NoLog(r)) => {
                format!("no {:?}{} for {}", r.as_str(), from(), self.for_.as_deref().unwrap_or_default())
            }
            Ok(Action::Sleep(d)) => format!("wait {}", seconds(d)),
            Err(why) => why,
        }
    }
}

/// `500ms`, `2s`, `1.5s`, `2m`, or a bare number of seconds
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let (number, unit) = match text.find(|c: char| c.is_ascii_alphabetic()) {
        Some(at) => text.split_at(at),
        None => (text, "s"),
    };
    let n: f64 = number.trim().parse().map_err(|_| format!("{text:?} isn't a time, like 500ms or 2s"))?;
    let seconds = match unit {
        "ms" => n / 1000.0,
        "s" => n,
        "m" | "min" => n * 60.0,
        _ => return Err(format!("{text:?} isn't a time, like 500ms or 2s")),
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(format!("{text:?} isn't a time"));
    }
    Ok(Duration::from_secs_f64(seconds))
}

fn seconds(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 1.0 { format!("{:.0} ms", s * 1000.0) } else { format!("{s:.1} s") }
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Waiting,
    Running,
    Passed(Duration),
    Failed(String),
    /// Not run, since an earlier step failed
    Skipped,
}

/// What the runner needs of the bench
pub struct Bench<'a> {
    pub sup: &'a Supervisor,
    pub ros: Option<&'a Arc<Ros>>,
    pub registry: &'a Arc<Mutex<Registry>>,
}

/// What a subscription has heard, with when
type Heard = Arc<Mutex<Vec<(Instant, Vec<u8>)>>>;

/// A topic the runner listens to, and what it has heard since
struct Listening {
    id: SubscriptionId,
    ty: String,
    heard: Heard,
}

pub struct Runner {
    pub test: TestFile,
    pub states: Vec<State>,
    current: usize,
    step_started: Instant,
    step_started_at: SystemTime,
    started: Instant,
    pub finished: Option<Duration>,
    listening: HashMap<String, Listening>,
    /// What the step that's running is waiting on, for the view
    pub note: String,
}

impl Runner {
    pub fn new(test: TestFile) -> Self {
        let states = vec![State::Waiting; test.steps.len()];
        Self {
            test,
            states,
            current: 0,
            step_started: Instant::now(),
            step_started_at: SystemTime::now(),
            started: Instant::now(),
            finished: None,
            listening: HashMap::new(),
            note: String::new(),
        }
    }

    pub fn passed(&self) -> Option<bool> {
        self.finished.map(|_| self.states.iter().all(|s| matches!(s, State::Passed(_))))
    }

    pub fn elapsed(&self) -> Duration {
        self.finished.unwrap_or_else(|| self.started.elapsed())
    }

    /// Moves the test along; returns the steps that finished, for printing them as they do
    pub fn poll(&mut self, bench: &Bench<'_>) -> Vec<usize> {
        let mut done = Vec::new();
        while self.finished.is_none() {
            let i = self.current;
            if self.states[i] == State::Waiting {
                self.states[i] = State::Running;
                self.step_started = Instant::now();
                self.step_started_at = SystemTime::now();
                self.note.clear();
            }
            match self.step(i, bench) {
                Some(Ok(())) => self.states[i] = State::Passed(self.step_started.elapsed()),
                Some(Err(why)) => self.states[i] = State::Failed(why),
                None => break,
            }
            done.push(i);
            let failed = matches!(self.states[i], State::Failed(_));
            if failed || i + 1 == self.states.len() {
                for state in &mut self.states[i + 1..] {
                    *state = State::Skipped;
                }
                self.finished = Some(self.started.elapsed());
                self.stop(bench);
            } else {
                self.current += 1;
            }
        }
        done
    }

    /// Stops listening (the test is over, or called off)
    pub fn stop(&mut self, bench: &Bench<'_>) {
        if let Some(ros) = bench.ros {
            for (_, listening) in self.listening.drain() {
                ros.unsubscribe(listening.id);
            }
        }
        if self.finished.is_none() {
            let i = self.current;
            self.states[i] = State::Failed("stopped".to_owned());
            for state in &mut self.states[i + 1..] {
                *state = State::Skipped;
            }
            self.finished = Some(self.started.elapsed());
        }
    }

    /// One step: `None` while it's still going
    fn step(&mut self, i: usize, bench: &Bench<'_>) -> Option<Result<(), String>> {
        let step = self.test.steps[i].clone();
        let action = step.action().expect("checked on load");
        let within = step.within.as_deref().and_then(|w| parse_duration(w).ok()).unwrap_or(self.test.within());
        let out_of_time = self.step_started.elapsed() >= within;
        match action {
            Action::Sleep(d) => (self.step_started.elapsed() >= d).then_some(Ok(())),
            Action::Publish(topic) => Some(self.publish(&topic, &step, bench)),
            Action::Wait(topic) | Action::Expect(topic) => {
                let Some(ros) = bench.ros else { return Some(Err("ROS isn't running".to_owned())) };
                if !self.listening.contains_key(&topic) {
                    let ty =
                        step.ty.clone().or_else(|| ros.graph().topic(&topic).and_then(|t| t.types.first().cloned()));
                    match ty {
                        Some(ty) => self.listen(ros, &topic, &ty),
                        None if out_of_time => return Some(Err(format!("{topic} never appeared"))),
                        None => {
                            self.note = format!("waiting for {topic} to appear");
                            return None;
                        }
                    }
                }
                let listening = &self.listening[&topic];
                let heard: Vec<(Instant, Vec<u8>)> = listening
                    .heard
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(at, _)| *at >= self.step_started)
                    .cloned()
                    .collect();
                let mut last = None;
                for (_, bytes) in &heard {
                    let value = decode(bench.registry, &listening.ty, bytes);
                    match check(&step, &value) {
                        Ok(()) => return Some(Ok(())),
                        Err(why) => last = Some(why),
                    }
                }
                self.note = match (&last, heard.len()) {
                    (_, 0) => format!("nothing on {topic} yet"),
                    (Some(why), n) => format!("{n} heard; the last: {why}"),
                    (None, n) => format!("{n} heard"),
                };
                out_of_time.then(|| {
                    Err(match last {
                        None => format!("heard nothing on {topic} in {}", seconds(within)),
                        Some(why) => format!("no message on {topic} in {} met it; the last: {why}", seconds(within)),
                    })
                })
            }
            Action::Log(pattern) => {
                if let Some(line) = self.find_line(&pattern, step.source.as_deref(), bench) {
                    return Some(Ok(())).inspect(|_| self.note = line);
                }
                self.note = format!("looking for {:?}", pattern.as_str());
                out_of_time.then(|| Err(format!("no line matched {:?} in {}", pattern.as_str(), seconds(within))))
            }
            Action::NoLog(pattern) => {
                if let Some(line) = self.find_line(&pattern, step.source.as_deref(), bench) {
                    return Some(Err(format!("saw {line}")));
                }
                let length = parse_duration(step.for_.as_deref().unwrap_or_default()).unwrap_or_default();
                (self.step_started.elapsed() >= length).then_some(Ok(()))
            }
        }
    }

    fn listen(&mut self, ros: &Ros, topic: &str, ty: &str) {
        let heard: Heard = Arc::default();
        let into = heard.clone();
        let id = ros.subscribe(
            topic,
            ty,
            Box::new(move |delivery| {
                if let Delivery::Message(bytes) = delivery {
                    let mut heard = into.lock().unwrap();
                    heard.push((Instant::now(), bytes.to_vec()));
                    // A fast topic over a long test: keep the recent ones
                    if heard.len() > 4096 {
                        heard.drain(..2048);
                    }
                }
            }),
        );
        self.listening.insert(topic.to_owned(), Listening { id, ty: ty.to_owned(), heard });
    }

    fn publish(&mut self, topic: &str, step: &Step, bench: &Bench<'_>) -> Result<(), String> {
        let ros = bench.ros.ok_or("ROS isn't running")?;
        let ty = step
            .ty
            .clone()
            .or_else(|| ros.graph().topic(topic).and_then(|t| t.types.first().cloned()))
            .ok_or_else(|| format!("give publishing to {topic} a type: it isn't on the graph to look up"))?;
        let mut registry = bench.registry.lock().unwrap();
        let mut publisher = crate::publish::Publisher::new(&mut registry, topic, topic, &ty);
        publisher.apply_table(&registry, step.value.as_ref().expect("checked on load"))?;
        let bytes = publisher.encode(&registry)?;
        ros.publish(topic, &ty, bytes);
        Ok(())
    }

    /// The first line since the step started that matches, from the sources asked for
    fn find_line(&self, pattern: &Regex, source: Option<&str>, bench: &Bench<'_>) -> Option<String> {
        for (_, entry) in bench.sup.live() {
            if source.is_some_and(|s| !entry.name.contains(s)) {
                continue;
            }
            // What the source said, not archon's notes about it (which quote its command)
            let found = entry
                .buffer
                .iter()
                .find(|l| l.origin == Origin::Source && l.time >= self.step_started_at && pattern.is_match(&l.text));
            if let Some(line) = found {
                return Some(format!("{}: {}", entry.name, line.text.trim()));
            }
        }
        None
    }
}

fn decode(registry: &Arc<Mutex<Registry>>, ty: &str, bytes: &[u8]) -> Result<Value, String> {
    let name = TypeName::parse(ty).ok_or_else(|| format!("{ty} isn't a message type"))?;
    let mut registry = registry.lock().unwrap();
    registry.load(&name).map_err(|e| format!("{e:#}"))?;
    cdr::decode(&registry, &name, bytes).map_err(|e| format!("{e:#}"))
}

/// Whether a message meets an expect step's condition, and if not, why
fn check(step: &Step, message: &Result<Value, String>) -> Result<(), String> {
    let message = message.as_ref().map_err(|why| format!("doesn't decode: {why}"))?;
    let value = match &step.field {
        Some(field) => message.get(field).ok_or_else(|| format!("has no field {field}"))?,
        None => message,
    };
    let text = value.to_text();
    let number = value.as_f64();
    let shown = || text.trim().replace('\n', ", ");
    if let Some(wanted) = &step.equals {
        let same = match (wanted, number) {
            (toml::Value::Integer(n), Some(x)) => (*n as f64 - x).abs() < 1e-9,
            (toml::Value::Float(n), Some(x)) => (n - x).abs() < 1e-9,
            (toml::Value::Boolean(b), Some(x)) => (f64::from(u8::from(*b)) - x).abs() < 1e-9,
            (toml::Value::String(s), _) => text.trim() == s,
            _ => false,
        };
        if !same {
            return Err(format!("{} was {}", step.field.as_deref().unwrap_or("it"), shown()));
        }
    }
    for (bound, above) in [(step.above, true), (step.below, false)] {
        let Some(bound) = bound else { continue };
        let x =
            number.ok_or_else(|| format!("{} isn't a number: {}", step.field.as_deref().unwrap_or("it"), shown()))?;
        if (above && x <= bound) || (!above && x >= bound) {
            return Err(format!("{} was {x}", step.field.as_deref().unwrap_or("it")));
        }
    }
    if let Some(pattern) = &step.matches {
        let pattern = Regex::new(pattern).expect("checked on load");
        if !pattern.is_match(&text) {
            return Err(format!("{} was {}", step.field.as_deref().unwrap_or("it"), shown()));
        }
    }
    Ok(())
}

pub enum ViewAction {
    RunAgain,
    Stop,
}

/// The test as it runs: each step, what it's waiting on, and how it went
pub fn show(ui: &mut egui::Ui, runner: &Runner, keys: [&str; 2]) -> Option<ViewAction> {
    use egui::{RichText, Sense, vec2};
    let visuals = ui.visuals().clone();
    let mut action = None;
    let (status, color) = match runner.passed() {
        None => ("RUNNING", visuals.selection.bg_fill),
        Some(true) => ("PASSED", egui::Color32::from_rgb(0x19, 0x9e, 0x70)),
        Some(false) => ("FAILED", visuals.error_fg_color),
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(runner.test.title()).strong());
        ui.label(
            RichText::new(format!(" {status} ")).small().strong().color(visuals.panel_fill).background_color(color),
        );
        ui.label(RichText::new(seconds(runner.elapsed())).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let [again, stop] = keys;
            if runner.finished.is_some() {
                if ui.button(format!("Run again ({again})")).clicked() {
                    action = Some(ViewAction::RunAgain);
                }
            } else if ui.button(format!("Stop ({stop})")).clicked() {
                action = Some(ViewAction::Stop);
            }
        });
    });
    ui.label(RichText::new(runner.test.path.display().to_string()).small().weak());
    ui.separator();
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for (i, (step, state)) in runner.test.steps.iter().zip(&runner.states).enumerate() {
            ui.horizontal_wrapped(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                let center = rect.center();
                let green = egui::Color32::from_rgb(0x19, 0x9e, 0x70);
                match state {
                    State::Passed(_) => _ = ui.painter().circle_filled(center, 5.0, green),
                    State::Failed(_) => _ = ui.painter().circle_filled(center, 5.0, visuals.error_fg_color),
                    State::Running => {
                        ui.painter().circle_stroke(center, 5.0, egui::Stroke::new(2.0, visuals.selection.bg_fill));
                    }
                    State::Waiting | State::Skipped => {
                        ui.painter().circle_stroke(center, 4.0, egui::Stroke::new(1.0, visuals.weak_text_color()));
                    }
                }
                ui.label(RichText::new(format!("{:>2}", i + 1)).monospace().weak());
                let text = RichText::new(step.describe());
                ui.label(if *state == State::Skipped { text.weak() } else { text });
                match state {
                    State::Passed(took) => _ = ui.label(RichText::new(seconds(*took)).small().weak()),
                    State::Running if !runner.note.is_empty() => {
                        ui.label(RichText::new(&runner.note).small().weak());
                    }
                    _ => {}
                }
            });
            if let State::Failed(why) = state {
                ui.indent(("why", i), |ui| {
                    ui.label(RichText::new(why).color(visuals.error_fg_color));
                });
            }
        }
    });
    action
}

/// The build a test simulates: a name like `IVCBoard2` (the newest build of it), or a path to a `.uf2`
pub fn firmware(
    build: &str,
    test: &TestFile,
    config: &crate::bench::Config,
    home: &Path,
) -> Result<crate::bench::Firmware, String> {
    if build.ends_with(".uf2") {
        let image = test.resolve(Path::new(build));
        return crate::bench::Firmware::from_image(image.clone())
            .filter(|f| f.image.is_file())
            .ok_or_else(|| format!("{} isn't there", image.display()));
    }
    config
        .firmware_builds(home)
        .into_iter()
        .find(|f| f.name.eq_ignore_ascii_case(build))
        .ok_or_else(|| format!("no build of {build} (looked for {build}_with_bl.uf2 under titan_firmware)"))
}

/// Test files near a bench: `tests/*.toml` beside it and in the folder archon runs in, newest first
pub fn find(dirs: &[PathBuf]) -> Vec<TestFile> {
    let mut found: Vec<(Option<SystemTime>, TestFile)> = Vec::new();
    for dir in dirs {
        for entry in std::fs::read_dir(dir.join("tests")).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "toml")
                && let Ok(test) = TestFile::load(&path)
                && !found.iter().any(|(_, t)| t.path == test.path)
            {
                found.push((entry.metadata().and_then(|m| m.modified()).ok(), test));
            }
        }
    }
    found.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    found.into_iter().map(|(_, t)| t).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> Result<TestFile, String> {
        let path = std::env::temp_dir().join(format!("archon-benchtest-{}-{}.toml", std::process::id(), text.len()));
        std::fs::write(&path, text).unwrap();
        let test = TestFile::load(&path);
        let _ = std::fs::remove_file(&path);
        test
    }

    #[test]
    fn reads_steps_and_says_what_they_do() {
        let test = load(
            r#"
            name = "kill switch"
            within = "5s"
            [[step]]
            wait = "/talos/heartbeat"
            within = "30s"
            [[step]]
            publish = "/talos/state/kill"
            type = "std_msgs/msg/Bool"
            value = { data = true }
            [[step]]
            expect = "/talos/state/firmware"
            field = "kill_switches_asserting_kill"
            above = 0
            [[step]]
            log = "Kill switch (asserted|on)"
            source = "console"
            [[step]]
            no_log = "ERROR"
            for = "2s"
            [[step]]
            sleep = "500ms"
            "#,
        )
        .unwrap();
        let said: Vec<String> = test.steps.iter().map(Step::describe).collect();
        assert_eq!(
            said,
            [
                "hear /talos/heartbeat",
                "publish data = true to /talos/state/kill",
                "hear /talos/state/firmware with kill_switches_asserting_kill > 0",
                "see \"Kill switch (asserted|on)\" on console",
                "no \"ERROR\" for 2s",
                "wait 500 ms",
            ]
        );
        assert_eq!(test.within(), Duration::from_secs(5));
    }

    #[test]
    fn says_what_is_wrong_with_a_step() {
        let err = load("[[step]]\nwait = \"/a\"\nsleep = \"1s\"\n").unwrap_err();
        assert!(err.contains("step 1: give one of"), "{err}");
        let err = load("[[step]]\npublish = \"/a\"\n").unwrap_err();
        assert!(err.contains("needs a value"), "{err}");
        let err = load("[[step]]\nno_log = \"(\"\nfor = \"1s\"\n").unwrap_err();
        assert!(err.contains("isn't a regex"), "{err}");
        let err = load("[[step]]\nsleep = \"soon\"\n").unwrap_err();
        assert!(err.contains("isn't a time"), "{err}");
        assert!(load("name = \"x\"\n").unwrap_err().contains("no [[step]]s"));
    }

    #[test]
    fn checks_values() {
        let message = Value::Struct(vec![
            ("count".into(), Value::UInt(3)),
            ("ok".into(), Value::Bool(true)),
            ("name".into(), Value::String("esc_board".into())),
        ]);
        let step = |text: &str| -> Step { toml::from_str(text).unwrap() };
        assert!(check(&step("field = \"count\"\nabove = 2"), &Ok(message.clone())).is_ok());
        assert_eq!(check(&step("field = \"count\"\nbelow = 3"), &Ok(message.clone())).unwrap_err(), "count was 3");
        assert!(check(&step("field = \"ok\"\nequals = true"), &Ok(message.clone())).is_ok());
        assert!(check(&step("field = \"name\"\nequals = \"esc_board\""), &Ok(message.clone())).is_ok());
        assert!(check(&step("field = \"name\"\nmatches = \"^esc\""), &Ok(message.clone())).is_ok());
        assert_eq!(check(&step("field = \"nope\"\nabove = 1"), &Ok(message)).unwrap_err(), "has no field nope");
        assert!(parse_duration("1.5s").unwrap() == Duration::from_millis(1500));
        assert!(parse_duration("2m").unwrap() == Duration::from_secs(120));
    }

    #[test]
    fn log_steps_pass_fail_and_skip_the_rest() {
        let profile = archon_core::profile::Profile::parse(
            "[[process]]\nname = \"board\"\ncmd = \"echo booting; sleep 0.3; echo 'ERROR: brownout'; sleep 5\"\nautostart = true\n",
        )
        .unwrap();
        let mut sup = Supervisor::new(&profile, Arc::new(|| {}));
        let registry = Arc::new(Mutex::new(Registry::default()));
        let test = load(
            r#"
            [[step]]
            log = "booting"
            source = "board"
            within = "3s"
            [[step]]
            no_log = "ERROR"
            for = "2s"
            [[step]]
            sleep = "1s"
            "#,
        )
        .unwrap();
        let mut runner = Runner::new(test);
        // The first step starts before the board does, so its line counts
        runner.poll(&Bench { sup: &sup, ros: None, registry: &registry });
        sup.start_autostart();
        let deadline = Instant::now() + Duration::from_secs(10);
        while runner.finished.is_none() {
            assert!(Instant::now() < deadline, "{:?}", runner.states);
            sup.poll();
            runner.poll(&Bench { sup: &sup, ros: None, registry: &registry });
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(matches!(runner.states[0], State::Passed(_)), "{:?}", runner.states);
        assert_eq!(runner.states[1], State::Failed("saw board: ERROR: brownout".to_owned()));
        assert_eq!(runner.states[2], State::Skipped);
        assert_eq!(runner.passed(), Some(false));
        sup.stop_all();
    }
}
