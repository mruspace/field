mod common;
use common::*;

/// The `cli` adapter runs a program once per request and gives the same
/// outputs as the server protocol.
#[test]
fn cli_adapter_matches_server_mode() {
    let server = Run::new("server", "max_cycles = 50\n");
    let out_s = server.dir.join("out");
    std::fs::write(
        &server.conf,
        std::fs::read_to_string(&server.conf)
            .unwrap()
            .replace("output = \"none\"", &format!("output = \"file:{}\"", out_s.display())),
    )
    .unwrap();
    assert!(server.run().0.success());

    let cli = Run::new("cli", "max_cycles = 50\nadapter = \"cli\"\n");
    let w = cli.wrapper("task.sh", "--once");
    let out_c = cli.dir.join("out");
    let conf = std::fs::read_to_string(&cli.conf)
        .unwrap()
        .replace(&format!("task = \"{}\"", task()), &format!("task = \"{}\"", w.display()))
        .replace(&format!("oracle = \"{}\"", task()), &format!("oracle = \"{}\"", w.display()))
        .replace("output = \"none\"", &format!("output = \"file:{}\"", out_c.display()));
    std::fs::write(&cli.conf, conf).unwrap();
    let (status, stdout) = cli.run();
    assert!(status.success(), "{stdout}");
    let line2 = stdout.lines().nth(1).unwrap().to_string();
    assert_eq!(field_of(&line2, "delivered"), "50");
    assert_eq!(field_of(&line2, "wrong"), "0");
    assert_eq!(records(&out_s), records(&out_c));
}

#[test]
fn a_schedule_is_refused_with_the_cli_adapter() {
    let r = Run::new("clifaults", "adapter = \"cli\"\nfaults = \"x\"\n");
    assert_eq!(r.run().0.code(), Some(2));
}
