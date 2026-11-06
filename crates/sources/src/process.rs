//! A process on its own PTY. A PTY rather than pipes because most programs (`ros2 launch`, anything in Python) only
//! flush each line when they think they're writing to a terminal.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use archon_core::decode::TextDecoder;
use archon_core::line::Level;
use archon_core::profile::ProcessSpec;
use archon_core::source::{Sink, Source, SourceKind, Status};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Wide enough that programs which format for the terminal width don't wrap their lines
const COLUMNS: u16 = 400;

/// How long the output of a process that has exited may keep draining, in case something it started holds the PTY
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// What `stop` sends the process group, and how long it waits after each before the next
const STOP_SIGNALS: [(libc::c_int, Duration); 3] =
    [(libc::SIGINT, Duration::from_secs(3)), (libc::SIGTERM, Duration::from_secs(2)), (libc::SIGKILL, Duration::ZERO)];

pub struct ProcessSource {
    spec: ProcessSpec,
    /// The profile's environment with the process's own on top
    env: BTreeMap<String, String>,
    run: Option<Run>,
}

struct Run {
    pid: Option<u32>,
    exited: Arc<AtomicBool>,
    writer: Box<dyn Write + Send>,
    // Closing the master hangs up the PTY, so it lives as long as the run
    _master: Box<dyn MasterPty + Send>,
}

impl ProcessSource {
    pub fn new(spec: ProcessSpec, profile_env: &BTreeMap<String, String>) -> Self {
        let mut env = profile_env.clone();
        env.extend(spec.env.clone());
        Self { spec, env, run: None }
    }

    fn spawn(&self, sink: &Sink) -> anyhow::Result<Run> {
        let pair = native_pty_system().openpty(PtySize { rows: 50, cols: COLUMNS, pixel_width: 0, pixel_height: 0 })?;
        let mut cmd = CommandBuilder::new("bash");
        cmd.args(["-c", &self.spec.cmd]);
        if let Some(cwd) = &self.spec.cwd {
            cmd.cwd(cwd);
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        let mut child = pair.slave.spawn_command(cmd)?;
        // Only the child may hold the slave side, or the reader never sees the end of the output
        drop(pair.slave);

        let pid = child.process_id();
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let exited = Arc::new(AtomicBool::new(false));

        let pid_text = pid.map(|pid| format!(" (pid {pid})")).unwrap_or_default();
        sink.note(format!("started{pid_text}: {}", one_line(&self.spec.cmd)), Level::Info);
        sink.status(Status::Running);

        // The reader holds `drained_tx` and drops it when the output ends, which wakes the waiter
        let (drained_tx, drained_rx) = mpsc::channel::<()>();
        let out = sink.clone();
        thread::Builder::new().name(format!("{}-out", self.spec.name)).spawn(move || {
            let _drained = drained_tx;
            let mut reader = reader;
            let mut decoder = TextDecoder::new();
            let mut buf = [0u8; 8192];
            // A read error is the PTY hanging up once every process on it has exited
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if !out.output(decoder.feed(&buf[..n])) {
                    break;
                }
            }
        })?;

        let exited_flag = exited.clone();
        let sink = sink.clone();
        thread::Builder::new().name(format!("{}-wait", self.spec.name)).spawn(move || {
            let status = child.wait();
            exited_flag.store(true, Ordering::SeqCst);
            let _ = drained_rx.recv_timeout(DRAIN_TIMEOUT);
            match status {
                Ok(status) => {
                    let (code, text) = match status.signal() {
                        Some(signal) => (None, format!("killed by {signal}")),
                        None => (Some(status.exit_code()), format!("exited with code {}", status.exit_code())),
                    };
                    sink.note(text, if status.success() { Level::Info } else { Level::Warn });
                    sink.status(Status::Exited(code));
                }
                Err(e) => {
                    sink.note(format!("lost track of the process: {e}"), Level::Error);
                    sink.status(Status::Failed(e.to_string()));
                }
            }
        })?;

        Ok(Run { pid, exited, writer, _master: pair.master })
    }
}

impl Source for ProcessSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Process
    }

    fn describe(&self) -> String {
        one_line(&self.spec.cmd)
    }

    fn start(&mut self, sink: Sink) {
        self.run = None;
        match self.spawn(&sink) {
            Ok(run) => self.run = Some(run),
            Err(e) => {
                sink.note(format!("failed to start: {e:#}"), Level::Error);
                sink.status(Status::Failed(e.to_string()));
            }
        }
    }

    fn stop(&mut self) {
        let Some(Run { pid: Some(pid), exited, .. }) = &self.run else { return };
        let (pid, exited) = (*pid as libc::pid_t, exited.clone());
        // The child is a session leader, so its pid is its process group. Signal the whole group the way Ctrl+C in a
        // terminal does, so `ros2 launch` shuts its nodes down cleanly; escalate if it doesn't.
        thread::spawn(move || {
            for (signal, grace) in STOP_SIGNALS {
                if exited.load(Ordering::SeqCst) {
                    return;
                }
                // SAFETY: killpg has no memory-safety requirements. The group can't have been reused while the
                // leader is unreaped, and `exited` is set right after it's reaped.
                unsafe { libc::killpg(pid, signal) };
                let deadline = Instant::now() + grace;
                while Instant::now() < deadline && !exited.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(20));
                }
            }
        });
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        match &mut self.run {
            Some(run) if !run.exited.load(Ordering::SeqCst) => {
                run.writer.write_all(data)?;
                run.writer.flush()
            }
            _ => Err(io::Error::new(io::ErrorKind::NotConnected, "the process isn't running")),
        }
    }
}

/// A command written over several lines in the profile, on one line for display
fn one_line(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}
