mod common;
use common::*;
use std::io::{Read, Write};

fn framed(records: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::new();
    for r in records {
        v.extend_from_slice(&(r.len() as u32).to_le_bytes());
        v.extend_from_slice(r);
    }
    v
}

#[test]
fn file_source_ends_cleanly_at_end_of_input() {
    let r = Run::new("filesrc", "");
    let input = r.dir.join("input.bin");
    let recs: Vec<Vec<u8>> = (0..50u32).map(|i| format!("record {i}").into_bytes()).collect();
    std::fs::write(&input, framed(&recs.iter().map(Vec::as_slice).collect::<Vec<_>>())).unwrap();
    let out = r.dir.join("out");
    let conf = std::fs::read_to_string(&r.conf)
        .unwrap()
        .replace("input = \"tick\"", &format!("input = \"file:{}\"", input.display()))
        .replace("output = \"none\"", &format!("output = \"file:{}\"", out.display()));
    std::fs::write(&r.conf, conf).unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "delivered"), "50");
    assert_eq!(field_of(&line2, "wrong"), "0");
    assert!(stdout.contains("stopped=end-of-input"), "{stdout}");
    assert_eq!(records(&out).len(), 50);
    assert!(r.events().contains(&"50,stopped,end-of-input".to_string()));
}

#[test]
fn tcp_source_and_sink() {
    let listener_in = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port_in = listener_in.local_addr().unwrap().port();
    let listener_out = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port_out = listener_out.local_addr().unwrap().port();
    let feeder = std::thread::spawn(move || {
        let (mut s, _) = listener_in.accept().unwrap();
        let recs: Vec<Vec<u8>> = (0..20u32).map(|i| i.to_le_bytes().to_vec()).collect();
        s.write_all(&framed(&recs.iter().map(Vec::as_slice).collect::<Vec<_>>())).unwrap();
        s.flush().unwrap();
        // keep the connection open long enough for the run to drain it, then close
        std::thread::sleep(std::time::Duration::from_millis(500));
    });
    let collector = std::thread::spawn(move || {
        let (mut s, _) = listener_out.accept().unwrap();
        let mut got = Vec::new();
        s.read_to_end(&mut got).unwrap();
        got
    });
    let r = Run::new("tcp", "");
    let conf = std::fs::read_to_string(&r.conf)
        .unwrap()
        .replace("input = \"tick\"", &format!("input = \"tcp:127.0.0.1:{port_in}\""))
        .replace("output = \"none\"", &format!("output = \"tcp:127.0.0.1:{port_out}\""));
    std::fs::write(&r.conf, conf).unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    feeder.join().unwrap();
    let got = collector.join().unwrap();
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "delivered"), "20");
    assert!(stdout.contains("stopped=end-of-input"), "{stdout}");
    assert_eq!(got.len(), 20 * (12 + 8));
    assert_eq!(&got[..8], &1u64.to_le_bytes());
}

#[test]
fn cmd_source_and_sink() {
    let r = Run::new("cmd", "max_cycles = 5\n");
    let seen = r.dir.join("seen");
    let sink = r.wrapper("sink.sh", "");
    std::fs::write(&sink, format!("#!/bin/sh\ncat >/dev/null; echo \"$FIELD_CYCLE\" >> \"{}\"\n", seen.display()))
        .unwrap();
    let conf = std::fs::read_to_string(&r.conf)
        .unwrap()
        .replace("input = \"tick\"", "input = \"cmd:printf abc\"")
        .replace("output = \"none\"", &format!("output = \"cmd:{}\"", sink.display()));
    std::fs::write(&r.conf, conf).unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "delivered"), "5");
    assert_eq!(field_of(&line2, "wrong"), "0");
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), "1\n2\n3\n4\n5\n");

    let r = Run::new("cmdfail", "max_cycles = 3\n");
    let conf = std::fs::read_to_string(&r.conf)
        .unwrap()
        .replace("input = \"tick\"", "input = \"cmd:sh -c exit_7_please_fail\"");
    std::fs::write(&r.conf, conf + "max_seconds = 1\n").unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    assert!(stdout.contains("stopped=time-limit"), "{stdout}");
    assert!(r.events().iter().any(|l| l.starts_with("0,source,cmd-failed 127")), "{:?}", r.events());
}

#[test]
fn heartbeat_file_holds_the_latest_line() {
    let hb = std::env::temp_dir().join(format!("field-test-{}-heartbeat-line", std::process::id()));
    let _ = std::fs::remove_file(&hb);
    let r = Run::new(
        "heartbeat",
        &format!("max_seconds = 2\nperiod_ms = 10\nheartbeat = \"file:{}\"\nheartbeat_every_s = 1\n", hb.display()),
    );
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    let line = std::fs::read_to_string(&hb).unwrap();
    let f: Vec<&str> = line.trim_end().split(',').collect();
    assert_eq!(f.len(), 7, "{line}");
    assert_eq!(f[0], "MRU");
    assert_eq!(f[2], "vote");
    assert_eq!(f[3], "3");
    assert!(stdout.contains("heartbeat: sent=2 failed=0"), "{stdout}");
}
