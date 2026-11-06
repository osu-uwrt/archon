//! What archon knows about a topic it listens to: the latest message, which fields changed and when, a short history
//! of each number for sparklines, and the message rate.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use archon_msg::Value;

use crate::{Delivery, Graph, Ros, SubscriptionId};

/// The rate is over the messages of the last this long
const RATE_WINDOW: Duration = Duration::from_secs(2);
/// Fields are compared and sampled at most this often, which bounds the cost of a 1 kHz topic
const SAMPLE_EVERY: Duration = Duration::from_millis(10);
/// Samples kept per number, for its sparkline
pub const SERIES_LEN: usize = 120;
/// Elements of an array tracked as fields of their own
const ARRAY_FIELDS: usize = 32;

#[derive(Default)]
pub struct Rate {
    times: VecDeque<Instant>,
}

impl Rate {
    pub fn tick(&mut self) {
        let now = Instant::now();
        self.times.push_back(now);
        while self.times.front().is_some_and(|&t| now - t > RATE_WINDOW) {
            self.times.pop_front();
        }
    }

    /// Messages per second, or `None` when nothing came for a while
    pub fn hz(&self) -> Option<f64> {
        let (first, last) = (*self.times.front()?, *self.times.back()?);
        if self.times.len() < 2 || last.elapsed() > RATE_WINDOW {
            return None;
        }
        Some((self.times.len() - 1) as f64 / (last - first).as_secs_f64())
    }

    pub fn last(&self) -> Option<Instant> {
        self.times.back().copied()
    }
}

#[derive(Clone, PartialEq, Debug)]
enum Leaf {
    Number(f64),
    Text(String),
}

#[derive(Default)]
pub struct TopicState {
    pub topic: String,
    /// Known once the topic is on the graph
    pub ty: Option<String>,
    pub latest: Option<Value>,
    pub count: u64,
    pub rate: Rate,
    leaves: HashMap<String, Leaf>,
    changed: HashMap<String, Instant>,
    series: HashMap<String, VecDeque<f64>>,
    last_sample: Option<Instant>,
    /// Fields being plotted, with every value they've had and when, recorded on every message
    traces: HashMap<String, VecDeque<(Instant, f64)>>,
    /// The last messages as they arrived, before decoding, for looking at the bytes
    pub raw: VecDeque<RawMessage>,
}

pub struct RawMessage {
    /// Its number among all the topic's messages, from 1
    pub number: u64,
    pub at: Instant,
    /// Cut short at [`RAW_BYTES`]
    pub bytes: Vec<u8>,
    pub size: usize,
    /// Why it didn't decode, if it didn't
    pub error: Option<String>,
}

/// Raw messages kept per topic
pub const RAW_KEEP: usize = 500;
/// Bytes kept of each raw message; an image doesn't need all of its megabytes in a hex view
pub const RAW_BYTES: usize = 64 * 1024;

/// The most points a plotted field keeps; at 1 kHz that's the last 50 seconds
pub const TRACE_LEN: usize = 50_000;

impl TopicState {
    pub fn new(topic: &str, ty: Option<&str>) -> Self {
        Self { topic: topic.to_owned(), ty: ty.map(str::to_owned), ..Self::default() }
    }

    pub fn update(&mut self, value: Value) {
        let now = Instant::now();
        self.count += 1;
        self.rate.tick();
        for (path, trace) in &mut self.traces {
            if let Some(y) = value.get(path).and_then(Value::as_f64) {
                if trace.len() == TRACE_LEN {
                    trace.pop_front();
                }
                trace.push_back((now, y));
            }
        }
        if self.last_sample.is_none_or(|at| now - at >= SAMPLE_EVERY) {
            self.last_sample = Some(now);
            let mut leaves = Vec::new();
            flatten("", &value, &mut leaves);
            for (path, leaf) in leaves {
                if let Leaf::Number(x) = leaf {
                    let series = self.series.entry(path.clone()).or_default();
                    if series.len() == SERIES_LEN {
                        series.pop_front();
                    }
                    series.push_back(x);
                }
                match self.leaves.get(&path) {
                    Some(old) if *old == leaf => {}
                    // The first value isn't a change
                    None => _ = self.leaves.insert(path, leaf),
                    Some(_) => {
                        self.changed.insert(path.clone(), now);
                        self.leaves.insert(path, leaf);
                    }
                }
            }
        }
        self.latest = Some(value);
    }

    /// Keeps a message's bytes as they arrived. One that didn't decode still counts as a message.
    pub fn record(&mut self, bytes: &[u8], error: Option<String>) {
        if error.is_some() {
            self.count += 1;
            self.rate.tick();
        }
        if self.raw.len() == RAW_KEEP {
            self.raw.pop_front();
        }
        let number = self.raw.back().map_or(1, |m| m.number + 1);
        let kept = bytes[..bytes.len().min(RAW_BYTES)].to_vec();
        self.raw.push_back(RawMessage { number, at: Instant::now(), bytes: kept, size: bytes.len(), error });
    }

    /// Starts recording every value of a field, for a plot
    pub fn trace(&mut self, path: &str) {
        self.traces.entry(path.to_owned()).or_default();
    }

    pub fn untrace(&mut self, path: &str) {
        self.traces.remove(path);
    }

    /// Every value of a traced field since it was traced, oldest first
    pub fn traced(&self, path: &str) -> Option<&VecDeque<(Instant, f64)>> {
        self.traces.get(path)
    }

    pub fn clear_traces(&mut self) {
        self.traces.values_mut().for_each(VecDeque::clear);
    }

