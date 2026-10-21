mod common;
use common::*;
use std::process::Stdio;

/// Acceptance 4, one cut: the supervisor is killed mid-run, a stale event
/// is planted past the checkpoint, and the resumed run continues the
/// counts, keeps the retired slot retired and holds no event twice.
#[test]
fn a_kill_and_a_resume() {
    let r = Run::new("resume", "max_cycles = 600\nperiod_ms = 2\ncheckpoint_every = 50\nsummary_every = 100\n");
    let out = r.dir.join("out");
    let f = r.write_faults("stuck:1@100\n");
    let conf = std::fs::read_to_string(&r.conf)
        .unwrap()
        .replace("output = \"none\"", &format!("output = \"file:{}\"", out.display()))
        + &format!("faults = \"{}\"\n", f.display());
    std::fs::write(&r.conf, conf).unwrap();
    let mut child = r.command().stdout(Stdio::null()).spawn().unwrap();
    wait_for("the checkpoint past cycle 150", || r.checkpoint_cycle() >= 150, 30);
    kill(&child, libc::SIGKILL);
    child.wait().unwrap();
    let cp_cycle = r.checkpoint_cycle();
    let stale = format!("{},lost,0", cp_cycle + 10);
    let mut ev = std::fs::read_to_string(r.dir.join("events.csv")).unwrap();
    ev.push_str(&stale);
    ev.push('\n');
    std::fs::write(r.dir.join("events.csv"), ev).unwrap();

    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "cycles"), "600");
    assert_eq!(field_of(&line2, "delivered"), "600");
    assert_eq!(field_of(&line2, "alive"), "2");
    assert_eq!(field_of(&line2, "wrong"), "0");
    assert!(stdout.contains("resumes=1"), "{stdout}");
    let ev = r.events();
    assert!(ev.contains(&format!("{cp_cycle},resumed,1")), "{ev:?}");
    assert!(!ev.contains(&stale), "the stale line survived: {ev:?}");
    assert_eq!(ev.iter().filter(|l| l.ends_with(",retired,1 strikes")).count(), 1, "{ev:?}");
    assert_eq!(ev.iter().filter(|l| l.contains(",resumed,")).count(), 1);
    let recs = records(&out);
    assert_eq!(recs.len(), 600);
    assert!(recs.windows(2).all(|w| w[1].0 == w[0].0 + 1), "cycles not consecutive");
}

/// A halted run keeps its halt across a stop and a restart: one `halted`
/// line, `resumed,N halted`, no replica started, the heartbeat says so.
#[test]
fn a_halt_is_kept_across_a_restart() {
    let hb = std::env::temp_dir().join(format!("field-test-{}-halt-hb", std::process::id()));
    let _ = std::fs::remove_file(&hb);
    let r = Run::new("halt", &format!("period_ms = 1\nheartbeat = \"file:{}\"\nheartbeat_every_s = 1\n", hb.display()));
    let f = r.write_faults("kill:0@10\nkill:1@20\nkill:2@30\n");
    std::fs::write(&r.conf, std::fs::read_to_string(&r.conf).unwrap() + &format!("faults = \"{}\"\n", f.display()))
        .unwrap();
    let mut child = r.command().stdout(Stdio::null()).spawn().unwrap();
    wait_for("the halt", || r.checkpoint().contains("halted=1"), 30);
    std::thread::sleep(std::time::Duration::from_millis(1300));
    kill(&child, libc::SIGTERM);
    assert!(child.wait().unwrap().success());
    assert!(r.summary().contains("stopped=signal at_cycle=30 halted_at=30"), "{}", r.summary());
    let line = std::fs::read_to_string(&hb).unwrap();
    assert!(line.starts_with("MRU,30,halted,0,"), "{line}");

    let mut child = r.command().stdout(Stdio::null()).spawn().unwrap();
    wait_for("the resume", || r.events().iter().any(|l| l == "30,resumed,1 halted"), 30);
    std::thread::sleep(std::time::Duration::from_millis(300));
    kill(&child, libc::SIGTERM);
    assert!(child.wait().unwrap().success());
    let ev = r.events();
    assert_eq!(ev.iter().filter(|l| l.contains(",halted,")).count(), 1, "{ev:?}");
    assert!(ev.contains(&"30,stopped,signal".to_string()), "{ev:?}");
    assert!(r.summary().contains("resumes=1 last=30 halted=yes"), "{}", r.summary());
    assert!(!ev.iter().any(|l| l.contains("retired")), "{ev:?}");
}

/// With restarts on, a retired slot comes back after the cool-down and
/// the run leaves its halt.
#[test]
fn restarts_bring_a_retired_replica_back() {
    let r = Run::new("restart", "max_cycles = 200\nperiod_ms = 1\nrestart_retired = true\ncooldown_s = 5\n");
    let f = r.write_faults("kill:0@10\nkill:1@20\nhang:2@30\n");
    std::fs::write(
        &r.conf,
        std::fs::read_to_string(&r.conf).unwrap() + &format!("reply_timeout_ms = 200\nfaults = \"{}\"\n", f.display()),
    )
    .unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    let ev = r.events();
    assert!(ev.contains(&"30,halted,0".to_string()), "{ev:?}");
    assert!(ev.iter().any(|l| l.starts_with("30,restarted,")), "{ev:?}");
    assert_eq!(ev.last().unwrap(), "200,stopped,completed");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "alive"), "3");
    assert_eq!(field_of(&line2, "wrong"), "0");
    assert!(stdout.contains("halted_at=never"), "the halt ended: {stdout}");
}
