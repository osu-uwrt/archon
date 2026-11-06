//! The ROS thread: owns the rcl context, node and subscriptions, takes in commands, waits for messages and refreshes
//! the graph snapshot.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::bail;
use r2r_rcl::*;

use crate::typesupport::TypeSupports;
use crate::{Callback, Command, Delivery, Graph, NodeInfo, Options, SubscriptionId, Topic};

/// How long a wait for messages lasts, which bounds how late a command is handled
const WAIT: Duration = Duration::from_millis(20);
const GRAPH_EVERY: Duration = Duration::from_millis(1000);
/// How long a message waits for a subscriber to match its publisher. A new publisher's first messages would
/// otherwise go out before discovery is done, to nobody: subscriptions are volatile, so they're lost.
const MATCH_WAIT: Duration = Duration::from_secs(2);

/// A message waiting for its publisher to have a subscriber
struct Pending {
    key: (String, String),
    bytes: Vec<u8>,
    since: Instant,
}

pub fn run(
    options: Options,
    commands: Receiver<Command>,
    graph: Arc<Mutex<Arc<Graph>>>,
    errors: Arc<Mutex<Vec<String>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    ready: Sender<anyhow::Result<()>>,
) {
    let mut node = match Node::new(options) {
        Ok(node) => {
            let _ = ready.send(Ok(()));
            node
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut types = TypeSupports::default();
    let mut subscriptions: HashMap<SubscriptionId, Subscription> = HashMap::new();
    let mut publishers: HashMap<(String, String), Box<rcl_publisher_t>> = HashMap::new();
    let mut pending: Vec<Pending> = Vec::new();
    // Topics already reported as having nothing subscribed, so a steady rate doesn't repeat it
    let mut unheard: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut wait_set = WaitSet::default();
    let mut last_graph: Option<Instant> = None;

    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Subscribe { id, topic, ty, mut callback }) => {
                    match types.get(&ty).and_then(|ts| node.subscribe(&topic, ts)) {
                        Ok(handle) => {
                            subscriptions.insert(id, Subscription { handle, callback });
                        }
                        Err(e) => callback(Delivery::Failed(format!("couldn't subscribe to {topic}: {e:#}"))),
                    }
                }
                Ok(Command::Unsubscribe(id)) => {
                    if let Some(subscription) = subscriptions.remove(&id) {
                        node.unsubscribe(subscription.handle);
                    }
                }
                Ok(Command::Advertise { topic, ty }) => {
                    if let Err(e) = publisher_for(&mut publishers, &mut types, &mut node, &topic, &ty) {
                        errors.lock().unwrap().push(format!("couldn't publish to {topic}: {e:#}"));
                        wake();
                    }
                }
                Ok(Command::Publish { topic, ty, bytes }) => {
                    match publisher_for(&mut publishers, &mut types, &mut node, &topic, &ty) {
                        Ok(_) => pending.push(Pending { key: (topic, ty), bytes, since: Instant::now() }),
                        Err(e) => {
                            errors.lock().unwrap().push(format!("couldn't publish to {topic}: {e:#}"));
                            wake();
                        }
                    }
                }
                Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => {
                    for (_, subscription) in subscriptions.drain() {
                        node.unsubscribe(subscription.handle);
                    }
                    for (_, mut publisher) in publishers.drain() {
                        // SAFETY: made by `advertise` and finished once
                        unsafe { rcl_publisher_fini(&mut *publisher, &mut *node.node) };
                    }
                    wait_set.fini();
                    return;
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        // Send what's waiting once its publisher has a subscriber, or it's waited long enough; in order per topic
        let mut held = std::collections::HashSet::new();
        pending.retain(|message| {
            if held.contains(&message.key) {
                return true;
            }
            let publisher = &publishers[&message.key];
            let heard = subscription_count(publisher) > 0;
            if !heard && message.since.elapsed() < MATCH_WAIT {
                held.insert(message.key.clone());
                return true;
            }
            let topic = &message.key.0;
            if let Err(e) = Node::publish(publisher, &message.bytes) {
                errors.lock().unwrap().push(format!("couldn't publish to {topic}: {e:#}"));
                wake();
            } else if heard {
                unheard.remove(&message.key);
            } else if unheard.insert(message.key.clone()) {
                errors
                    .lock()
                    .unwrap()
                    .push(format!("sent to {topic}, but nothing subscribes to it, so nothing got it"));
                wake();
            }
            false
        });

        if last_graph.is_none_or(|at| at.elapsed() >= GRAPH_EVERY) {
            last_graph = Some(Instant::now());
            let fresh = node.graph();
            let mut shared = graph.lock().unwrap();
            if **shared != fresh {
                *shared = Arc::new(fresh);
                wake();
            }
        }

        if subscriptions.is_empty() {
            std::thread::sleep(WAIT);
            continue;
        }
        let ready = match wait_set.wait(&mut node, subscriptions.values().map(|s| &*s.handle.sub)) {
            Ok(ready) => ready,
            Err(e) => {
                eprintln!("archon: waiting for ROS messages failed: {e:#}");
                std::thread::sleep(WAIT);
                continue;
            }
        };
        for subscription in subscriptions.values_mut() {
            if ready.contains(&(&*subscription.handle.sub as *const rcl_subscription_t)) {
                subscription.take_all();
            }
        }
    }
}

/// The publisher for a topic and type, made the first time
fn publisher_for<'a>(
    publishers: &'a mut HashMap<(String, String), Box<rcl_publisher_t>>,
    types: &mut TypeSupports,
    node: &mut Node,
    topic: &str,
    ty: &str,
) -> anyhow::Result<&'a mut Box<rcl_publisher_t>> {
    match publishers.entry((topic.to_owned(), ty.to_owned())) {
        std::collections::hash_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
        std::collections::hash_map::Entry::Vacant(entry) => {
            types.get(ty).and_then(|ts| node.advertise(topic, ts)).map(|p| entry.insert(p))
        }
    }
}

