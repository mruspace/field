//! The run's state on disk, to resume after a reset or a power cut. Written
//! every N cycles and on every change of which replicas are alive. The write
//! is atomic (write, sync, rename, sync the directory), so a cut leaves the
//! old file or the new one. One `key=value` per line, in a fixed order.

use crate::faults::Kind;
use crate::log::Streak;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

const REPLICAS: usize = quorum::REPLICAS;
const STRIKES: usize = quorum::STRIKES;

/// A scheduled fault the supervisor reacted to: when it was injected, when
/// it was caught, and how (lost, outvoted, strikes, diagnose, periodic).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caught {
    pub kind: Kind,
    pub replica: usize,
    pub at: u64,
    pub caught: u64,
    pub how: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Checkpoint {
    /// The configuration's identity (config::identity).
    pub config: u64,
    /// Last cycle completed.
    pub cycle: u64,
    pub delivered: u64,
    pub detected: u64,
    pub wrong: u64,
    pub last_delivered: u64,
    pub kat_tests: u64,
    pub alive: [bool; REPLICAS],
    pub strikes: [[u64; STRIKES]; REPLICAS],
    pub strike_count: [usize; REPLICAS],
    /// Unix seconds when the slot was retired, for the cool-down.
    pub retired_at: [Option<u64>; REPLICAS],
    /// Cycle of a scheduled fault not yet reacted to.
    pub fault_at: [Option<u64>; REPLICAS],
    pub faults_injected: u64,
    pub caught: Vec<Caught>,
    /// The state record from the last delivered cycle.
    pub state: Vec<u8>,
    /// Bytes of events.csv that belong to this checkpoint.
    pub log_len: u64,
    /// Bytes of a file output sink that belong to this checkpoint.
    pub out_len: u64,
    pub streak: Option<Streak>,
    pub resumes: u64,
    pub halted: bool,
    pub started_unix: u64,
}

impl Checkpoint {
    pub fn fresh(config: u64, now_unix: u64) -> Checkpoint {
        Checkpoint {
            config,
            cycle: 0,
            delivered: 0,
            detected: 0,
            wrong: 0,
            last_delivered: 0,
            kat_tests: 0,
            alive: [true; REPLICAS],
            strikes: [[0; STRIKES]; REPLICAS],
            strike_count: [0; REPLICAS],
            retired_at: [None; REPLICAS],
            fault_at: [None; REPLICAS],
            faults_injected: 0,
            caught: Vec::new(),
            state: Vec::new(),
            log_len: 0,
            out_len: 0,
            streak: None,
            resumes: 0,
            halted: false,
            started_unix: now_unix,
        }
    }

    pub fn to_text(&self) -> String {
        let list = |v: &[bool]| v.iter().map(|b| if *b { "1" } else { "0" }).collect::<Vec<_>>().join(",");
        let opt =
            |v: &[Option<u64>]| v.iter().map(|x| x.map_or("-".into(), |t| t.to_string())).collect::<Vec<_>>().join(",");
        let mut s = String::new();
        s.push_str("field=1\n");
        s.push_str(&format!("config={:016x}\n", self.config));
        s.push_str(&format!("cycle={}\n", self.cycle));
        s.push_str(&format!("delivered={}\n", self.delivered));
        s.push_str(&format!("detected={}\n", self.detected));
        s.push_str(&format!("wrong={}\n", self.wrong));
        s.push_str(&format!("last_delivered={}\n", self.last_delivered));
        s.push_str(&format!("kat_tests={}\n", self.kat_tests));
        s.push_str(&format!("alive={}\n", list(&self.alive)));
        let strikes: Vec<String> = self.strikes.iter().flatten().map(u64::to_string).collect();
        s.push_str(&format!("strikes={}\n", strikes.join(",")));
        let counts: Vec<String> = self.strike_count.iter().map(usize::to_string).collect();
        s.push_str(&format!("strike_count={}\n", counts.join(",")));
        s.push_str(&format!("retired_at={}\n", opt(&self.retired_at)));
        s.push_str(&format!("fault_at={}\n", opt(&self.fault_at)));
        s.push_str(&format!("faults_injected={}\n", self.faults_injected));
        let caught: Vec<String> = self
            .caught
            .iter()
            .map(|c| format!("{}:{}@{}:{}:{}", c.kind.name(), c.replica, c.at, c.caught, c.how))
            .collect();
        s.push_str(&format!("caught={}\n", if caught.is_empty() { "-".to_string() } else { caught.join(",") }));
        s.push_str(&format!("state={}\n", hex(&self.state)));
        s.push_str(&format!("log_len={}\n", self.log_len));
        s.push_str(&format!("out_len={}\n", self.out_len));
        s.push_str(&format!(
            "streak={}\n",
            self.streak.as_ref().map_or("-".into(), |(w, d, first, last, n)| format!("{first}|{last}|{n}|{w}|{d}"))
        ));
        s.push_str(&format!("resumes={}\n", self.resumes));
        s.push_str(&format!("halted={}\n", u8::from(self.halted)));
        s.push_str(&format!("started_unix={}\n", self.started_unix));
        s
    }

