//! A serial port that reconnects on its own, so flashing a board (which re-enumerates its USB device) doesn't end the
//! session the way it ends `screen`.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use archon_core::decode::TextDecoder;
use archon_core::line::Level;
use archon_core::profile::SerialSpec;
use archon_core::source::{Sink, Source, SourceKind, Status};
use serialport::SerialPort;

/// How long a read waits, which bounds how long a stop takes
const READ_TIMEOUT: Duration = Duration::from_millis(100);
const RETRY_INTERVAL: Duration = Duration::from_millis(250);

type SharedPort = Arc<Mutex<Option<Box<dyn SerialPort>>>>;

pub struct SerialSource {
    spec: SerialSpec,
    run: Option<Run>,
}

struct Run {
    stop: Arc<AtomicBool>,
    /// A second handle to the open port for writes, or `None` while disconnected
    writer: SharedPort,
}

impl SerialSource {
    pub fn new(spec: SerialSpec) -> Self {
        Self { spec, run: None }
    }
}

impl Source for SerialSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Serial
    }

    fn describe(&self) -> String {
        format!("{} @ {}", self.spec.port, self.spec.baud)
    }

    fn start(&mut self, sink: Sink) {
        let run = Run { stop: Arc::new(AtomicBool::new(false)), writer: Arc::default() };
        let (spec, stop, writer) = (self.spec.clone(), run.stop.clone(), run.writer.clone());
        let spawned = thread::Builder::new()
            .name(format!("{}-serial", spec.name))
            .spawn(move || run_port(&spec, &sink, &stop, &writer));
        if spawned.is_ok() {
            self.run = Some(run);
        }
    }

    fn stop(&mut self) {
        if let Some(run) = &self.run {
            run.stop.store(true, Ordering::SeqCst);
        }
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let not_connected = || io::Error::new(io::ErrorKind::NotConnected, "the port isn't connected");
        let run = self.run.as_ref().ok_or_else(not_connected)?;
        let mut port = run.writer.lock().unwrap();
        let port = port.as_mut().ok_or_else(not_connected)?;
        port.write_all(data)?;
        port.flush()
    }
}

fn run_port(spec: &SerialSpec, sink: &Sink, stop: &AtomicBool, writer: &Mutex<Option<Box<dyn SerialPort>>>) {
    // Only say we're waiting once per disconnection, not on every retry
    let mut said_waiting = false;
    while !stop.load(Ordering::SeqCst) {
        let mut port = match serialport::new(&spec.port, spec.baud).timeout(READ_TIMEOUT).open() {
            Ok(port) => port,
            Err(e) => {
                if !said_waiting {
                    sink.note(format!("waiting for {}: {e}", spec.port), Level::Info);
                    sink.status(Status::Waiting(e.to_string()));
                    said_waiting = true;
                }
                sleep_unless_stopped(RETRY_INTERVAL, stop);
                continue;
            }
        };

        said_waiting = false;
        *writer.lock().unwrap() = port.try_clone().ok();
        sink.note(format!("connected to {} at {} baud", spec.port, spec.baud), Level::Info);
        sink.status(Status::Running);

        let mut decoder = TextDecoder::new();
        let mut buf = [0u8; 4096];
        let lost = loop {
            if stop.load(Ordering::SeqCst) {
                break None;
            }
            match port.read(&mut buf) {
                // Zero bytes without a timeout is the device hanging up
                Ok(0) => break Some("the device hung up".to_owned()),
                Ok(n) => {
                    if !sink.output(decoder.feed(&buf[..n])) {
                        return;
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {}
                Err(e) => break Some(e.to_string()),
            }
        };
        *writer.lock().unwrap() = None;

        if let Some(reason) = lost {
            sink.note(format!("disconnected: {reason}"), Level::Warn);
            sink.status(Status::Waiting(reason));
            said_waiting = true;
            sleep_unless_stopped(RETRY_INTERVAL, stop);
        }
    }
    sink.note("closed", Level::Info);
    sink.status(Status::Stopped);
}

fn sleep_unless_stopped(duration: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(20));
    }
}
