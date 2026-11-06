//! The interface every line producer implements: supervised processes, serial ports, and later CAN buses and ROS
//! topic echoes. A source runs on its own threads and reports through a [`Sink`], so the UI never waits on it.

use std::io;
use std::sync::Arc;
use std::sync::mpsc::Sender;

pub type SourceId = usize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceKind {
    Process,
    Serial,
    /// A ROS topic echo
    Topic,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Status {
    Stopped,
    Running,
    /// Wants to run but can't yet, such as a serial port whose device isn't plugged in
    Waiting(String),
    /// A process ended. `None` means it was killed by a signal.
    Exited(Option<u32>),
    Failed(String),
}

impl Status {
    pub fn is_active(&self) -> bool {
        matches!(self, Status::Running | Status::Waiting(_))
    }
}

pub enum Event {
    /// Decoded output, which need not end at a line break
    Output(String),
    /// archon's own message about the source
    Note(String, crate::line::Level),
    Status(Status),
    /// The restart delay after an exit has passed
    RestartDue,
}

pub struct Envelope {
    pub id: SourceId,
    /// Which run of the source sent this, so events from a run that has since been replaced are ignored
    pub run: u64,
    pub event: Event,
}

/// Where one run of a source sends its events. Sending also wakes the UI.
#[derive(Clone)]
pub struct Sink {
    id: SourceId,
    run: u64,
    tx: Sender<Envelope>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Sink {
    pub fn new(id: SourceId, run: u64, tx: Sender<Envelope>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { id, run, tx, wake }
    }

    /// Returns false once nobody is listening, which tells the source's threads to give up
    pub fn send(&self, event: Event) -> bool {
        let sent = self.tx.send(Envelope { id: self.id, run: self.run, event }).is_ok();
        (self.wake)();
        sent
    }

    pub fn output(&self, text: String) -> bool {
        text.is_empty() || self.send(Event::Output(text))
    }

    pub fn note(&self, text: impl Into<String>, level: crate::line::Level) -> bool {
        self.send(Event::Note(text.into(), level))
    }

    pub fn status(&self, status: Status) -> bool {
        self.send(Event::Status(status))
    }
}

pub trait Source: Send {
    fn kind(&self) -> SourceKind;

    /// The command or port, for display
    fn describe(&self) -> String;

    /// Starts a new run that reports through `sink`. Called only when the previous run has ended.
    fn start(&mut self, sink: Sink);

    /// Asks the current run to end without waiting for it. The run reports its end through its sink.
    fn stop(&mut self);

    /// Sends input to the source: a process's stdin, or the serial port
    fn write(&mut self, data: &[u8]) -> io::Result<()>;

    /// A live detail for the sidebar while it runs, like a topic's message rate
    fn detail(&self) -> Option<String> {
        None
    }
}