/// How many subscriptions have matched a publisher
fn subscription_count(publisher: &rcl_publisher_t) -> usize {
    let mut count = 0;
    // SAFETY: the publisher is initialized; rcl writes the count
    let ret = unsafe { rcl_publisher_get_subscription_count(publisher, &mut count) };
    if ret == RCL_RET_OK as i32 { count } else { 0 }
}

struct Subscription {
    handle: SubscriptionHandle,
    callback: Callback,
}

impl Subscription {
    fn take_all(&mut self) {
        loop {
            // SAFETY: the subscription and buffer are initialized and owned by this thread
            let ret = unsafe {
                rcl_take_serialized_message(
                    &*self.handle.sub,
                    &mut *self.handle.buffer,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if ret != RCL_RET_OK as i32 {
                // RCL_RET_SUBSCRIPTION_TAKE_FAILED just means nothing is left
                return;
            }
            let buffer = &*self.handle.buffer;
            // SAFETY: the RMW filled `buffer_length` bytes of `buffer`
            let bytes = unsafe { std::slice::from_raw_parts(buffer.buffer, buffer.buffer_length) };
            (self.callback)(Delivery::Message(bytes));
        }
    }
}

/// Boxed so the pointers rcl keeps to them stay valid
struct SubscriptionHandle {
    sub: Box<rcl_subscription_t>,
    buffer: Box<rcl_serialized_message_t>,
}

struct Node {
    context: Box<rcl_context_t>,
    node: Box<rcl_node_t>,
}

/// The last rcl error, which also clears it
fn rcl_error() -> String {
    // SAFETY: rcutils keeps the error per thread, and these only read and reset it
    unsafe {
        let error = rcutils_get_error_string();
        rcutils_reset_error();
        CStr::from_ptr(error.str_.as_ptr()).to_string_lossy().into_owned()
    }
}

fn check(ret: rcl_ret_t, what: &str) -> anyhow::Result<()> {
    if ret == RCL_RET_OK as i32 { Ok(()) } else { bail!("{what} failed ({ret}): {}", rcl_error()) }
}

/// Matches publishers whatever their QoS: a best-effort, volatile reader is compatible with reliable and
/// best-effort writers alike, and with micro-ROS's best-effort ones
fn any_publisher_qos() -> rmw_qos_profile_t {
    let unspecified = rmw_time_s { sec: 0, nsec: 0 };
    rmw_qos_profile_t {
        history: rmw_qos_history_policy_e::RMW_QOS_POLICY_HISTORY_KEEP_LAST,
        depth: 50,
        reliability: rmw_qos_reliability_policy_e::RMW_QOS_POLICY_RELIABILITY_BEST_EFFORT,
        durability: rmw_qos_durability_policy_e::RMW_QOS_POLICY_DURABILITY_VOLATILE,
        deadline: unspecified,
        lifespan: unspecified,
        liveliness: rmw_qos_liveliness_policy_e::RMW_QOS_POLICY_LIVELINESS_SYSTEM_DEFAULT,
        liveliness_lease_duration: unspecified,
        avoid_ros_namespace_conventions: false,
    }
}

impl Node {
    fn new(options: Options) -> anyhow::Result<Node> {
        // SAFETY: the usual rcl start-up sequence; the context and node are boxed before rcl keeps pointers to them
        unsafe {
            let mut init_options = rcl_get_zero_initialized_init_options();
            check(rcl_init_options_init(&mut init_options, rcutils_get_default_allocator()), "rcl_init_options_init")?;
            if let Some(domain) = options.domain_id {
                check(rcl_init_options_set_domain_id(&mut init_options, domain), "setting the domain")?;
            }
            let mut context = Box::new(rcl_get_zero_initialized_context());
            let ret = rcl_init(0, ptr::null(), &init_options, &mut *context);
            rcl_init_options_fini(&mut init_options);
            check(ret, "rcl_init")?;

            let mut node = Box::new(rcl_get_zero_initialized_node());
            let name = CString::new(format!("archon_{}", std::process::id())).expect("no NUL in a node name");
            let node_options = rcl_node_get_default_options();
            let ret = rcl_node_init(&mut *node, name.as_ptr(), c"".as_ptr(), &mut *context, &node_options);
            if let Err(e) = check(ret, "rcl_node_init") {
                rcl_shutdown(&mut *context);
                rcl_context_fini(&mut *context);
                return Err(e);
            }
            Ok(Node { context, node })
        }
    }

    fn subscribe(
        &mut self,
        topic: &str,
        type_support: *const rosidl_message_type_support_t,
    ) -> anyhow::Result<SubscriptionHandle> {
        let topic_c = CString::new(topic)?;
        // SAFETY: the node is initialized; the subscription and buffer are boxed before rcl sees them
        unsafe {
            let mut sub = Box::new(rcl_get_zero_initialized_subscription());
            let mut options = rcl_subscription_get_default_options();
            options.qos = any_publisher_qos();
            check(
                rcl_subscription_init(&mut *sub, &*self.node, type_support, topic_c.as_ptr(), &options),
                "subscribing",
            )?;
            let mut buffer = Box::new(rcutils_get_zero_initialized_uint8_array());
            let allocator = rcutils_get_default_allocator();
            if let Err(e) = check(rcutils_uint8_array_init(&mut *buffer, 0, &allocator), "allocating a buffer") {
                rcl_subscription_fini(&mut *sub, &mut *self.node);
                return Err(e);
            }
            Ok(SubscriptionHandle { sub, buffer })
        }
    }

    /// A publisher with the default QoS (reliable, volatile), which best-effort subscribers like micro-ROS's
    /// kill switch also accept
    fn advertise(
        &mut self,
        topic: &str,
        type_support: *const rosidl_message_type_support_t,
    ) -> anyhow::Result<Box<rcl_publisher_t>> {
        let topic_c = CString::new(topic)?;
        // SAFETY: the node is initialized; the publisher is boxed before rcl sees it
        unsafe {
            let mut publisher = Box::new(rcl_get_zero_initialized_publisher());
            let options = rcl_publisher_get_default_options();
            check(
                rcl_publisher_init(&mut *publisher, &*self.node, type_support, topic_c.as_ptr(), &options),
                "making a publisher",
            )?;
            Ok(publisher)
        }
    }

    fn publish(publisher: &rcl_publisher_t, bytes: &[u8]) -> anyhow::Result<()> {
        let message = rcl_serialized_message_t {
            buffer: bytes.as_ptr().cast_mut(),
            buffer_length: bytes.len(),
            buffer_capacity: bytes.len(),
            // SAFETY: only read; the RMW doesn't resize a message it's given to publish
            allocator: unsafe { rcutils_get_default_allocator() },
        };
        // SAFETY: the publisher is initialized and the buffer outlives the call
        check(unsafe { rcl_publish_serialized_message(publisher, &message, ptr::null_mut()) }, "publishing")
    }

    fn unsubscribe(&mut self, mut handle: SubscriptionHandle) {
        // SAFETY: both were initialized by `subscribe` and are finished once
        unsafe {
            rcl_subscription_fini(&mut *handle.sub, &mut *self.node);
            rcutils_uint8_array_fini(&mut *handle.buffer);
        }
    }

    fn graph(&mut self) -> Graph {
        let mut topics = Vec::new();
        // SAFETY: the names-and-types struct is zero-initialized, filled by rcl, read, then finished
        unsafe {
            let mut found = rmw_get_zero_initialized_names_and_types();
            let mut allocator = rcutils_get_default_allocator();
            if rcl_get_topic_names_and_types(&*self.node, &mut allocator, false, &mut found) != RCL_RET_OK as i32 {
                rcl_error();
                return Graph::default();
            }
            for i in 0..found.names.size {
                let name = CStr::from_ptr(*found.names.data.add(i)).to_string_lossy().into_owned();
                let types = &*found.types.add(i);
                let types = (0..types.size)
                    .map(|j| CStr::from_ptr(*types.data.add(j)).to_string_lossy().into_owned())
                    .collect();
                let mut publishers = 0;
                let name_c = CString::new(name.clone()).unwrap_or_default();
                if rcl_count_publishers(&*self.node, name_c.as_ptr(), &mut publishers) != RCL_RET_OK as i32 {
                    rcl_error();
                }
                let mut subscribers = 0;
                if rcl_count_subscribers(&*self.node, name_c.as_ptr(), &mut subscribers) != RCL_RET_OK as i32 {
                    rcl_error();
                }
                topics.push(Topic { name, types, publishers, subscribers });
            }
            rcl_names_and_types_fini(&mut found);
        }
        topics.sort_by(|a, b| a.name.cmp(&b.name));
        Graph { topics, nodes: self.nodes() }
    }

    /// Every node but this one, with its publishers, subscribers and services
    fn nodes(&mut self) -> Vec<NodeInfo> {
        let mut nodes = Vec::new();
        // SAFETY: the string arrays and names-and-types are zero-initialized, filled by rcl, read, then finished
        unsafe {
            let mut names = rcutils_get_zero_initialized_string_array();
            let mut namespaces = rcutils_get_zero_initialized_string_array();
            if rcl_get_node_names(&*self.node, rcutils_get_default_allocator(), &mut names, &mut namespaces)
                != RCL_RET_OK as i32
            {
                rcl_error();
                return nodes;
            }
            let own = CStr::from_ptr(rcl_node_get_name(&*self.node)).to_string_lossy().into_owned();
            for i in 0..names.size {
                let (name_c, namespace_c) = (*names.data.add(i), *namespaces.data.add(i));
                let name = CStr::from_ptr(name_c).to_string_lossy().into_owned();
                let namespace = CStr::from_ptr(namespace_c).to_string_lossy().into_owned();
                if name == own || name.is_empty() {
                    continue;
                }
                let read = |found: &mut rcl_names_and_types_t, ret: rcl_ret_t| -> Vec<(String, String)> {
                    if ret != RCL_RET_OK as i32 {
                        rcl_error();
                        return Vec::new();
                    }
                    let list = (0..found.names.size)
                        .map(|j| {
                            let topic = CStr::from_ptr(*found.names.data.add(j)).to_string_lossy().into_owned();
                            let types = &*found.types.add(j);
                            let ty = if types.size > 0 {
                                CStr::from_ptr(*types.data).to_string_lossy().into_owned()
                            } else {
                                String::new()
                            };
                            (topic, ty)
                        })
                        .collect();
                    rcl_names_and_types_fini(found);
                    list
                };
                let mut allocator = rcutils_get_default_allocator();
                let mut found = rmw_get_zero_initialized_names_and_types();
                let ret = rcl_get_publisher_names_and_types_by_node(
                    &*self.node,
                    &mut allocator,
                    false,
                    name_c,
                    namespace_c,
                    &mut found,
                );
                let publishes = read(&mut found, ret);
                let mut found = rmw_get_zero_initialized_names_and_types();
                let ret = rcl_get_subscriber_names_and_types_by_node(
                    &*self.node,
                    &mut allocator,
                    false,
                    name_c,
                    namespace_c,
                    &mut found,
                );
                let subscribes = read(&mut found, ret);
                let mut found = rmw_get_zero_initialized_names_and_types();
                let ret = rcl_get_service_names_and_types_by_node(
                    &*self.node,
                    &mut allocator,
                    name_c,
                    namespace_c,
                    &mut found,
                );
                let services = read(&mut found, ret);
                let full =
                    if namespace.ends_with('/') { format!("{namespace}{name}") } else { format!("{namespace}/{name}") };
                nodes.push(NodeInfo { name: full, publishes, subscribes, services });
            }
            rcutils_string_array_fini(&mut names);
            rcutils_string_array_fini(&mut namespaces);
        }
        nodes.sort_by(|a, b| a.name.cmp(&b.name));
        // A node that's there twice under one name (a restarted board, say) shows once
        nodes.dedup_by(|a, b| a.name == b.name);
        nodes
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        // SAFETY: finished once, node before context, after every subscription
        unsafe {
            rcl_node_fini(&mut *self.node);
            rcl_shutdown(&mut *self.context);
            rcl_context_fini(&mut *self.context);
        }
    }
}

/// A wait set sized for the current subscriptions, remade when their number changes
#[derive(Default)]
struct WaitSet {
    set: Option<(Box<rcl_wait_set_t>, usize)>,
}

impl WaitSet {
    /// Waits for messages on any of `subscriptions`. Returns the ones with messages.
    fn wait<'a>(
        &mut self,
        node: &mut Node,
        subscriptions: impl ExactSizeIterator<Item = &'a rcl_subscription_t>,
    ) -> anyhow::Result<Vec<*const rcl_subscription_t>> {
        let count = subscriptions.len();
        if self.set.as_ref().is_none_or(|(_, size)| *size != count) {
            self.fini();
            // SAFETY: a zero-initialized wait set is initialized once and finished in `fini`
            unsafe {
                let mut set = Box::new(rcl_get_zero_initialized_wait_set());
                let allocator = rcutils_get_default_allocator();
                check(
                    rcl_wait_set_init(&mut *set, count, 0, 0, 0, 0, 0, &mut *node.context, allocator),
                    "rcl_wait_set_init",
                )?;
                self.set = Some((set, count));
            }
        }
        let (set, _) = self.set.as_mut().expect("made above");
        // SAFETY: the wait set holds `count` subscriptions, all initialized and alive for this call
        unsafe {
            check(rcl_wait_set_clear(&mut **set), "rcl_wait_set_clear")?;
            for subscription in subscriptions {
                check(
                    rcl_wait_set_add_subscription(&mut **set, subscription, ptr::null_mut()),
                    "adding to the wait set",
                )?;
            }
            let ret = rcl_wait(&mut **set, WAIT.as_nanos() as i64);
            if ret == RCL_RET_TIMEOUT as i32 {
                return Ok(Vec::new());
            }
            check(ret, "rcl_wait")?;
            // rcl nulls out the subscriptions without messages
            Ok((0..set.size_of_subscriptions).map(|i| *set.subscriptions.add(i)).filter(|s| !s.is_null()).collect())
        }
    }

    fn fini(&mut self) {
        if let Some((mut set, _)) = self.set.take() {
            // SAFETY: initialized in `wait`, finished once
            unsafe { rcl_wait_set_fini(&mut *set) };
        }
    }
}
