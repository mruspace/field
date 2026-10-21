//! `field check-task`: the determinism test of docs/spec.md, acceptance 2.
//! Two instances of the task get the same requests; every request also goes
//! to the first instance twice. All three answers must be byte-identical.
//! A task that reads the clock, a random source or anything but its
//! request fails here, before it is ever supervised.

use crate::config::Config;
use crate::protocol::{Limits, FLAG_SELF_CHECK};
use crate::replica::{ProcessLimits, Replica};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

pub struct Options {
    pub requests: u64,
    pub seed: u64,
    pub inputs: Option<std::path::PathBuf>,
}

fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// Returns the exit code: 0 only with no mismatch and no lost reply.
pub fn run(cfg: &Config, o: &Options) -> i32 {
    let limits = Limits { record_max: cfg.record_max, state_max: cfg.state_max };
    let timeout = Duration::from_millis(cfg.reply_timeout_ms);
    let spawn = || Replica::spawn(&cfg.task, cfg.adapter, ProcessLimits { memory_mb: cfg.memory_mb });
    let (mut a, mut b) = match (spawn(), spawn()) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("field: task {}: {e}", cfg.task.display());
            return 2;
        }
    };
    let inputs = match &o.inputs {
        Some(p) => match read_inputs(p, limits) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("field: inputs {}: {e}", p.display());
                return 2;
            }
        },
        None => None,
    };
    let mut rng = o.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(7);
    let mut state: Vec<u8> = Vec::new();
    let (mut mismatches, mut lost) = (0u64, 0u64);
    let n = inputs.as_ref().map_or(o.requests, |v| v.len() as u64);
    for k in 1..=n {
        let input = match &inputs {
            Some(v) => v[(k - 1) as usize].clone(),
            None if k % 10 == 0 => k.to_le_bytes().to_vec(),
            None => {
                let len = (xorshift(&mut rng) % 256 + 1) as usize;
                (0..len).map(|_| xorshift(&mut rng) as u8).collect()
            }
        };
        let flags = if k % 100 == 0 { FLAG_SELF_CHECK } else { 0 };
        let one = a.ask(k, flags, &input, &state, limits, timeout);
        let two = b.ask(k, flags, &input, &state, limits, timeout);
        let three = a.ask(k, flags, &input, &state, limits, timeout);
        match (one, two, three) {
            (Ok(r1), Ok(r2), Ok(r3)) => {
                let same = r1 == r2 && r1 == r3 && r1.second.as_ref().is_none_or(|s| *s == r1.output);
                if same {
                    state = r1.state;
                } else {
                    mismatches += 1;
                    if mismatches <= 5 {
                        eprintln!("request {k}: answers differ ({} bytes in)", input.len());
                    }
                }
            }
            (r1, r2, r3) => {
                lost += 1;
                if lost <= 5 {
                    for (who, r) in [("a", &r1), ("b", &r2), ("a again", &r3)] {
                        if let Err(e) = r {
                            eprintln!("request {k}: {who}: {e}");
                        }
                    }
                }
                if lost >= 3 {
                    break;
                }
            }
        }
    }
    a.stop(Duration::from_millis(500));
    b.stop(Duration::from_millis(500));
    println!("requests={n} mismatches={mismatches} lost={lost}");
    if mismatches == 0 && lost == 0 {
        0
    } else {
        1
    }
}

/// Length-prefixed records from a file, as the file source reads them.
fn read_inputs(path: &Path, limits: Limits) -> Result<Vec<Vec<u8>>, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut all = Vec::new();
    f.read_to_end(&mut all).map_err(|e| e.to_string())?;
    let mut v = Vec::new();
    let mut at = 0;
    while at + 4 <= all.len() {
        let len = u32::from_le_bytes(all[at..at + 4].try_into().unwrap());
        if len > limits.record_max {
            return Err(format!("record length {len} over record_max"));
        }
        at += 4;
        let end = at + len as usize;
        if end > all.len() {
            return Err("file ends inside a record".into());
        }
        v.push(all[at..end].to_vec());
        at = end;
    }
    Ok(v)
}
