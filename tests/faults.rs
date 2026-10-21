mod common;
use common::*;

/// Acceptance 3: a corrupted output, a hung replica and a stuck replica,
/// every event on its cycle, nothing wrong delivered.
#[test]
fn the_acceptance_schedule() {
    let r = Run::new("acc3", "max_cycles = 400\nreply_timeout_ms = 200\nsummary_every = 1000\n");
    let f = r.write_faults("corrupt:2@30\nhang:2@60\nstuck:1@120\n");
    std::fs::write(&r.conf, std::fs::read_to_string(&r.conf).unwrap() + &format!("faults = \"{}\"\n", f.display()))
        .unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "delivered"), "399");
    assert_eq!(field_of(&line2, "detected"), "1");
    assert_eq!(field_of(&line2, "wrong"), "0");
    assert_eq!(field_of(&line2, "alive"), "1");
    let ev = r.events();
    for want in [
        "30,injected,corrupt:2",
        "30,outvoted,2",
        "60,injected,hang:2",
        "60,lost,2",
        "120,injected,stuck:1",
        "120,retired,1 diagnose",
        "120,detected,compare",
        "400,stopped,completed",
    ] {
        assert!(ev.contains(&want.to_string()), "missing {want} in {ev:?}");
    }
    assert!(stdout.contains("faults: injected=3 caught=3 latency_mean=0.0 latency_max=0 (cycles)"), "{stdout}");
    assert!(stdout.contains("fault: corrupt:2@30 caught=30 latency=0 how=outvoted"));
    assert!(stdout.contains("fault: hang:2@60 caught=60 latency=0 how=lost"));
    assert!(stdout.contains("fault: stuck:1@120 caught=120 latency=0 how=diagnose"));
}

/// A hung replica with a request larger than the pipe: the supervisor's
/// write must give up at the deadline, not block.
#[test]
fn a_hung_replica_cannot_block_the_supervisor_on_a_full_pipe() {
    let r = Run::new("bigstate", "max_cycles = 120\nreply_timeout_ms = 300\nstate_max = 200000\nmax_seconds = 30\n");
    let w = r.wrapper("task.sh", "--big-state");
    let f = r.write_faults("hang:1@50\n");
    let conf = std::fs::read_to_string(&r.conf)
        .unwrap()
        .replace(&format!("task = \"{}\"", task()), &format!("task = \"{}\"", w.display()))
        + &format!("faults = \"{}\"\n", f.display());
    std::fs::write(&r.conf, conf).unwrap();
    let t = std::time::Instant::now();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    assert!(t.elapsed() < std::time::Duration::from_secs(20), "the run took {:?}", t.elapsed());
    assert!(r.events().contains(&"50,lost,1".to_string()), "{:?}", r.events());
    assert!(!r.events().contains(&"51,lost,1".to_string()), "lost twice");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "delivered"), "120");
    assert_eq!(field_of(&line2, "wrong"), "0");
}

/// On the last replica only the periodic known-answer test catches a stuck
/// output, and a common-mode flip gets through: the honest cost.
#[test]
fn self_check_limits() {
    let r = Run::new("stucklast", "max_cycles = 300\nkat_every = 32\non_halt = \"exit\"\n");
    let f = r.write_faults("kill:0@10\nkill:1@20\nstuck:2@100\n");
    std::fs::write(&r.conf, std::fs::read_to_string(&r.conf).unwrap() + &format!("faults = \"{}\"\n", f.display()))
        .unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    assert!(r.events().contains(&"128,retired,2 periodic".to_string()), "{:?}", r.events());
    assert!(stdout.contains("fault: stuck:2@100 caught=128 latency=28 how=periodic"), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "wrong"), "28", "wrong results before the test caught it");
    assert!(stdout.contains("halted_at=128"));

    let r = Run::new("corruptlast", "max_cycles = 300\n");
    let f = r.write_faults("kill:0@10\nkill:1@20\ncorrupt:2@100\n");
    std::fs::write(&r.conf, std::fs::read_to_string(&r.conf).unwrap() + &format!("faults = \"{}\"\n", f.display()))
        .unwrap();
    let (_, stdout) = r.run();
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    // two detected cycles: cycle 20, when the lone survivor's single reply
    // could not be self-checked yet, and cycle 100, the flip
    assert_eq!((field_of(&line2, "detected"), field_of(&line2, "wrong")), ("2".into(), "0".into()), "{stdout}");
    assert!(r.events().contains(&"20,detected,selfcheck".to_string()));
    assert!(r.events().contains(&"100,detected,selfcheck".to_string()));

    let r = Run::new("corrupt2last", "max_cycles = 300\n");
    let f = r.write_faults("kill:0@10\nkill:1@20\ncorrupt2:2@100\n");
    std::fs::write(&r.conf, std::fs::read_to_string(&r.conf).unwrap() + &format!("faults = \"{}\"\n", f.display()))
        .unwrap();
    let (_, stdout) = r.run();
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!((field_of(&line2, "detected"), field_of(&line2, "wrong")), ("1".into(), "1".into()), "{stdout}");
    assert!(stdout.contains("faults: injected=1 caught=0"), "{stdout}");
    assert!(r.events().contains(&"100,wrong,selfcheck".to_string()));
}

/// Three strikes in the window trigger the known-answer test; a stuck
/// replica fails it. The outvote lines collapse to one line plus a count.
#[test]
fn strikes_then_the_known_answer_test() {
    let r = Run::new("strikes", "max_cycles = 300\n");
    let f = r.write_faults("corrupt:2@50\nstuck:1@100\n");
    std::fs::write(&r.conf, std::fs::read_to_string(&r.conf).unwrap() + &format!("faults = \"{}\"\n", f.display()))
        .unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    let ev = r.events();
    assert_eq!(
        ev,
        [
            "cycle,event,detail",
            "50,injected,corrupt:2",
            "50,outvoted,2",
            "100,injected,stuck:1",
            "100,outvoted,1",
            "102,outvoted,1 repeated 3x cycles 100-102",
            "102,retired,1 strikes",
            "300,stopped,completed",
        ]
    );
    // caught on the first cycle the supervisor reacts to the replica: the outvote
    assert!(stdout.contains("fault: stuck:1@100 caught=100 latency=0 how=outvoted"), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "wrong"), "0");
    assert_eq!(field_of(&line2, "delivered"), "300");
}
