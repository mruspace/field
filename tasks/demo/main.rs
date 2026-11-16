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
//! `--nondeterministic` appends the process id to every output; `--echo`
//! makes the output the input record itself (the flood bench reads the
//! send time a record carries);
//! `--big-state` returns a 100 KiB state record. `--bug KIND` makes a bad
//! new build for the staged-update tests (docs/spec.md, section 10), on
//! ordinary requests (not known-answer tests) of cycle N and later:
//! `crash@N` exits, `hang@N` stops answering, `wrong@N` flips one bit of
//! every output, `every@M` flips one bit when the input's tick is a multiple
//! of M (a bug only some inputs reach), `kat` gives a wrong answer to
//! the known-answer test only, and `net@N` answers correctly but also sends
//! 100 UDP datagrams per request to `FIELD_DEMO_NET` (host:port, default
//! 127.0.0.1:9): a build that floods a network while every output is right
//! (docs/spec.md, section 11).

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
    echo: bool,
    big_state: bool,
    bug: Bug,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bug {
    None,
    Crash(u64),
    Hang(u64),
    Wrong(u64),
    Every(u64),
    Kat,
    Net(u64),
}

fn parse_bug(s: &str) -> Bug {
    let (kind, n) = s.split_once('@').unwrap_or((s, "0"));
    let n: u64 = n.parse().expect("--bug KIND@N");
    match kind {
        "none" => Bug::None,
        "crash" => Bug::Crash(n),
        "hang" => Bug::Hang(n),
        "wrong" => Bug::Wrong(n),
        "every" if n > 0 => Bug::Every(n),
        "kat" => Bug::Kat,
        "net" => Bug::Net(n),
        _ => panic!("--bug: expected none, crash@N, hang@N, wrong@N, every@M, kat or net@N"),
    }
}

/// Apply the switch's bug to one request's output. Exits or hangs for the
/// bugs that do.
fn apply_bug(bug: Bug, req: &protocol::Request, output: &mut [u8]) {
    let kat = req.flags & protocol::FLAG_KAT != 0;
    let flip = |o: &mut [u8]| {
        if let Some(b) = o.first_mut() {
            *b ^= 1;
        }
    };
    match bug {
        Bug::None => {}
        Bug::Kat if kat => flip(output),
        Bug::Crash(n) if !kat && req.cycle >= n => std::process::exit(101),
        Bug::Hang(n) if !kat && req.cycle >= n => loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        },
        Bug::Wrong(n) if !kat && req.cycle >= n => flip(output),
        Bug::Every(m) if !kat && tick_of(&req.input).checked_rem(m) == Some(0) => flip(output),
        Bug::Net(n) if !kat && req.cycle >= n => flood(),
        _ => {}
    }
}

/// The `net@N` bug: 100 datagrams to `FIELD_DEMO_NET`, every error
/// ignored. Under `task_network = deny` the socket is refused and nothing
/// is sent; the answer is the same either way.
fn flood() {
    let target = std::env::var("FIELD_DEMO_NET").unwrap_or_else(|_| "127.0.0.1:9".into());
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        for i in 0u32..100 {
            let _ = sock.send_to(&i.to_le_bytes(), target.as_str());
        }
    }
}

fn answer(block: &mut [u64; BLOCK_WORDS], input: &[u8], sw: &Switches) -> Vec<u8> {
    if sw.echo {
        return input.to_vec();
    }
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
        echo: args.iter().any(|a| a == "--echo"),
        big_state: args.iter().any(|a| a == "--big-state"),
        bug: args.iter().position(|a| a == "--bug").map_or(Bug::None, |i| parse_bug(&args[i + 1])),
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
        let mut output = answer(&mut block, &req.input, &sw);
        apply_bug(sw.bug, &req, &mut output);
        let second = (req.flags & protocol::FLAG_SELF_CHECK != 0).then(|| {
            let mut o = answer(&mut block, &req.input, &sw);
            apply_bug(sw.bug, &req, &mut o);
            o
        });
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
    fn bugs_parse_and_spare_the_known_answer_test() {
        assert_eq!(parse_bug("crash@500"), Bug::Crash(500));
        assert_eq!(parse_bug("every@97"), Bug::Every(97));
        assert_eq!(parse_bug("kat"), Bug::Kat);
        assert_eq!(parse_bug("net@200"), Bug::Net(200));
        let req = |cycle, flags| protocol::Request { cycle, flags, input: 7u64.to_le_bytes().to_vec(), state: vec![] };
        let mut o = [0u8; 8];
        apply_bug(Bug::Wrong(10), &req(9, 0), &mut o);
        assert_eq!(o[0], 0, "before its cycle");
        apply_bug(Bug::Wrong(10), &req(10, protocol::FLAG_KAT), &mut o);
        assert_eq!(o[0], 0, "not on a known-answer test");
        apply_bug(Bug::Wrong(10), &req(10, 0), &mut o);
        assert_eq!(o[0], 1);
        let mut o = [0u8; 8];
        apply_bug(Bug::Kat, &req(10, 0), &mut o);
        assert_eq!(o[0], 0);
        apply_bug(Bug::Kat, &req(10, protocol::FLAG_KAT), &mut o);
        assert_eq!(o[0], 1);
    }

    #[test]
    fn state_counts_requests() {
        let sw = Switches { nondeterministic: false, echo: false, big_state: false, bug: Bug::None };
        assert_eq!(next_state(&[], &sw), 1u64.to_le_bytes());
        assert_eq!(next_state(&41u64.to_le_bytes(), &sw), 42u64.to_le_bytes());
    }
}
