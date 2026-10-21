//! A stored fault schedule, for tests and for a pilot that injects its own
//! faults: one fault per line, `kind:replica@cycle`, `#` comments. The
//! faults act on the replica's process or on the bytes it returned, never
//! inside the task, so any task can be put under them.
//!
//! - `kill`: the process gets SIGKILL; the replica is lost on that cycle.
//! - `hang`: the process gets SIGSTOP; it never answers and is lost after
//!   the reply timeout.
//! - `stuck`: from that cycle on, every output of the replica is replaced
//!   by one fixed wrong value (its known-answer test fails too).
//! - `corrupt`: one bit of the output is flipped, once. In self-check mode
//!   only the first computation is hit, so the replica catches it.
//! - `corrupt2`: as `corrupt`, but both computations are hit alike: the
//!   case self-check cannot catch.

use crate::protocol::Response;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Kill,
    Hang,
    Stuck,
    Corrupt,
    Corrupt2,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Kill => "kill",
            Kind::Hang => "hang",
            Kind::Stuck => "stuck",
            Kind::Corrupt => "corrupt",
            Kind::Corrupt2 => "corrupt2",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "kill" => Kind::Kill,
            "hang" => Kind::Hang,
            "stuck" => Kind::Stuck,
            "corrupt" => Kind::Corrupt,
            "corrupt2" => Kind::Corrupt2,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub kind: Kind,
    pub replica: usize,
    pub cycle: u64,
}

impl Fault {
    #[cfg(test)]
    pub fn text(&self) -> String {
        format!("{}:{}@{}", self.kind.name(), self.replica, self.cycle)
    }
}

/// Parse `kind:replica@cycle`.
pub fn parse_line(s: &str) -> Option<Fault> {
    let (kind, rest) = s.split_once(':')?;
    let (replica, cycle) = rest.split_once('@')?;
    let replica: usize = replica.parse().ok()?;
    if replica >= quorum::REPLICAS {
        return None;
    }
    Some(Fault { kind: Kind::parse(kind)?, replica, cycle: cycle.parse().ok()? })
}

/// Parse a schedule's text. Any bad line is an error.
pub fn parse(text: &str) -> Result<Vec<Fault>, String> {
    let mut faults = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f = parse_line(line).ok_or_else(|| {
            format!("line {}: '{line}': expected kind:replica@cycle with kind kill, hang, stuck, corrupt or corrupt2 and replica 0 to 2", n + 1)
        })?;
        faults.push(f);
    }
    faults.sort_by_key(|f| f.cycle);
    Ok(faults)
}

pub fn load(path: &Path) -> Result<(String, Vec<Fault>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let faults = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((text, faults))
}

/// The fixed wrong output of a stuck replica.
pub const STUCK_BYTES: [u8; 8] = [0xef, 0xbe, 0xad, 0xde, 0xef, 0xbe, 0xad, 0xde];

/// What is pending on a replica from the schedule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Injected {
    pub stuck: bool,
    pub corrupt: Option<Kind>,
}

/// Apply the byte faults to a response the replica just gave.
pub fn inject_bytes(inj: &mut Injected, resp: &mut Response) {
    if inj.stuck {
        resp.output = STUCK_BYTES.to_vec();
        if let Some(second) = resp.second.as_mut() {
            *second = STUCK_BYTES.to_vec();
        }
    }
    match inj.corrupt.take() {
        Some(Kind::Corrupt) => flip(&mut resp.output),
        Some(Kind::Corrupt2) => {
            flip(&mut resp.output);
            if let Some(second) = resp.second.as_mut() {
                flip(second);
            }
        }
        _ => {}
    }
}

fn flip(bytes: &mut Vec<u8>) {
    match bytes.first_mut() {
        Some(b) => *b ^= 1,
        None => bytes.push(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_parse_and_sort() {
        let f = parse("# c\n\nstuck:1@120\nkill:0@5\n corrupt2:2@30 \nhang:2@60\ncorrupt:2@30\n").unwrap();
        let text: Vec<String> = f.iter().map(Fault::text).collect();
        assert_eq!(text, ["kill:0@5", "corrupt2:2@30", "corrupt:2@30", "hang:2@60", "stuck:1@120"]);
    }

    #[test]
    fn bad_lines_are_refused() {
        for bad in ["kill:3@1", "burn:0@1", "kill:0", "kill@1", "kill:a@1", "kill:0@x", "stuckbit:0@1/out.0.0"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn byte_faults() {
        let resp = || Response { output: vec![0x10, 0x20], state: vec![9], second: Some(vec![0x10, 0x20]) };
        let mut inj = Injected { stuck: true, corrupt: None };
        let mut r = resp();
        inject_bytes(&mut inj, &mut r);
        assert_eq!(r.output, STUCK_BYTES);
        assert_eq!(r.second.unwrap(), STUCK_BYTES);
        assert!(inj.stuck, "stuck is persistent");

        let mut inj = Injected { stuck: false, corrupt: Some(Kind::Corrupt) };
        let mut r = resp();
        inject_bytes(&mut inj, &mut r);
        assert_eq!(r.output, [0x11, 0x20]);
        assert_eq!(r.second.unwrap(), [0x10, 0x20]);
        assert_eq!(inj.corrupt, None, "corrupt is consumed");

        let mut inj = Injected { stuck: false, corrupt: Some(Kind::Corrupt2) };
        let mut r = resp();
        inject_bytes(&mut inj, &mut r);
        assert_eq!(r.output, [0x11, 0x20]);
        assert_eq!(r.second.unwrap(), [0x11, 0x20]);

        let mut inj = Injected { stuck: false, corrupt: Some(Kind::Corrupt) };
        let mut r = Response { output: vec![], state: vec![], second: None };
        inject_bytes(&mut inj, &mut r);
        assert_eq!(r.output, [1], "an empty output becomes one byte");
    }
}
