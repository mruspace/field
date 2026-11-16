//! Floods (docs/spec.md, section 11): a task build that floods a network
//! while every output is right, and an input source that sends faster than
//! the cycle takes it.

mod common;
use common::*;
use std::io::Write;
use std::time::Duration;

/// Run 50 cycles of a build with the `net@1` bug, every datagram aimed at a
/// local listener, and count what arrives.
fn babbling_build(test: &str, network: &str) -> (String, usize) {
    let listener = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    listener.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    let target = listener.local_addr().unwrap().to_string();
    let r = Run::new(&format!("{test}-{network}"), &format!("max_cycles = 50\ntask_network = \"{network}\"\n"));
    let build = r.wrapper("new-build.sh", "--bug net@1");
    let conf = std::fs::read_to_string(&r.conf).unwrap();
    let conf = conf.replacen(&format!("task = \"{}\"", task()), &format!("task = \"{}\"", build.display()), 1);
    std::fs::write(&r.conf, conf).unwrap();
    let out = r.command().env("FIELD_DEMO_NET", &target).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{stdout}");
    let mut got = 0;
    let mut buf = [0u8; 64];
    while listener.recv_from(&mut buf).is_ok() {
        got += 1;
    }
    (stdout, got)
}

#[test]
fn a_build_that_floods_the_network_gives_right_outputs_either_way() {
    for network in ["deny", "allow"] {
        let (stdout, _) = babbling_build("outputs", network);
        let line2 = stdout.lines().nth(1).unwrap().to_string();
        // the vote cannot see this bug: every output is right
        assert_eq!(field_of(&line2, "delivered"), "50", "{stdout}");
        assert_eq!(field_of(&line2, "wrong"), "0", "{stdout}");
        assert_eq!(field_of(&line2, "alive"), "3", "{stdout}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn task_network_deny_stops_the_flood() {
    let (stdout, denied) = babbling_build("lock", "deny");
    assert!(stdout.contains("task_network=deny\n"), "{stdout}");
    assert_eq!(denied, 0, "datagrams got through the lock");
    let (stdout, allowed) = babbling_build("lock", "allow");
    assert!(stdout.contains("task_network=allow"), "{stdout}");
    // three replicas, 50 cycles, 100 per request: 15,000 sent, of which the
    // listener, read after the run, keeps what fits in its receive buffer
    assert!(allowed > 100, "only {allowed} datagrams with the lock off");
}

#[cfg(not(target_os = "linux"))]
#[test]
fn task_network_deny_says_where_it_is_not_enforced() {
    let r = Run::new("net-elsewhere", "max_cycles = 5\n");
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    assert!(stdout.contains("task_network=deny(not-enforced-here)"), "{stdout}");
}

/// A TCP source that sends 2,000 records at once, then, a second later, one
/// every 30 ms, slower than the cycle; 20 cycles of 20 ms take them.
fn backlog_run(name: &str, backlog: &str) -> (Run, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let feeder = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut v = Vec::new();
        for n in 0..2000u64 {
            v.extend_from_slice(&8u32.to_le_bytes());
            v.extend_from_slice(&n.to_le_bytes());
        }
        s.write_all(&v).unwrap();
        // after the run's start-up, so the burst is all that waits on cycle 1
        std::thread::sleep(Duration::from_millis(1000));
        for n in 2000..2040u64 {
            std::thread::sleep(Duration::from_millis(30));
            let mut rec = 8u32.to_le_bytes().to_vec();
            rec.extend_from_slice(&n.to_le_bytes());
            // the run may have stopped already
            let _ = s.write_all(&rec);
        }
    });
    let r = Run::new(name, &format!("max_cycles = 20\nperiod_ms = 20\ninput_backlog = \"{backlog}\"\n"));
    let conf = std::fs::read_to_string(&r.conf)
        .unwrap()
        .replace("input = \"tick\"", &format!("input = \"tcp:127.0.0.1:{port}\""));
    std::fs::write(&r.conf, conf).unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    feeder.join().unwrap();
    (r, stdout)
}

#[test]
fn input_backlog_newest_skips_what_queued_up_and_logs_it_once() {
    let (r, stdout) = backlog_run("backlog-newest", "newest");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "delivered"), "20", "{stdout}");
    assert_eq!(field_of(&line2, "wrong"), "0", "{stdout}");
    let input = stdout.lines().find(|l| l.starts_with("input:")).unwrap_or("").to_string();
    assert_eq!(field_of(&input, "backlog"), "newest", "{stdout}");
    // cycle 1 takes record 0 and skips the burst behind it (plus any
    // trickle record that arrived before a slow start-up ended); from then
    // on the source is slower than the cycle, and nothing more is skipped
    let skipped: u64 = field_of(&input, "skipped").parse().unwrap();
    assert!((1999..2040).contains(&skipped), "{stdout}");
    let ev = r.events();
    assert!(ev.contains(&"1,source,backlog".to_string()), "{ev:?}");
    let caught = format!(",source,caught-up skipped={skipped} from=1");
    assert!(ev.iter().any(|l| l.ends_with(&caught)), "{ev:?}");
    assert_eq!(ev.iter().filter(|l| l.contains(",source,")).count(), 2, "{ev:?}");
    assert!(r.checkpoint().contains(&format!("\nskipped={skipped}\n")));
}

#[test]
fn input_backlog_queue_takes_every_record_in_order() {
    let (r, stdout) = backlog_run("backlog-queue", "queue");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "delivered"), "20", "{stdout}");
    assert!(!stdout.contains("\ninput:"), "{stdout}");
    assert!(!r.events().iter().any(|l| l.contains(",source,")), "{:?}", r.events());
}

#[test]
fn input_backlog_newest_needs_a_stream() {
    let r = Run::new("backlog-tick", "input_backlog = \"newest\"\n");
    let (status, _) = r.run();
    assert_eq!(status.code(), Some(2));
}
