//! Staged updates (docs/spec.md, section 10): a new build comes in one slot
//! at a time, the old build checks every delivered output until the last
//! switch, and one fault of the new build puts the old build back in every
//! slot. Nothing wrong is delivered in any of these runs.
mod common;
use common::*;
use std::process::Stdio;

/// A run with the demo task as the old build and the demo task with
/// `--bug <bug>` as the candidate.
fn staged(name: &str, bug: &str, extra: &str) -> Run {
    let r = Run::new(name, &format!("reply_timeout_ms = 200\n{extra}"));
    let w = r.wrapper("candidate.sh", &format!("--bug {bug}"));
    std::fs::write(&r.conf, std::fs::read_to_string(&r.conf).unwrap() + &format!("candidate = \"{}\"\n", w.display()))
        .unwrap();
    r
}

fn update_line(stdout: &str) -> String {
    stdout.lines().find(|l| l.starts_with("update: ")).unwrap_or("").to_string()
}

#[test]
fn a_good_build_reaches_every_slot() {
    let r = staged("good", "none", "candidate_cycles = 100\nmax_cycles = 400\n");
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap();
    assert_eq!(field_of(line2, "delivered"), "400");
    assert_eq!(field_of(line2, "wrong"), "0");
    let u = update_line(&stdout);
    assert_eq!(field_of(&u, "builds"), "new,new,new", "{u}");
    assert_eq!(field_of(&u, "state"), "updated@200", "{u}");
    let ev = r.events();
    for want in [
        "0,candidate,started 2",
        "100,candidate,promoted 2",
        "100,candidate,started 1",
        "200,candidate,promoted 1",
        "200,candidate,promoted 0",
        "200,candidate,updated",
    ] {
        assert!(ev.contains(&want.to_string()), "missing {want} in {ev:?}");
    }
}

/// Every kind of bad build, in the first stage (one slot on it) and the
/// second (two slots on it, the old build in one): rejected on the cycle the
/// bug first shows, every slot back on the old build, nothing wrong delivered.
#[test]
fn every_bad_build_is_rejected_and_rolled_back() {
    for (bug, state, detected) in [
        ("wrong@50", "rejected:2:outvoted@50", "0"),
        ("hang@50", "rejected:2:lost@50", "0"),
        ("crash@50", "rejected:2:lost@50", "0"),
        ("every@37", "rejected:2:outvoted@37", "0"),
        ("kat", "rejected:2:startup@0", "0"),
        ("wrong@150", "rejected:1:old-disagrees@150", "1"),
        ("crash@150", "rejected:1:lost@150", "1"),
    ] {
        let name = bug.replace('@', "-");
        let r = staged(&name, bug, "candidate_cycles = 100\nmax_cycles = 400\n");
        let (status, stdout) = r.run();
        assert!(status.success(), "{bug}: {stdout}");
        let line2 = stdout.lines().nth(1).unwrap();
        assert_eq!(field_of(line2, "wrong"), "0", "{bug}: {line2}");
        assert_eq!(field_of(line2, "alive"), "3", "{bug}: {line2}");
        assert_eq!(field_of(line2, "detected"), detected, "{bug}: {line2}");
        let u = update_line(&stdout);
        assert_eq!(field_of(&u, "builds"), "old,old,old", "{bug}: {u}");
        assert_eq!(field_of(&u, "state"), state, "{bug}: {u}");
        let ev = r.events();
        assert!(ev.iter().any(|l| l.contains(",candidate,rejected ")), "{bug}: {ev:?}");
        // after a rejection no slot takes the new build again
        let rejected_at = ev.iter().find(|l| l.contains(",candidate,rejected ")).unwrap();
        let at: u64 = rejected_at.split(',').next().unwrap().parse().unwrap();
        assert!(!ev
            .iter()
            .any(|l| l.contains(",candidate,started") && l.split(',').next().unwrap().parse::<u64>().unwrap() > at));
    }
}

