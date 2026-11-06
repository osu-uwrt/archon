//! UWRT's CANmore frames in words, from titan_firmware's `canmore/protocol.h`: who sent a frame, which way it goes,
//! and what it is (a heartbeat, a thruster command, part of a micro-ROS message).

/// Board names by client ID, on the internal bus (the external bus reuses some numbers for the smart batteries)
const CLIENTS: [(u32, &str); 8] = [
    (1, "power board"),
    (2, "ESC board 0"),
    (3, "ESC board 1"),
    (4, "camera cage"),
    (5, "actuator"),
    (6, "LED board"),
    (7, "SBC"),
    (8, "IVC board"),
];

const MODES: [&str; 8] = ["no control", "normal", "bootloader", "linux", "mode 4", "boot delay", "mode 6", "mode 7"];

/// A frame's ID (without the extended flag) and data, in words
pub fn describe(id: u32, extended: bool, data: &[u8]) -> String {
    let field = |offset: u32, len: u32| (id >> offset) & ((1 << len) - 1);
    let (client, utility, to_client, noc) = if extended {
        (field(24, 5), field(23, 1) == 1, field(22, 1) == 1, field(18, 4))
    } else {
        (field(6, 5), field(5, 1) == 1, field(4, 1) == 1, field(0, 4))
    };
    let who = match CLIENTS.iter().find(|(n, _)| *n == client) {
        Some((_, name)) => format!("client {client} ({name})"),
        None => format!("client {client}"),
    };
    let way = if to_client { "← agent" } else { "→ agent" };
    if !utility {
        let part = if extended && noc == 0 {
            let length = field(0, 11);
            let subtype = field(11, 6);
            let single = field(17, 1) == 1;
            format!("{} subtype {subtype}, {length} bytes", if single { "whole message," } else { "message start," })
        } else {
            format!("message part {noc}")
        };
        return format!("{who} {way} {part}");
    }
    let what = match noc {
        15 => heartbeat(data),
        14 => "control interface".to_owned(),
        13 => "remote shell".to_owned(),
        0 if to_client => {
            let commands: Vec<String> =
                data.as_chunks::<2>().0.iter().map(|pair| u16::from_be_bytes(*pair).to_string()).collect();
            format!("thrusters {}", commands.join(" "))
        }
        n => format!("utility channel {n}"),
    };
    format!("{who} {way} {what}")
}

/// `| T | V | M M M | E | C C |`
fn heartbeat(data: &[u8]) -> String {
    let Some(&b) = data.first() else { return "heartbeat (empty)".to_owned() };
    let mode = MODES[usize::from((b >> 3) & 0b111)];
    let state = if b & 0b100 != 0 { "ERROR" } else { "ok" };
    let mut text = format!("heartbeat · {mode} · {state} · #{}", b & 0b11);
    if b & 0x40 != 0 {
        text.push_str(if b & 0x80 != 0 { " · terminated" } else { " · not terminated" });
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A standard ID: client, type, direction, NOC
    fn std_id(client: u32, utility: bool, to_client: bool, noc: u32) -> u32 {
        (client << 6) | (u32::from(utility) << 5) | (u32::from(to_client) << 4) | noc
    }

    #[test]
    fn heartbeats_name_the_board_and_its_state() {
        // Normal mode, ok, count 1, termination valid and on
        let data = [0x80 | 0x40 | (1 << 3) | 1];
        assert_eq!(
            describe(std_id(2, true, false, 15), false, &data),
            "client 2 (ESC board 0) → agent heartbeat · normal · ok · #1 · terminated"
        );
        assert_eq!(
            describe(std_id(8, true, false, 15), false, &[(5 << 3) | 0b100]),
            "client 8 (IVC board) → agent heartbeat · boot delay · ERROR · #0"
        );
    }

    #[test]
    fn thrusters_and_messages() {
        let data = [0x00, 0x30, 0x04, 0x00, 0x00, 0x00, 0x07, 0xd0];
        assert_eq!(
            describe(std_id(3, true, true, 0), false, &data),
            "client 3 (ESC board 1) ← agent thrusters 48 1024 0 2000"
        );
        assert_eq!(describe(std_id(1, false, false, 4), false, &[]), "client 1 (power board) → agent message part 4");
        // Extended: client 5, message, to the agent, NOC 0, single, subtype 2, 40 bytes
        let id = (5 << 24) | (1 << 17) | (2 << 11) | 40;
        assert_eq!(describe(id, true, &[]), "client 5 (actuator) → agent whole message, subtype 2, 40 bytes");
        assert_eq!(describe(std_id(20, true, true, 9), false, &[]), "client 20 ← agent utility channel 9");
    }
}
