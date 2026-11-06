//! A small ROS 2 client for archon: the topics on the graph, and subscriptions that hand over each message's raw CDR
//! bytes, for any message type installed, without generated code. The middleware comes from `RMW_IMPLEMENTATION` as
//! for any ROS program, so the same client talks to Fast DDS (micro-ROS) or zenoh.
//!
//! All the rcl state lives on one thread; [`Ros`] is the handle to it.

mod echo;
mod rcl;
mod topic;
mod typesupport;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub use echo::EchoSource;
pub use topic::{RAW_KEEP, Rate, RawMessage, SERIES_LEN, TopicState, Watcher};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topic {
    pub name: String,
    /// Usually one; more means publishers disagree, which is worth showing
    pub types: Vec<String>,
    pub publishers: usize,
    /// archon's own subscriptions count too
    pub subscribers: usize,
}

/// A snapshot of the graph, refreshed about once a second
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Graph {
    pub topics: Vec<Topic>,
    /// The other nodes on the graph, and what each one publishes, subscribes to and serves
    pub nodes: Vec<NodeInfo>,
}

/// A node, by its full name (`/talos/ivc_board`), with the topics and services it has as (name, type)
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeInfo {
    pub name: String,
    pub publishes: Vec<(String, String)>,
    pub subscribes: Vec<(String, String)>,
    pub services: Vec<(String, String)>,
}

impl Graph {
    pub fn topic(&self, name: &str) -> Option<&Topic> {
        self.topics.iter().find(|t| t.name == name)
    }
}

pub type SubscriptionId = u64;

/// What a subscription's callback receives
pub enum Delivery<'a> {
    Message(&'a [u8]),
    /// The subscription couldn't be made, such as for a type with no type support installed
    Failed(String),
}

pub type Callback = Box<dyn FnMut(Delivery<'_>) + Send>;

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// `None` uses `ROS_DOMAIN_ID`, as ROS programs do
    pub domain_id: Option<usize>,
}

pub struct Ros {
    commands: Sender<Command>,
    graph: Arc<Mutex<Arc<Graph>>>,
    /// Publishing failures, until taken
    errors: Arc<Mutex<Vec<String>>>,
    next_id: AtomicU64,
    thread: Option<JoinHandle<()>>,
}

enum Command {
    Subscribe {
        id: SubscriptionId,
        topic: String,
        ty: String,
        callback: Callback,
    },
    Unsubscribe(SubscriptionId),
    /// Makes the publisher ahead of the first message, so subscribers have matched it by then
    Advertise {
        topic: String,
        ty: String,
    },
    /// A serialized message to publish; the publisher is made the first time and kept
    Publish {
        topic: String,
        ty: String,
        bytes: Vec<u8>,
    },
    Shutdown,
}

impl Ros {
    /// Joins the ROS graph as a node named `archon_<pid>`. `wake` is called when the graph changes.
    pub fn start(options: Options, wake: Arc<dyn Fn() + Send + Sync>) -> anyhow::Result<Ros> {
        let (commands, receiver) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let graph = Arc::new(Mutex::new(Arc::new(Graph::default())));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let (shared, errors_shared) = (graph.clone(), errors.clone());
        let thread = std::thread::Builder::new()
            .name("ros".to_owned())
            .spawn(move || rcl::run(options, receiver, shared, errors_shared, wake, ready_tx))?;
        ready_rx.recv().unwrap_or_else(|_| Err(anyhow::anyhow!("the ROS thread died starting up")))?;
        Ok(Ros { commands, graph, errors, next_id: AtomicU64::new(1), thread: Some(thread) })
    }

    pub fn graph(&self) -> Arc<Graph> {
        self.graph.lock().unwrap().clone()
    }

    /// Subscribes to `topic` as type `ty` (like `std_msgs/msg/String`). Failure arrives as [`Delivery::Failed`].
    pub fn subscribe(&self, topic: &str, ty: &str, callback: Callback) -> SubscriptionId {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let command = Command::Subscribe { id, topic: topic.to_owned(), ty: ty.to_owned(), callback };
        let _ = self.commands.send(command);
        id
    }

    pub fn unsubscribe(&self, id: SubscriptionId) {
        let _ = self.commands.send(Command::Unsubscribe(id));
    }

    /// Gets ready to publish to `topic`: subscribers find a publisher in the background, which takes a moment
    pub fn advertise(&self, topic: &str, ty: &str) {
        let _ = self.commands.send(Command::Advertise { topic: topic.to_owned(), ty: ty.to_owned() });
    }

    /// Publishes a serialized (CDR) message of type `ty`. It waits a moment for a subscriber to match the
    /// publisher, so a message to a subscriber that's there isn't lost. Failures, and messages nothing subscribed to,
    /// show up in [`Ros::take_errors`].
    pub fn publish(&self, topic: &str, ty: &str, bytes: Vec<u8>) {
        let _ = self.commands.send(Command::Publish { topic: topic.to_owned(), ty: ty.to_owned(), bytes });
    }

    /// Publishing failures since the last call
    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut self.errors.lock().unwrap())
    }
}

impl Drop for Ros {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
