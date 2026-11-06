//! `ros2 topic echo` as a source: each message decoded to text, a `---` between messages, and the message rate for
//! the sidebar. Log messages (`/rosout`) come out as log lines instead, so every node's logs read like one terminal.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use archon_core::line::Level;
use archon_core::source::{Sink, Source, SourceKind, Status};
use archon_msg::{Registry, TypeName, Value, cdr};

use crate::{Delivery, Ros, SubscriptionId};

/// How often to look for a topic that isn't on the graph yet
const LOOK_EVERY: Duration = Duration::from_millis(500);
/// The rate is over the messages of the last this long
const RATE_WINDOW: Duration = Duration::from_secs(2);

const LOG_TYPE: &str = "rcl_interfaces/msg/Log";

pub struct EchoSource {
    ros: Arc<Ros>,
    registry: Arc<Mutex<Registry>>,
    topic: String,
    /// Known up front, or found on the graph once the topic appears
    ty: Option<String>,
    rate: Arc<Mutex<Rate>>,
    run: Option<Run>,
}

struct Run {
    stop: Arc<AtomicBool>,
    subscription: Arc<Mutex<Option<SubscriptionId>>>,
    sink: Sink,
}

impl EchoSource {
    pub fn new(ros: Arc<Ros>, registry: Arc<Mutex<Registry>>, topic: &str, ty: Option<&str>) -> Self {
        Self { ros, registry, topic: topic.to_owned(), ty: ty.map(str::to_owned), rate: Arc::default(), run: None }
    }
}

impl Source for EchoSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Topic
    }

    fn describe(&self) -> String {
        self.ty.clone().unwrap_or_else(|| "type not known yet".to_owned())
    }

    fn start(&mut self, sink: Sink) {
        let run = Run { stop: Arc::default(), subscription: Arc::default(), sink: sink.clone() };
        let (stop, subscription) = (run.stop.clone(), run.subscription.clone());
        let (ros, registry, rate) = (self.ros.clone(), self.registry.clone(), self.rate.clone());
        let (topic, ty) = (self.topic.clone(), self.ty.clone());
        *self.rate.lock().unwrap() = Rate::default();
        self.run = Some(run);
        // Off the UI thread, since the topic may not exist yet
        std::thread::spawn(move || {
            let Some(ty) = ty.or_else(|| wait_for_type(&ros, &topic, &sink, &stop)) else {
                sink.status(Status::Stopped);
                return;
            };
            let Some(name) = TypeName::parse(&ty) else {
                sink.note(format!("{ty} isn't a message type archon can read"), Level::Error);
                sink.status(Status::Failed(format!("can't read {ty}")));
                return;
            };
            if let Err(e) = registry.lock().unwrap().load(&name) {
                sink.note(format!("{e:#}"), Level::Error);
                sink.status(Status::Failed(format!("no definition of {ty}")));
                return;
            }

            let deliver = sink.clone();
            let type_name = ty.clone();
            let id = ros.subscribe(
                &topic,
                &type_name,
                Box::new(move |delivery| match delivery {
                    Delivery::Message(bytes) => {
                        rate.lock().unwrap().tick();
                        let decoded = cdr::decode(&registry.lock().unwrap(), &name, bytes);
                        deliver.output(match decoded {
                            Ok(value) => format_message(&ty, &value),
                            Err(e) => format!("[archon] couldn't decode a {ty} message: {e:#}\n"),
                        });
                    }
                    Delivery::Failed(e) => {
                        deliver.note(e.clone(), Level::Error);
                        deliver.status(Status::Failed(e));
                    }
                }),
            );
            let mut held = subscription.lock().unwrap();
            if stop.load(Ordering::SeqCst) {
                ros.unsubscribe(id);
                sink.status(Status::Stopped);
                return;
            }
            *held = Some(id);
            sink.note(format!("subscribed to {topic}"), Level::Info);
            sink.status(Status::Running);
        });
    }

    fn stop(&mut self) {
        let Some(run) = &self.run else { return };
        run.stop.store(true, Ordering::SeqCst);
        // While still looking for the topic, the looking thread reports the stop instead
        if let Some(id) = run.subscription.lock().unwrap().take() {
            self.ros.unsubscribe(id);
            run.sink.note("unsubscribed", Level::Info);
            run.sink.status(Status::Stopped);
        }
    }

    fn write(&mut self, _data: &[u8]) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "publishing to a topic isn't supported yet"))
    }

    fn detail(&self) -> Option<String> {
        let rate = self.rate.lock().unwrap().hz();
        let publishers = self.ros.graph().topic(&self.topic).map_or(0, |t| t.publishers);
        let rate = match rate {
            Some(hz) if hz >= 10.0 => format!("{hz:.0} Hz"),
            Some(hz) => format!("{hz:.1} Hz"),
            None => "no messages".to_owned(),
        };
        let s = if publishers == 1 { "" } else { "s" };
        Some(format!("{rate} · {publishers} publisher{s}"))
    }
}

