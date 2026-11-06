//! Owns every source in a profile, keeps their output, and restarts them according to their restart policy.

use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use archon_core::line::{Level, LineBuffer};
use archon_core::profile::{Profile, RestartPolicy};
use archon_core::source::{Envelope, Event, Sink, Source, SourceId, SourceKind, Status};

use crate::process::ProcessSource;
use crate::serial::SerialSource;

/// Lines kept per source
const BUFFER_LINES: usize = 200_000;

/// A run shorter than this counts as a crash loop, and each one in a row doubles the restart delay
const QUICK_EXIT: Duration = Duration::from_secs(5);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(30);

pub struct Entry {
    pub name: String,
    pub kind: SourceKind,
    pub description: String,
    pub status: Status,
    pub buffer: LineBuffer,
    /// Appended to each line sent from the input bar
    pub line_ending: &'static str,
    restart: RestartPolicy,
    /// Whether the user wants it running. A stop clears it, so the exit that follows doesn't restart it.
    wanted: bool,
    /// Start again as soon as the current run ends
    restart_when_ended: bool,
    run: u64,
    /// When the current or last run started
    pub started: Instant,
    /// When an automatic restart is due, after an exit
    pub restart_at: Option<Instant>,
    quick_exits: u32,
    autostart: bool,
    source: Box<dyn Source>,
}

pub struct Supervisor {
    entries: Vec<Entry>,
    tx: Sender<Envelope>,
    rx: Receiver<Envelope>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Supervisor {
    /// `wake` is called from the sources' threads whenever they have news, to get the UI to poll
    pub fn new(profile: &Profile, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut supervisor = Self { entries: Vec::new(), tx, rx, wake };
        for spec in &profile.processes {
            let source = ProcessSource::new(spec.clone(), &profile.env);
            supervisor.add(&spec.name, Box::new(source), spec.restart, spec.autostart, "\n");
        }
        for spec in &profile.serials {
            let ending = spec.line_ending.as_str();
            supervisor.add(
                &spec.name,
                Box::new(SerialSource::new(spec.clone())),
                RestartPolicy::Never,
                spec.autostart,
                ending,
            );
        }
        supervisor
    }

    fn add(
        &mut self,
        name: &str,
        source: Box<dyn Source>,
        restart: RestartPolicy,
        autostart: bool,
        line_ending: &'static str,
    ) {
        self.entries.push(Entry {
            name: name.to_owned(),
            kind: source.kind(),
            description: source.describe(),
            status: Status::Stopped,
            buffer: LineBuffer::new(BUFFER_LINES),
            line_ending,
            restart,
            wanted: false,
            restart_when_ended: false,
            run: 0,
            started: Instant::now(),
            restart_at: None,
            quick_exits: 0,
            autostart,
            source,
        });
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn entry_mut(&mut self, id: SourceId) -> &mut Entry {
        &mut self.entries[id]
    }

    pub fn start_autostart(&mut self) {
        for id in 0..self.entries.len() {
            if self.entries[id].autostart {
                self.start(id);
            }
        }
    }

    pub fn start(&mut self, id: SourceId) {
        let entry = &mut self.entries[id];
        entry.wanted = true;
        entry.quick_exits = 0;
        if !entry.status.is_active() {
            self.launch(id);
        }
    }

    pub fn stop(&mut self, id: SourceId) {
        let entry = &mut self.entries[id];
        entry.wanted = false;
        entry.restart_when_ended = false;
        entry.restart_at = None;
        if entry.status.is_active() {
            entry.buffer.push_note("stopping", Level::Info);
            entry.source.stop();
        }
    }

    pub fn restart(&mut self, id: SourceId) {
        let entry = &mut self.entries[id];
        entry.wanted = true;
        entry.quick_exits = 0;
        if entry.status.is_active() {
            // Wait for the old run to end, so it lets go of whatever the new one needs (its port, its socket)
            entry.restart_when_ended = true;
            entry.buffer.push_note("restarting", Level::Info);
            entry.source.stop();
        } else {
            self.launch(id);
        }
    }

    pub fn start_all(&mut self) {
        (0..self.entries.len()).for_each(|id| self.start(id));
    }

    pub fn stop_all(&mut self) {
        (0..self.entries.len()).for_each(|id| self.stop(id));
    }

    pub fn write(&mut self, id: SourceId, data: &[u8]) -> io::Result<()> {
        self.entries[id].source.write(data)
    }

    fn launch(&mut self, id: SourceId) {
        let entry = &mut self.entries[id];
        entry.run += 1;
        entry.started = Instant::now();
        entry.restart_at = None;
        // Assume it's running until it says otherwise, so a second start before the first event doesn't launch twice
        entry.status = Status::Running;
        let sink = Sink::new(id, entry.run, self.tx.clone(), self.wake.clone());
        entry.source.start(sink);
    }

    /// Takes in everything the sources have sent. Returns whether anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(Envelope { id, run, event }) = self.rx.try_recv() {
            let entry = &mut self.entries[id];
            if run != entry.run {
                continue;
            }
            changed = true;
            match event {
                Event::Output(text) => entry.buffer.push_output(&text),
                Event::Note(text, level) => entry.buffer.push_note(text, level),
                Event::Status(status) => {
                    let ended = !status.is_active();
                    entry.status = status;
                    if ended {
                        self.ended(id);
                    }
                }
                Event::RestartDue => {
                    if entry.wanted && !entry.status.is_active() {
                        self.launch(id);
                    }
                }
            }
        }
        changed
    }

