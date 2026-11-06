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
use crate::{Callback, Command, Delivery, Graph, Options, SubscriptionId, Topic};

/// How long a wait for messages lasts, which bounds how late a command is handled
const WAIT: Duration = Duration::from_millis(20);
const GRAPH_EVERY: Duration = Duration::from_millis(1000);

pub fn run(
    options: Options,
    commands: Receiver<Command>,
    graph: Arc<Mutex<Arc<Graph>>>,
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
                Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => {
                    for (_, subscription) in subscriptions.drain() {
                        node.unsubscribe(subscription.handle);
                    }
                    wait_set.fini();
                    return;
                }
                Err(TryRecvError::Empty) => break,
            }
        }

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
                topics.push(Topic { name, types, publishers });
            }
            rcl_names_and_types_fini(&mut found);
        }
        topics.sort_by(|a, b| a.name.cmp(&b.name));
        Graph { topics }
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
