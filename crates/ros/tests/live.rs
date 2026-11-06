//! Against a real ROS graph: `ros2 topic pub` publishes, archon's client finds the topic, subscribes and decodes.
//! Uses its own domain on localhost only, so it can't see or disturb a robot on the network. Needs the ROS
//! environment (`nix develop`).

use std::process::{Child, Command};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use archon_core::source::{Envelope, Event, Sink, Source, Status};
use archon_msg::Registry;
use archon_ros::{Delivery, EchoSource, Options, Ros};

const DOMAIN: usize = 77;

/// A publisher that's killed, with everything it started, when dropped
struct Publisher(Child);

impl Publisher {
    fn start(topic: &str, ty: &str, value: &str) -> Publisher {
        use std::os::unix::process::CommandExt;
        let child = Command::new("ros2")
            .args(["topic", "pub", "-r", "20", topic, ty, value])
            .env("ROS_DOMAIN_ID", DOMAIN.to_string())
            .env("ROS_LOCALHOST_ONLY", "1")
            .stdout(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .expect("ros2 is on the PATH in the ROS environment");
        Publisher(child)
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        // SAFETY: signals the publisher's own process group
        unsafe { libc::killpg(self.0.id() as i32, libc::SIGINT) };
        let _ = self.0.wait();
    }
}

fn wait_for<T>(what: &str, timeout: Duration, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn echoes_a_live_topic() {
    if std::env::var_os("AMENT_PREFIX_PATH").is_none() {
        eprintln!("skipped: no ROS environment");
        return;
    }
    // SAFETY: set before any thread of this test process reads the environment
    unsafe { std::env::set_var("ROS_LOCALHOST_ONLY", "1") };
    let topic = format!("/archon_live_{}", std::process::id());
    let _publisher = Publisher::start(&topic, "std_msgs/msg/String", "{data: hello archon}");
    let ros = Arc::new(Ros::start(Options { domain_id: Some(DOMAIN) }, Arc::new(|| {})).unwrap());

    // The graph shows the topic and its type
    let found = wait_for("the topic on the graph", Duration::from_secs(20), || ros.graph().topic(&topic).cloned());
    assert_eq!(found.types, ["std_msgs/msg/String"]);

    // An echo finds the type itself, subscribes, and decodes
    let registry = Arc::new(Mutex::new(Registry::from_env()));
    let mut echo = EchoSource::new(ros.clone(), registry, &topic, None);
    let (tx, rx) = mpsc::channel::<Envelope>();
    echo.start(Sink::new(0, 1, tx, Arc::new(|| {})));
    let mut output = String::new();
    let mut statuses = Vec::new();
    wait_for("a decoded message", Duration::from_secs(20), || {
        for envelope in rx.try_iter() {
            match envelope.event {
                Event::Output(text) => output.push_str(&text),
                Event::Status(status) => statuses.push(status),
                _ => {}
            }
        }
        output.contains("data: hello archon\n---\n").then_some(())
    });
    assert!(statuses.contains(&Status::Running), "{statuses:?}");
    wait_for("a message rate", Duration::from_secs(5), || {
        echo.detail().filter(|d| d.contains("Hz") && d.contains("1 publisher"))
    });

    echo.stop();
    wait_for("the echo to stop", Duration::from_secs(5), || {
        rx.try_iter().any(|e| matches!(e.event, Event::Status(Status::Stopped))).then_some(())
    });

    // A type that isn't installed fails with a reason instead of hanging
    let (failed_tx, failed_rx) = mpsc::channel();
    ros.subscribe(
        "/nothing",
        "no_such_pkg/msg/Thing",
        Box::new(move |delivery| {
            if let Delivery::Failed(why) = delivery {
                let _ = failed_tx.send(why);
            }
        }),
    );
    let why = failed_rx.recv_timeout(Duration::from_secs(5)).expect("a failure");
    assert!(why.contains("libno_such_pkg__rosidl_typesupport_c.so"), "{why}");
}