    /// Parse a checkpoint; `None` for anything that is not one of this format.
    pub fn from_text(text: &str) -> Option<Checkpoint> {
        let get = |key: &str| -> Option<&str> {
            text.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        };
        if get("field")? != "1" {
            return None;
        }
        let num = |s: &str| s.parse::<u64>().ok();
        let bools = |s: &str| -> Option<[bool; REPLICAS]> {
            let v: Vec<bool> = s.split(',').map(|x| x == "1").collect();
            v.try_into().ok()
        };
        let opts = |s: &str| -> Option<[Option<u64>; REPLICAS]> {
            let v: Option<Vec<Option<u64>>> =
                s.split(',').map(|x| if x == "-" { Some(None) } else { num(x).map(Some) }).collect();
            v?.try_into().ok()
        };
        let strikes: Vec<u64> = get("strikes")?.split(',').map(num).collect::<Option<_>>()?;
        if strikes.len() != REPLICAS * STRIKES {
            return None;
        }
        let mut sk = [[0u64; STRIKES]; REPLICAS];
        for (i, v) in strikes.iter().enumerate() {
            sk[i / STRIKES][i % STRIKES] = *v;
        }
        let counts: Vec<usize> = get("strike_count")?.split(',').map(|x| x.parse().ok()).collect::<Option<_>>()?;
        let caught = match get("caught")? {
            "-" => Vec::new(),
            s => s
                .split(',')
                .map(|c| {
                    let mut p = c.splitn(5, ':');
                    let kind = crate::faults::parse_line(&format!("{}:{}", p.next()?, p.next()?))?;
                    Some(Caught {
                        kind: kind.kind,
                        replica: kind.replica,
                        at: kind.cycle,
                        caught: num(p.next()?)?,
                        how: p.next()?.to_string(),
                    })
                })
                .collect::<Option<_>>()?,
        };
        let streak = match get("streak")? {
            "-" => None,
            s => {
                let mut p = s.splitn(5, '|');
                let (first, last, n) = (num(p.next()?)?, num(p.next()?)?, num(p.next()?)?);
                Some((p.next()?.to_string(), p.next()?.to_string(), first, last, n))
            }
        };
        Some(Checkpoint {
            config: u64::from_str_radix(get("config")?, 16).ok()?,
            cycle: num(get("cycle")?)?,
            delivered: num(get("delivered")?)?,
            detected: num(get("detected")?)?,
            wrong: num(get("wrong")?)?,
            last_delivered: num(get("last_delivered")?)?,
            kat_tests: num(get("kat_tests")?)?,
            alive: bools(get("alive")?)?,
            strikes: sk,
            strike_count: counts.try_into().ok()?,
            retired_at: opts(get("retired_at")?)?,
            fault_at: opts(get("fault_at")?)?,
            faults_injected: num(get("faults_injected")?)?,
            caught,
            state: unhex(get("state")?)?,
            log_len: num(get("log_len")?)?,
            out_len: num(get("out_len")?)?,
            streak,
            resumes: num(get("resumes")?)?,
            halted: get("halted")? == "1",
            started_unix: num(get("started_unix")?)?,
        })
    }

    /// Write atomically: a temporary file, synced, renamed into place, and
    /// the directory synced so the rename itself is durable.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension("tmp");
        let mut f = File::create(&tmp)?;
        f.write_all(self.to_text().as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        File::open(dir)?.sync_all()?;
        Ok(())
    }

    #[cfg(test)]
    pub fn load(path: &Path) -> Option<Checkpoint> {
        Checkpoint::from_text(&std::fs::read_to_string(path).ok()?)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() & 1 == 1 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Checkpoint {
        let mut c = Checkpoint::fresh(0x9f3c0a1e55b2c7d4, 1760990000);
        c.cycle = 12000;
        c.delivered = 11990;
        c.detected = 10;
        c.last_delivered = 12000;
        c.kat_tests = 7;
        c.alive = [true, true, false];
        c.strikes[2] = [120, 121, 122];
        c.strike_count[2] = 3;
        c.retired_at[2] = Some(1761040000);
        c.faults_injected = 3;
        c.caught = vec![
            Caught { kind: Kind::Stuck, replica: 1, at: 120, caught: 120, how: "diagnose".into() },
            Caught { kind: Kind::Hang, replica: 2, at: 60, caught: 60, how: "lost".into() },
        ];
        c.state = vec![0x0a, 0x1b, 0x2c];
        c.log_len = 4821;
        c.out_len = 240000;
        c.streak = Some(("progress".into(), "delivered=11990 detected=10 alive=2".into(), 12000, 12000, 1));
        c.resumes = 2;
        c
    }

    #[test]
    fn text_round_trip() {
        let c = sample();
        let text = c.to_text();
        assert!(text.starts_with("field=1\nconfig=9f3c0a1e55b2c7d4\ncycle=12000\n"));
        assert!(text.contains("\ncaught=stuck:1@120:120:diagnose,hang:2@60:60:lost\n"));
        assert!(text.contains("\nstreak=12000|12000|1|progress|delivered=11990 detected=10 alive=2\n"));
        assert_eq!(Checkpoint::from_text(&text).unwrap(), c);
        let fresh = Checkpoint::fresh(1, 2);
        assert_eq!(Checkpoint::from_text(&fresh.to_text()).unwrap(), fresh);
        let mut big = sample();
        big.state = vec![0xee; 65536];
        big.halted = true;
        assert_eq!(Checkpoint::from_text(&big.to_text()).unwrap(), big);
    }

    #[test]
    fn other_formats_are_not_checkpoints() {
        assert!(Checkpoint::from_text("").is_none());
        assert!(Checkpoint::from_text("run=other\ntick=5\n").is_none());
        assert!(Checkpoint::from_text(&sample().to_text().replace("field=1", "field=2")).is_none());
        assert!(Checkpoint::from_text(&sample().to_text().replace("\ncycle=12000", "")).is_none());
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("field-cp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("checkpoint");
        sample().save(&path).unwrap();
        assert_eq!(Checkpoint::load(&path).unwrap(), sample());
        assert!(!dir.join("checkpoint.tmp").exists());
        let mut later = sample();
        later.cycle = 12100;
        later.save(&path).unwrap();
        assert_eq!(Checkpoint::load(&path).unwrap().cycle, 12100);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