/// Waits for the topic to show up on the graph. `None` if stopped first.
fn wait_for_type(ros: &Ros, topic: &str, sink: &Sink, stop: &AtomicBool) -> Option<String> {
    let mut said = false;
    while !stop.load(Ordering::SeqCst) {
        if let Some(ty) = ros.graph().topic(topic).and_then(|t| t.types.first().cloned()) {
            return Some(ty);
        }
        if !said {
            sink.note(format!("waiting for {topic} to appear on the graph"), Level::Info);
            sink.status(Status::Waiting("not on the graph yet".to_owned()));
            said = true;
        }
        std::thread::sleep(LOOK_EVERY);
    }
    None
}

fn format_message(ty: &str, value: &Value) -> String {
    if ty == LOG_TYPE
        && let Some(line) = log_line(value)
    {
        return line;
    }
    let mut text = value.to_text();
    text.push_str("---\n");
    text
}

/// A `rcl_interfaces/msg/Log` as the line its node would have printed: `[WARN] [12.345] [thrusters]: no heartbeat`
fn log_line(value: &Value) -> Option<String> {
    let level = match value.get("level")?.as_f64()? as u8 {
        10 => "DEBUG",
        20 => "INFO",
        30 => "WARN",
        40 => "ERROR",
        50 => "FATAL",
        _ => "LOG",
    };
    let sec = value.get("stamp.sec")?.as_f64()?;
    let nanosec = value.get("stamp.nanosec")?.as_f64()?;
    let text = |field| match value.get(field) {
        Some(Value::String(s)) => Some(s.as_str()),
        _ => None,
    };
    Some(format!("[{level}] [{sec}.{:09}] [{}]: {}\n", nanosec as u64, text("name")?, text("msg")?))
}

#[derive(Default)]
struct Rate {
    times: VecDeque<Instant>,
}

impl Rate {
    fn tick(&mut self) {
        let now = Instant::now();
        self.times.push_back(now);
        while self.times.front().is_some_and(|&t| now - t > RATE_WINDOW) {
            self.times.pop_front();
        }
    }

    fn hz(&self) -> Option<f64> {
        let (first, last) = (*self.times.front()?, *self.times.back()?);
        if self.times.len() < 2 || last.elapsed() > RATE_WINDOW {
            return None;
        }
        Some((self.times.len() - 1) as f64 / (last - first).as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rosout_reads_as_log_lines() {
        let field = |n: &str, v: Value| (n.to_owned(), v);
        let log = Value::Struct(vec![
            field("stamp", Value::Struct(vec![field("sec", Value::Int(12)), field("nanosec", Value::UInt(5_000_000))])),
            field("level", Value::UInt(30)),
            field("name", Value::String("thrusters".into())),
            field("msg", Value::String("no heartbeat".into())),
        ]);
        assert_eq!(format_message(LOG_TYPE, &log), "[WARN] [12.005000000] [thrusters]: no heartbeat\n");
        assert!(format_message("std_msgs/msg/Empty", &Value::Struct(Vec::new())).ends_with("---\n"));
    }

    #[test]
    fn measures_rate() {
        let mut rate = Rate::default();
        assert_eq!(rate.hz(), None);
        let start = Instant::now();
        rate.times = (0..11).map(|i| start - Duration::from_millis(100 * (10 - i))).collect();
        let hz = rate.hz().unwrap();
        assert!((hz - 10.0).abs() < 0.01, "{hz}");
    }
}
