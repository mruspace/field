//! field-demo-task: the task the Mru Field supervisor ships with, for the
//! bench, the scenario checks and a self-contained pilot. It is the flight
//! demo's payload (mruspace/flight, demo/src/main.rs): fill a 4 KiB block of
//! working memory from the input and hash it. The output is the 8-byte hash;
//! the state is an 8-byte count of the requests the task has answered.
//!
//! Modes: with no arguments it answers requests on its standard input until
//! end of input (the server protocol in docs/spec.md, section 3). `--once`
//! reads one input record to end of input and writes the output, for the
//! `cli` adapter. `--kat DIR` writes `kat.in` and `kat.out`. Test switches:
//! `--nondeterministic` appends the process id to every output;
//! `--big-state` returns a 100 KiB state record.

use std::io::{Read, Write};

#[path = "../../src/protocol.rs"]
#[allow(dead_code)]
mod protocol;

const BLOCK_WORDS: usize = 512;

fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// Fill the working block for `tick` and hash it (the flight demo's
/// `compute`, without its fault parameters).
fn compute(block: &mut [u64; BLOCK_WORDS], tick: u64) -> u64 {
    let mut s = tick.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    for w in block.iter_mut() {
        *w = xorshift(&mut s);
    }
    hash(block)
}

/// FNV-1a over the block.
#[inline(always)]
fn hash(block: &[u64; BLOCK_WORDS]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for w in block.iter() {
        for b in 0..8 {
            h ^= (w >> (8 * b)) & 0xff;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// The tick an input record stands for: the record itself when it is 8
/// bytes (the supervisor's `tick` source), else a hash of it.
fn tick_of(input: &[u8]) -> u64 {
    match <[u8; 8]>::try_from(input) {
        Ok(b) => u64::from_le_bytes(b),
        Err(_) => {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in input {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
            h
        }
    }
}

struct Switches {
    nondeterministic: bool,
    big_state: bool,
}

fn answer(block: &mut [u64; BLOCK_WORDS], input: &[u8], sw: &Switches) -> Vec<u8> {
    let mut out = compute(block, tick_of(input)).to_le_bytes().to_vec();
    if sw.nondeterministic {
        out.extend_from_slice(&std::process::id().to_le_bytes());
    }
    out
}

fn next_state(state: &[u8], sw: &Switches) -> Vec<u8> {
    let n = <[u8; 8]>::try_from(&state[..state.len().min(8)]).map(u64::from_le_bytes).unwrap_or(0);
    let mut s = (n + 1).to_le_bytes().to_vec();
    if sw.big_state {
        s.resize(100 * 1024, 0xa5);
    }
    s
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sw = Switches {
        nondeterministic: args.iter().any(|a| a == "--nondeterministic"),
        big_state: args.iter().any(|a| a == "--big-state"),
    };
    let mut block = Box::new([0u64; BLOCK_WORDS]);
    let mut stdout = std::io::stdout().lock();

    if let Some(i) = args.iter().position(|a| a == "--kat") {
        let dir = args.get(i + 1).map(String::as_str).unwrap_or(".");
        let input = 0u64.to_le_bytes();
        std::fs::write(format!("{dir}/kat.in"), input).expect("kat.in");
        std::fs::write(format!("{dir}/kat.out"), compute(&mut block, 0).to_le_bytes()).expect("kat.out");
        return;
    }
    if args.iter().any(|a| a == "--once") {
        let mut input = Vec::new();
        std::io::stdin().lock().read_to_end(&mut input).expect("input");
        stdout.write_all(&answer(&mut block, &input, &sw)).expect("output");
        return;
    }

    // the record limits are generous: the supervisor enforces its own
    let limits = protocol::Limits { record_max: 1 << 24, state_max: 1 << 24 };
    let mut stdin = std::io::stdin().lock();
    let mut buf = Vec::new();
    while let Ok(Some(req)) = protocol::read_request(&mut stdin, limits) {
        let output = answer(&mut block, &req.input, &sw);
        let second = (req.flags & protocol::FLAG_SELF_CHECK != 0).then(|| answer(&mut block, &req.input, &sw));
        let resp = protocol::Response { output, state: next_state(&req.state, &sw), second };
        buf.clear();
        protocol::encode_response(&resp, &mut buf);
        if stdout.write_all(&buf).and_then(|_| stdout.flush()).is_err() {
            break;
        }
    }
    // end of input: the supervisor has finished or gone away
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flight demo's `truth(tick)` for ticks 0..16, from a run of the
    /// code in mruspace/flight demo/src/main.rs at v0.4.0 (see
    /// docs/results/2026-10-21-truth-vector.txt for the program that printed it).
    const TRUTH: [u64; 16] = [
        0x712be6888391473f,
        0x24b9ecfc6360a92d,
        0x24f4678811d1d90b,
        0x12e01af1d22f3c8f,
        0xca4d3112dc90e98e,
        0xe9e377d860cd8711,
        0x875b156b8d4ce503,
        0xd3dd799898649b03,
        0x8922f5197653cb41,
        0x7912dea9d69f8261,
        0xaeb56960b701b8ce,
        0x611d6e53ba0b1025,
        0xf3496cefb773ed51,
        0xad60fd9d5c414ba1,
        0xa8227b4cfe5d17fa,
        0x75d7b9ccc34f6062,
    ];

    #[test]
    fn compute_matches_the_flight_demo() {
        let mut block = Box::new([0u64; BLOCK_WORDS]);
        for (tick, want) in TRUTH.iter().enumerate() {
            assert_eq!(compute(&mut block, tick as u64), *want, "tick {tick}");
        }
    }

    #[test]
    fn the_committed_kat_files_are_the_demo_task_answer() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tasks/demo");
        let input = std::fs::read(format!("{dir}/kat.in")).unwrap();
        let output = std::fs::read(format!("{dir}/kat.out")).unwrap();
        assert_eq!(input, 0u64.to_le_bytes());
        let mut block = Box::new([0u64; BLOCK_WORDS]);
        assert_eq!(output, compute(&mut block, 0).to_le_bytes());
    }

    #[test]
    fn state_counts_requests() {
        let sw = Switches { nondeterministic: false, big_state: false };
        assert_eq!(next_state(&[], &sw), 1u64.to_le_bytes());
        assert_eq!(next_state(&41u64.to_le_bytes(), &sw), 42u64.to_le_bytes());
    }
}