    /// When the field at `path` (like `linear_acceleration.z`) last changed
    pub fn changed_at(&self, path: &str) -> Option<Instant> {
        self.changed.get(path).copied()
    }

    /// Recent values of a numeric field, oldest first
    pub fn series(&self, path: &str) -> Option<&VecDeque<f64>> {
        self.series.get(path)
    }
}

fn flatten(prefix: &str, value: &Value, out: &mut Vec<(String, Leaf)>) {
    let join = |name: &str| if prefix.is_empty() { name.to_owned() } else { format!("{prefix}.{name}") };
    match value {
        Value::Struct(fields) => fields.iter().for_each(|(name, v)| flatten(&join(name), v, out)),
        Value::Array(items) => {
            for (i, item) in items.iter().take(ARRAY_FIELDS).enumerate() {
                flatten(&format!("{prefix}[{i}]"), item, out);
            }
        }
        Value::Bytes(bytes) => {
            for (i, b) in bytes.iter().take(ARRAY_FIELDS).enumerate() {
                out.push((format!("{prefix}[{i}]"), Leaf::Number(f64::from(*b))));
            }
        }
        Value::String(s) => out.push((prefix.to_owned(), Leaf::Text(s.clone()))),
        scalar => out.push((prefix.to_owned(), Leaf::Number(scalar.as_f64().unwrap_or(f64::NAN)))),
    }
}

/// Counts a topic's messages without decoding them, for its rate. Subscribes once the topic is on the graph.
pub struct Watcher {
    topic: String,
    subscription: Option<SubscriptionId>,
    rate: Arc<Mutex<Rate>>,
    failed: Arc<Mutex<Option<String>>>,
}

impl Watcher {
    pub fn new(topic: &str) -> Self {
        Self { topic: topic.to_owned(), subscription: None, rate: Arc::default(), failed: Arc::default() }
    }

    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// Cheap enough to call every frame
    pub fn poll(&mut self, ros: &Ros, graph: &Graph) {
        if self.subscription.is_some() || self.failed.lock().unwrap().is_some() {
            return;
        }
        let Some(ty) = graph.topic(&self.topic).and_then(|t| t.types.first()) else { return };
        let (rate, failed) = (self.rate.clone(), self.failed.clone());
        let callback = Box::new(move |delivery: Delivery<'_>| match delivery {
            Delivery::Message(_) => rate.lock().unwrap().tick(),
            Delivery::Failed(why) => *failed.lock().unwrap() = Some(why),
        });
        self.subscription = Some(ros.subscribe(&self.topic, ty, callback));
    }

    pub fn hz(&self) -> Option<f64> {
        self.rate.lock().unwrap().hz()
    }

    pub fn last_message(&self) -> Option<Instant> {
        self.rate.lock().unwrap().last()
    }

    pub fn failed(&self) -> Option<String> {
        self.failed.lock().unwrap().clone()
    }

    pub fn stop(&mut self, ros: &Ros) {
        if let Some(id) = self.subscription.take() {
            ros.unsubscribe(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imu(z: f64, frame: &str) -> Value {
        let field = |n: &str, v| (n.to_owned(), v);
        Value::Struct(vec![
            field("frame", Value::String(frame.to_owned())),
            field("accel", Value::Struct(vec![field("x", Value::Float64(0.0)), field("z", Value::Float64(z))])),
            field("force", Value::Array(vec![Value::Float32(1.0), Value::Float32(2.0)])),
        ])
    }

    #[test]
    fn tracks_changes_and_series() {
        let mut state = TopicState::default();
        state.update(imu(9.8, "base"));
        assert_eq!(state.changed_at("accel.z"), None, "the first value isn't a change");
        std::thread::sleep(SAMPLE_EVERY);
        state.update(imu(9.9, "base"));
        assert!(state.changed_at("accel.z").is_some());
        assert!(state.changed_at("accel.x").is_none());
        assert!(state.changed_at("frame").is_none());
        assert_eq!(state.series("accel.z").unwrap().iter().copied().collect::<Vec<_>>(), [9.8, 9.9]);
        assert_eq!(state.series("force[1]").unwrap().len(), 2);
        assert_eq!(state.count, 2);
    }

    #[test]
    fn samples_fast_topics_at_most_every_10ms() {
        let mut state = TopicState::default();
        for i in 0..50 {
            state.update(imu(f64::from(i), "base"));
        }
        assert_eq!(state.count, 50);
        assert!(state.series("accel.z").unwrap().len() < 5);
        assert!(matches!(state.latest.as_ref().and_then(|v| v.get("accel.z")), Some(Value::Float64(49.0))));
    }

    #[test]
    fn traces_every_value_of_a_plotted_field() {
        let mut state = TopicState::default();
        state.trace("accel.z");
        for z in [1.0, 2.0, 3.0] {
            state.update(imu(z, "base"));
        }
        let values: Vec<f64> = state.traced("accel.z").unwrap().iter().map(|(_, v)| *v).collect();
        assert_eq!(values, [1.0, 2.0, 3.0], "not sampled like the sparklines");
        assert!(state.traced("accel.x").is_none());
    }

    #[test]
    fn keeps_raw_messages_with_their_errors() {
        let mut state = TopicState::default();
        state.record(&[0, 1, 0, 0, 7], None);
        state.record(&[9; 3], Some("only 3 bytes".into()));
        assert_eq!(state.raw.len(), 2);
        assert_eq!(state.raw[1].number, 2);
        assert_eq!(state.count, 1, "the undecodable one counts; the decoded one is counted by update");
        for _ in 0..RAW_KEEP {
            state.record(&[0], None);
        }
        assert_eq!(state.raw.len(), RAW_KEEP);
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
