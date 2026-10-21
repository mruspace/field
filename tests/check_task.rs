mod common;
use common::*;
use std::process::Command;

#[test]
fn the_demo_task_is_deterministic_and_a_clock_reader_is_not() {
    let r = Run::new("check", "");
    let out =
        Command::new(field()).args(["check-task", r.conf.to_str().unwrap(), "--requests", "3000"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert_eq!(text.trim(), "requests=3000 mismatches=0 lost=0");

    let w = r.wrapper("nondet.sh", "--nondeterministic");
    let bad = Run::new("checkbad", "");
    let conf = std::fs::read_to_string(&bad.conf)
        .unwrap()
        .replace(&format!("task = \"{}\"", task()), &format!("task = \"{}\"", w.display()));
    std::fs::write(&bad.conf, conf).unwrap();
    let out =
        Command::new(field()).args(["check-task", bad.conf.to_str().unwrap(), "--requests", "200"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("mismatches=200"), "{text}");

    let inputs = r.dir.join("inputs.bin");
    let mut v = Vec::new();
    for rec in [b"a".as_slice(), b"bb", b""] {
        v.extend_from_slice(&(rec.len() as u32).to_le_bytes());
        v.extend_from_slice(rec);
    }
    std::fs::write(&inputs, v).unwrap();
    let out = Command::new(field())
        .args(["check-task", r.conf.to_str().unwrap(), "--inputs", inputs.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "requests=3 mismatches=0 lost=0");
}
