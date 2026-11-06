//! A SocketCAN bus as a source, like `candump`: one line per frame. It waits for the interface and comes back when
//! it does, and input in `cansend` syntax (`123#DEADBEEF`) sends a frame.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use archon_core::line::Level;
use archon_core::profile::CanSpec;
use archon_core::source::{Sink, Source, SourceKind, Status};

const RETRY: Duration = Duration::from_millis(500);
/// How long a read waits, which bounds how long a stop takes
const READ_TIMEOUT: Duration = Duration::from_millis(100);

const CAN_EFF_FLAG: u32 = 0x8000_0000;
const CAN_RTR_FLAG: u32 = 0x4000_0000;
const CAN_ERR_FLAG: u32 = 0x2000_0000;
const CAN_EFF_MASK: u32 = 0x1fff_ffff;
const CAN_SFF_MASK: u32 = 0x7ff;
/// `struct can_frame`: id, length, three bytes of padding, eight of data
const CAN_MTU: usize = 16;
/// `struct canfd_frame`: the same head, then 64 bytes of data
const CANFD_MTU: usize = 72;

pub struct CanSource {
    spec: CanSpec,
    run: Option<Run>,
}

struct Run {
    stop: Arc<AtomicBool>,
    /// The socket while the interface is up, for sending
    socket: Arc<Mutex<Option<i32>>>,
}

impl CanSource {
    pub fn new(spec: CanSpec) -> Self {
        Self { spec, run: None }
    }
}

impl Source for CanSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Can
    }

    fn describe(&self) -> String {
        self.spec.interface.clone()
    }

    fn start(&mut self, sink: Sink) {
        let run = Run { stop: Arc::default(), socket: Arc::default() };
        let (spec, stop, socket) = (self.spec.clone(), run.stop.clone(), run.socket.clone());
        thread::spawn(move || read_bus(&spec, &sink, &stop, &socket));
        self.run = Some(run);
    }

    fn stop(&mut self) {
        if let Some(run) = &self.run {
            run.stop.store(true, Ordering::SeqCst);
        }
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let text = String::from_utf8_lossy(data);
        let frame = parse_cansend(text.trim()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let socket = self.run.as_ref().and_then(|r| *r.socket.lock().unwrap());
        let socket = socket.ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "the interface is down"))?;
        // SAFETY: writing a buffer of the length given to an open socket
        let written = unsafe { libc::write(socket, frame.as_ptr().cast(), frame.len()) };
        if written < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }
}

fn read_bus(spec: &CanSpec, sink: &Sink, stop: &AtomicBool, shared: &Mutex<Option<i32>>) {
    let mut said_waiting = false;
    while !stop.load(Ordering::SeqCst) {
        let socket = match open(&spec.interface) {
            Ok(socket) => socket,
            Err(e) => {
                if !said_waiting {
                    sink.note(format!("waiting for {}: {e}", spec.interface), Level::Info);
                    sink.status(Status::Waiting(e.to_string()));
                    said_waiting = true;
                }
                sleep_unless(stop, RETRY);
                continue;
            }
        };
        said_waiting = false;
        *shared.lock().unwrap() = Some(socket);
        sink.note(format!("listening on {}", spec.interface), Level::Info);
        sink.status(Status::Running);

        let mut buf = [0u8; CANFD_MTU];
        let lost = loop {
            if stop.load(Ordering::SeqCst) {
                break None;
            }
            // SAFETY: reading into a buffer of its own length
            let n = unsafe { libc::read(socket, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let error = io::Error::last_os_error();
                match error.kind() {
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted => continue,
                    _ => break Some(error.to_string()),
                }
            }
            if !sink.output(format_frame(&spec.interface, &buf[..n as usize], spec.canmore)) {
                break None;
            }
        };
        *shared.lock().unwrap() = None;
        // SAFETY: the socket is ours and closed once
        unsafe { libc::close(socket) };
        if let Some(why) = lost {
            sink.note(format!("lost {}: {why}", spec.interface), Level::Warn);
            sink.status(Status::Waiting(why));
            said_waiting = true;
            sleep_unless(stop, RETRY);
        }
    }
    sink.note("closed", Level::Info);
    sink.status(Status::Stopped);
}

fn open(interface: &str) -> io::Result<i32> {
    let name =
        std::ffi::CString::new(interface).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad name"))?;
    // SAFETY: a name lookup on a NUL-terminated string
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    if index == 0 {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no such interface"));
    }
    // SAFETY: plain socket calls; the socket is closed on every failure after it's made
    unsafe {
        let socket = libc::socket(libc::AF_CAN, libc::SOCK_RAW, libc::CAN_RAW);
        if socket < 0 {
            return Err(io::Error::last_os_error());
        }
        let fail = |socket| {
            let error = io::Error::last_os_error();
            libc::close(socket);
            Err(error)
        };
        // Take CAN FD frames too, where the bus has them
        let on: libc::c_int = 1;
        libc::setsockopt(
            socket,
            libc::SOL_CAN_RAW,
            libc::CAN_RAW_FD_FRAMES,
            (&on as *const libc::c_int).cast(),
            size_of::<libc::c_int>() as u32,
        );
        let timeout = libc::timeval { tv_sec: 0, tv_usec: READ_TIMEOUT.as_micros() as libc::suseconds_t };
        if libc::setsockopt(
            socket,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&timeout as *const libc::timeval).cast(),
            size_of::<libc::timeval>() as u32,
        ) < 0
        {
            return fail(socket);
        }
        let mut address: libc::sockaddr_can = std::mem::zeroed();
        address.can_family = libc::AF_CAN as libc::sa_family_t;
        address.can_ifindex = index as libc::c_int;
        if libc::bind(socket, (&address as *const libc::sockaddr_can).cast(), size_of::<libc::sockaddr_can>() as u32)
            < 0
        {
            return fail(socket);
        }
        Ok(socket)
    }
}

