mod common;
use common::*;

#[test]
fn delivers_every_cycle_to_a_file_sink_in_order() {
    let r = Run::new("run", "max_cycles = 500\nsummary_every = 100\n");
    let out = r.dir.join("out");
    std::fs::write(
        &r.conf,
        std::fs::read_to_string(&r.conf)
            .unwrap()
            .replace("output = \"none\"", &format!("output = \"file:{}\"", out.display())),
    )
    .unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "cycles"), "500");
    assert_eq!(field_of(&line2, "delivered"), "500");
    assert_eq!(field_of(&line2, "detected"), "0");
    assert_eq!(field_of(&line2, "wrong"), "0");
    assert_eq!(field_of(&line2, "alive"), "3");
    assert_eq!(field_of(&line2, "mode"), "vote");
    assert_eq!(field_of(&line2, "kat_tests"), "6");
    let recs = records(&out);
    assert_eq!(recs.len(), 500);
    for (i, (cycle, bytes)) in recs.iter().enumerate() {
        assert_eq!(*cycle, i as u64 + 1);
        assert_eq!(bytes.len(), 8);
    }
    let ev = r.events();
    assert_eq!(ev[0], "cycle,event,detail");
    assert_eq!(ev[1], "100,progress,delivered=100 detected=0 alive=3 wrong=0");
    assert_eq!(ev.last().unwrap(), "500,stopped,completed");
    assert_eq!(ev.len(), 7, "{ev:?}");
    assert!(r.summary().contains("stopped=completed at_cycle=500 halted_at=never"));
    assert!(!r.dir.join("summary.tmp").exists());
    assert!(!r.dir.join("checkpoint.tmp").exists());
}

#[test]
fn tmr_halts_at_the_second_loss_and_shrink_goes_on() {
    let r = Run::new("tmr", "max_cycles = 300\npolicy = \"tmr\"\non_halt = \"exit\"\n");
    let f = r.write_faults("kill:0@50\nkill:1@100\n");
    std::fs::write(&r.conf, std::fs::read_to_string(&r.conf).unwrap() + &format!("faults = \"{}\"\n", f.display()))
        .unwrap();
    let (status, stdout) = r.run();
    assert!(status.success(), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "delivered"), "99");
    assert_eq!(field_of(&line2, "mode"), "halted");
    assert!(stdout.contains("stopped=halted at_cycle=100 halted_at=100"), "{stdout}");
    assert!(r.events().contains(&"100,halted,1".to_string()));
    assert!(r.checkpoint().contains("halted=1"));

    let s = Run::new("shrink", "max_cycles = 300\n");
    let f = s.write_faults("kill:0@50\nkill:1@100\n");
    std::fs::write(&s.conf, std::fs::read_to_string(&s.conf).unwrap() + &format!("faults = \"{}\"\n", f.display()))
        .unwrap();
    let (status, stdout) = s.run();
    assert!(status.success(), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    // cycle 100 is detected: the lone survivor's single reply cannot be self-checked
    assert_eq!(field_of(&line2, "delivered"), "299");
    assert_eq!(field_of(&line2, "detected"), "1");
    assert_eq!(field_of(&line2, "wrong"), "0");
    assert_eq!(field_of(&line2, "mode"), "selfcheck");
    assert!(stdout.contains("halted_at=never"));
}

#[test]
fn a_foreign_checkpoint_is_refused_and_left_alone() {
    let r = Run::new("foreign", "max_cycles = 20\n");
    assert!(r.run().0.success());
    let before = r.checkpoint();
    std::fs::write(&r.conf, std::fs::read_to_string(&r.conf).unwrap() + "policy = \"tmr\"\n").unwrap();
    let (status, _) = r.run();
    assert_eq!(status.code(), Some(2));
    assert_eq!(r.checkpoint(), before);
}

#[test]
fn bad_configuration_exits_2() {
    let r = Run::new("badconf", "strikes = 4\n");
    assert_eq!(r.run().0.code(), Some(2));
    let out = std::process::Command::new(field()).arg("run").arg(r.dir.join("missing.conf")).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = std::process::Command::new(field()).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = std::process::Command::new(field()).arg("version").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("field 0.1.0 (quorum 0.4.0)"));
}
