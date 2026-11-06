//! A small ROS 2 client for archon: the topics on the graph, and subscriptions that hand over each message's raw CDR
//! bytes, for any message type installed, without generated code. The middleware comes from `RMW_IMPLEMENTATION` as
//! for any ROS program, so the same client talks to Fast DDS (micro-ROS) or zenoh.
//!
//! All the rcl state lives on one thread; [`Ros`] is the handle to it.

mod echo;
mod rcl;
mod typesupport;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub use echo::EchoSource;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topic {
    pub name: String,
    /// Usually one; more means publishers disagree, which is worth showing
    pub types: Vec<String>,
    pub publishers: usize,
}

/// A snapshot of the graph, refreshed about once a second
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Graph {
    pub topics: Vec<Topic>,
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
    next_id: AtomicU64,
    thread: Option<JoinHandle<()>>,
}

enum Command {
    Subscribe { id: SubscriptionId, topic: String, ty: String, callback: Callback },
    Unsubscribe(SubscriptionId),
    Shutdown,
}

impl Ros {
    /// Joins the ROS graph as a node named `archon_<pid>`. `wake` is called when the graph changes.
    pub fn start(options: Options, wake: Arc<dyn Fn() + Send + Sync>) -> anyhow::Result<Ros> {
        let (commands, receiver) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let graph = Arc::new(Mutex::new(Arc::new(Graph::default())));
        let shared = graph.clone();
        let thread = std::thread::Builder::new()
            .name("ros".to_owned())
            .spawn(move || rcl::run(options, receiver, shared, wake, ready_tx))?;
        ready_rx.recv().unwrap_or_else(|_| Err(anyhow::anyhow!("the ROS thread died starting up")))?;
        Ok(Ros { commands, graph, next_id: AtomicU64::new(1), thread: Some(thread) })
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
}

impl Drop for Ros {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