    fn ended(&mut self, id: SourceId) {
        let entry = &mut self.entries[id];
        if std::mem::take(&mut entry.restart_when_ended) {
            self.launch(id);
            return;
        }
        let success = match entry.status {
            Status::Exited(code) => code == Some(0),
            _ => false,
        };
        let restart =
            entry.wanted && matches!(entry.status, Status::Exited(_)) && entry.restart.should_restart(success);
        if !restart {
            entry.wanted = false;
            return;
        }

        if entry.started.elapsed() < QUICK_EXIT {
            entry.quick_exits += 1;
        } else {
            entry.quick_exits = 0;
        }
        let delay = Duration::from_secs(1 << entry.quick_exits.min(5)).min(MAX_RESTART_DELAY);
        entry.buffer.push_note(format!("restarting in {}s", delay.as_secs()), Level::Info);
        entry.restart_at = Some(Instant::now() + delay);
        let sink = Sink::new(id, entry.run, self.tx.clone(), self.wake.clone());
        thread::spawn(move || {
            thread::sleep(delay);
            sink.send(Event::RestartDue);
        });
    }

    /// Stops everything and waits up to `timeout` for it all to end. Whatever is left gets a hangup when its PTY
    /// closes.
    pub fn shutdown(&mut self, timeout: Duration) {
        self.stop_all();
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline && self.entries.iter().any(|e| e.status.is_active()) {
            thread::sleep(Duration::from_millis(20));
            self.poll();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supervisor(profile: &str) -> Supervisor {
        Supervisor::new(&Profile::parse(profile).unwrap(), Arc::new(|| {}))
    }

    /// Polls until `done` holds, or panics after a few seconds
    fn poll_until(sup: &mut Supervisor, done: impl Fn(&Supervisor) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(sup) {
            assert!(Instant::now() < deadline, "timed out");
            sup.poll();
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn texts(entry: &Entry) -> Vec<String> {
        entry.buffer.iter().map(|l| l.text.clone()).collect()
    }

    #[test]
    fn captures_output_and_exit() {
        let mut sup = supervisor("[[process]]\nname = \"echo\"\ncmd = \"echo hello; echo world; exit 3\"\n");
        sup.start(0);
        poll_until(&mut sup, |s| s.entries()[0].status == Status::Exited(Some(3)));
        let lines = texts(&sup.entries()[0]);
        assert!(lines.contains(&"hello".to_owned()), "{lines:?}");
        assert!(lines.contains(&"world".to_owned()), "{lines:?}");
        assert!(lines.last().unwrap().contains("exited with code 3"), "{lines:?}");
    }

    #[test]
    fn profile_env_reaches_the_process() {
        let mut sup = supervisor(
            "env = { ARCHON_TEST = \"from-profile\" }\n[[process]]\nname = \"env\"\ncmd = \"echo $ARCHON_TEST\"\n",
        );
        sup.start(0);
        poll_until(&mut sup, |s| matches!(s.entries()[0].status, Status::Exited(_)));
        assert!(texts(&sup.entries()[0]).contains(&"from-profile".to_owned()));
    }

    #[test]
    fn stop_interrupts_the_process_group() {
        // The trap shows the process got SIGINT rather than a kill
        let mut sup = supervisor(
            "[[process]]\nname = \"sleeper\"\ncmd = \"trap 'echo got-int; exit 0' INT; echo ready; while true; do sleep 0.05; done\"\n",
        );
        sup.start(0);
        poll_until(&mut sup, |s| texts(&s.entries()[0]).contains(&"ready".to_owned()));
        sup.stop(0);
        poll_until(&mut sup, |s| matches!(s.entries()[0].status, Status::Exited(_)));
        assert!(texts(&sup.entries()[0]).contains(&"got-int".to_owned()));
    }

    #[test]
    fn restarts_on_failure() {
        let mut sup =
            supervisor("[[process]]\nname = \"crash\"\ncmd = \"echo run; exit 1\"\nrestart = \"on-failure\"\n");
        sup.start(0);
        poll_until(&mut sup, |s| texts(&s.entries()[0]).iter().filter(|l| *l == "run").count() >= 2);
        sup.stop(0);
    }

    #[test]
    fn stop_cancels_a_pending_restart() {
        let mut sup = supervisor("[[process]]\nname = \"crash\"\ncmd = \"exit 1\"\nrestart = \"always\"\n");
        sup.start(0);
        poll_until(&mut sup, |s| s.entries()[0].restart_at.is_some());
        sup.stop(0);
        assert!(sup.entries()[0].restart_at.is_none());
        thread::sleep(Duration::from_millis(1200));
        sup.poll();
        assert_eq!(sup.entries()[0].status, Status::Exited(Some(1)), "it stayed stopped");
    }

    #[test]
    fn restart_waits_for_the_old_run() {
        let mut sup = supervisor("[[process]]\nname = \"long\"\ncmd = \"echo up; sleep 30\"\n");
        sup.start(0);
        poll_until(&mut sup, |s| texts(&s.entries()[0]).contains(&"up".to_owned()));
        sup.restart(0);
        poll_until(&mut sup, |s| texts(&s.entries()[0]).iter().filter(|l| *l == "up").count() >= 2);
        assert_eq!(sup.entries()[0].status, Status::Running);
        sup.shutdown(Duration::from_secs(5));
        assert!(!sup.entries()[0].status.is_active());
    }

    #[test]
    fn missing_serial_port_waits() {
        let mut sup = supervisor("[[serial]]\nname = \"board\"\nport = \"/dev/archon-test-missing\"\n");
        sup.start_autostart();
        poll_until(&mut sup, |s| matches!(s.entries()[0].status, Status::Waiting(_)));
        sup.stop(0);
        poll_until(&mut sup, |s| s.entries()[0].status == Status::Stopped);
    }
}