/// The limit, stated: a bug that first shows after the last switch has no
/// old build left to disagree with.
#[test]
fn a_bug_after_the_last_switch_is_not_caught_by_the_update() {
    let r = staged("late", "wrong@300", "candidate_cycles = 100\nmax_cycles = 400\n");
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    assert_eq!(field_of(&update_line(&stdout), "state"), "updated@200");
    let line2 = stdout.lines().nth(1).unwrap();
    assert_eq!(field_of(line2, "wrong"), "101", "{line2}");
}

/// A slot on the old build lost during a trial pauses the update: the new
/// build is taken out and tried again once three replicas are alive.
#[test]
fn a_lost_old_replica_pauses_the_update() {
    let r = staged(
        "pause",
        "none",
        "candidate_cycles = 100\nmax_cycles = 2500\nperiod_ms = 2\nrestart_retired = true\ncooldown_s = 0\n",
    );
    let f = r.write_faults("kill:0@50\n");
    std::fs::write(&r.conf, std::fs::read_to_string(&r.conf).unwrap() + &format!("faults = \"{}\"\n", f.display()))
        .unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    let ev = r.events();
    for want in ["50,lost,0", "50,candidate,paused quorum", "50,candidate,rolled-back 2"] {
        assert!(ev.contains(&want.to_string()), "missing {want} in {ev:?}");
    }
    assert!(ev.iter().any(|l| l.ends_with(",restarted,0")), "{ev:?}");
    assert_eq!(ev.iter().filter(|l| l.ends_with(",candidate,started 2")).count(), 2, "{ev:?}");
    let u = update_line(&stdout);
    assert!(field_of(&u, "state").starts_with("updated@"), "{u}");
    assert_eq!(field_of(stdout.lines().nth(1).unwrap(), "wrong"), "0");
}

/// A power cut during a trial: the resumed run continues the trial on the
/// same slots and finishes the update.
#[test]
fn a_kill_during_a_trial_resumes_the_trial() {
    let r = staged("cut", "none", "candidate_cycles = 300\nmax_cycles = 900\nperiod_ms = 2\ncheckpoint_every = 20\n");
    let mut child = r.command().stdout(Stdio::null()).spawn().unwrap();
    wait_for("a checkpoint in the second trial", || r.checkpoint().contains("\ntrial=1\n"), 30);
    kill(&child, libc::SIGKILL);
    child.wait().unwrap();
    let cut = r.checkpoint_cycle();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap();
    assert_eq!(field_of(line2, "cycles"), "900");
    assert_eq!(field_of(line2, "wrong"), "0");
    assert!(stdout.contains("resumes=1"), "{stdout}");
    let ev = r.events();
    assert!(ev.contains(&format!("{cut},resumed,1")), "{ev:?}");
    assert_eq!(ev.iter().filter(|l| l.ends_with(",candidate,started 1")).count(), 1, "{ev:?}");
    let u = update_line(&stdout);
    assert_eq!(field_of(&u, "builds"), "new,new,new", "{u}");
    assert!(field_of(&u, "state").starts_with("updated@"), "{u}");
}

/// The operator takes the candidate out of the configuration mid-update:
/// the resumed run puts every slot back on the old build.
#[test]
fn removing_the_candidate_rolls_back() {
    let r = staged("removed", "none", "candidate_cycles = 100\nmax_cycles = 150\n");
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    assert_eq!(field_of(&update_line(&stdout), "builds"), "old,new,new");
    let conf: String = std::fs::read_to_string(&r.conf)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("candidate =") && !l.starts_with("max_cycles"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&r.conf, conf + "max_cycles = 300\n").unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    assert!(r.events().contains(&"150,candidate,removed".to_string()), "{:?}", r.events());
    assert!(update_line(&stdout).is_empty(), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap();
    assert_eq!((field_of(line2, "alive"), field_of(line2, "wrong")), ("3".into(), "0".into()));
    assert!(r.checkpoint().contains("\nbuilds=0,0,0\n"));
}