fn sleep_unless(stop: &AtomicBool, duration: Duration) {
    let end = std::time::Instant::now() + duration;
    while std::time::Instant::now() < end && !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(20));
    }
}

/// A frame as `candump` prints it: `  can0  123   [3]  01 02 03`, and with `canmore`, what it is after a `·`
pub fn format_frame(interface: &str, frame: &[u8], canmore: bool) -> String {
    if frame.len() < 8 {
        return format!("  {interface}  (a {}-byte read, not a frame)\n", frame.len());
    }
    let id = u32::from_ne_bytes(frame[..4].try_into().expect("four bytes"));
    let len = usize::from(frame[4]).min(frame.len() - 8);
    let data = &frame[8..8 + len];
    let id_text = if id & CAN_EFF_FLAG != 0 {
        format!("{:08X}", id & CAN_EFF_MASK)
    } else {
        format!("{:>3X}  ", id & CAN_SFF_MASK)
    };
    let fd = frame.len() == CANFD_MTU;
    let length = if fd { format!("[{len:02}]") } else { format!(" [{len}]") };
    let body = if id & CAN_ERR_FLAG != 0 {
        "ERRORFRAME".to_owned()
    } else if id & CAN_RTR_FLAG != 0 {
        "remote request".to_owned()
    } else {
        data.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
    };
    let meaning = if canmore && id & (CAN_ERR_FLAG | CAN_RTR_FLAG) == 0 {
        let extended = id & CAN_EFF_FLAG != 0;
        let bare = id & if extended { CAN_EFF_MASK } else { CAN_SFF_MASK };
        format!("   · {}", crate::canmore::describe(bare, extended, data))
    } else {
        String::new()
    };
    format!("  {interface}  {id_text} {length}  {body}{meaning}\n")
}

/// Parses `cansend` syntax: `123#DEADBEEF` (standard id), `12345678#00` (extended), `123#R` (remote request)
pub fn parse_cansend(text: &str) -> Result<Vec<u8>, String> {
    let (id, data) = text.split_once('#').ok_or("frames look like 123#DEADBEEF")?;
    let mut can_id = u32::from_str_radix(id, 16).map_err(|_| format!("{id:?} isn't a hex id"))?;
    if id.len() > 3 {
        if can_id > CAN_EFF_MASK {
            return Err("an extended id is at most 1FFFFFFF".into());
        }
        can_id |= CAN_EFF_FLAG;
    } else if can_id > CAN_SFF_MASK {
        return Err("a standard id is at most 7FF; use 8 digits for an extended one".into());
    }
    let mut frame = vec![0u8; CAN_MTU];
    if data.eq_ignore_ascii_case("r") {
        can_id |= CAN_RTR_FLAG;
    } else {
        let hex: String = data.chars().filter(|c| *c != '.').collect();
        if !hex.len().is_multiple_of(2) || hex.len() > 16 {
            return Err("data is up to 8 bytes as pairs of hex digits".into());
        }
        for (i, pair) in hex.as_bytes().chunks(2).enumerate() {
            let pair = std::str::from_utf8(pair).expect("ASCII");
            frame[8 + i] = u8::from_str_radix(pair, 16).map_err(|_| format!("{pair:?} isn't hex"))?;
        }
        frame[4] = (hex.len() / 2) as u8;
    }
    frame[..4].copy_from_slice(&can_id.to_ne_bytes());
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_candump() {
        let frame = parse_cansend("123#DEADBEEF").unwrap();
        assert_eq!(format_frame("can0", &frame, false), "  can0  123    [4]  DE AD BE EF\n");
        let frame = parse_cansend("1F334455#11.22").unwrap();
        assert_eq!(format_frame("can0", &frame, false), "  can0  1F334455  [2]  11 22\n");
        let frame = parse_cansend("7FF#R").unwrap();
        assert_eq!(format_frame("vcan0", &frame, false), "  vcan0  7FF    [0]  remote request\n");
    }

    #[test]
    fn says_what_a_canmore_frame_is() {
        // Client 2's heartbeat: utility, to the agent, channel 15
        let frame = parse_cansend("0AF#09").unwrap();
        assert_eq!(
            format_frame("can0", &frame, true),
            "  can0   AF    [1]  09   · client 2 (ESC board 0) → agent heartbeat · normal · ok · #1\n"
        );
    }

    #[test]
    fn rejects_bad_frames() {
        assert!(parse_cansend("800#00").is_err(), "standard ids stop at 7FF");
        assert!(parse_cansend("123#ABC").is_err(), "odd hex");
        assert!(parse_cansend("123#00112233445566778899").is_err(), "more than 8 bytes");
        assert!(parse_cansend("hello").is_err());
    }

    /// Whatever is on vcan0 for three seconds, said in CANmore terms: `cargo test -p archon-sources reads_canmore
    /// -- --ignored --nocapture` while a simulated robot's bus is on vcan0
    #[test]
    #[ignore]
    fn reads_canmore_off_vcan0() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut source =
            CanSource::new(CanSpec { name: "bus".into(), interface: "vcan0".into(), autostart: true, canmore: true });
        source.start(Sink::new(0, 1, tx, Arc::new(|| {})));
        let end = std::time::Instant::now() + Duration::from_secs(3);
        let mut seen = std::collections::BTreeMap::new();
        while std::time::Instant::now() < end {
            if let Ok(archon_core::source::Event::Output(text)) =
                rx.recv_timeout(Duration::from_millis(100)).map(|e| e.event)
            {
                let meaning =
                    text.split('·').nth(1).map(|m| m.split(" heartbeat").next().unwrap_or(m).trim().to_owned());
                *seen.entry(meaning.unwrap_or_default()).or_insert(0) += 1;
                if seen.len() < 12 && seen.values().sum::<i32>() < 40 {
                    print!("{text}");
                }
            }
        }
        source.stop();
        println!("{seen:#?}");
        assert!(!seen.is_empty(), "nothing on vcan0");
    }

    /// Against a real bus, when there's a vcan0 (`ip link add vcan0 type vcan && ip link set vcan0 up`)
    #[test]
    fn talks_to_vcan0_if_there_is_one() {
        if open("vcan0").is_err() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let mut source =
            CanSource::new(CanSpec { name: "bus".into(), interface: "vcan0".into(), autostart: true, canmore: false });
        source.start(Sink::new(0, 1, tx, Arc::new(|| {})));
        std::thread::sleep(Duration::from_millis(300));
        // A second socket sends, the source hears it
        let sender = open("vcan0").unwrap();
        let frame = parse_cansend("321#0102").unwrap();
        // SAFETY: writing a whole frame to an open socket
        unsafe { libc::write(sender, frame.as_ptr().cast(), frame.len()) };
        let heard = (0..20).find_map(|_| match rx.recv_timeout(Duration::from_millis(100)).map(|e| e.event) {
            Ok(archon_core::source::Event::Output(text)) => Some(text),
            _ => None,
        });
        source.stop();
        // SAFETY: closing our own socket
        unsafe { libc::close(sender) };
        assert_eq!(heard.as_deref(), Some("  vcan0  321    [2]  01 02\n"));
    }
}
