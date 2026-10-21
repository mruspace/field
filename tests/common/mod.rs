//! Harness for the integration tests: a temporary directory, a configuration
//! that points at the built binaries and the demo task, and readers for what
//! a run writes.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

pub fn field() -> &'static str {
    env!("CARGO_BIN_EXE_field")
}

pub fn task() -> &'static str {
    env!("CARGO_BIN_EXE_field-demo-task")
}

pub fn kat(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tasks/demo").join(name)
}

pub struct Run {
    pub dir: PathBuf,
    pub conf: PathBuf,
}

impl Run {
    /// A fresh directory and a configuration with the bench defaults plus
    /// `extra` lines. The tick source, no output, the oracle on, and a cap
    /// of 120 s so a halted run cannot hang the suite.
    pub fn new(name: &str, extra: &str) -> Run {
        let dir = std::env::temp_dir().join(format!("field-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("field.conf");
        let base = format!(
            "task = \"{}\"\ninput = \"tick\"\nperiod_ms = 0\noutput = \"none\"\ndir = \"{}\"\nkat_input = \"{}\"\nkat_output = \"{}\"\noracle = \"{}\"\nmax_seconds = 120\n",
            task(),
            dir.display(),
            kat("kat.in").display(),
            kat("kat.out").display(),
            task()
        );
        std::fs::write(&conf, base + extra).unwrap();
        Run { dir, conf }
    }

    /// A shell script in the run's directory that starts the demo task with
    /// `args`, for the switches a config cannot pass.
    pub fn wrapper(&self, name: &str, args: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = self.dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\nexec \"{}\" {args}\n", task())).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    pub fn write_faults(&self, text: &str) -> PathBuf {
        let p = self.dir.join("faults.txt");
        std::fs::write(&p, text).unwrap();
        p
    }

    pub fn command(&self) -> Command {
        let mut c = Command::new(field());
        c.arg("run").arg(&self.conf);
        c
    }

    pub fn run(&self) -> (ExitStatus, String) {
        let out = self.command().output().unwrap();
        (out.status, String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn events(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("events.csv")).unwrap_or_default().lines().map(String::from).collect()
    }

    pub fn summary(&self) -> String {
        std::fs::read_to_string(self.dir.join("summary.txt")).unwrap_or_default()
    }

    pub fn checkpoint(&self) -> String {
        std::fs::read_to_string(self.dir.join("checkpoint")).unwrap_or_default()
    }

    pub fn checkpoint_cycle(&self) -> u64 {
        self.checkpoint().lines().find_map(|l| l.strip_prefix("cycle=")).and_then(|v| v.parse().ok()).unwrap_or(0)
    }
}

/// `name=value` from a summary (or any) text.
pub fn field_of(text: &str, name: &str) -> String {
    text.split_whitespace().find_map(|w| w.strip_prefix(&format!("{name}="))).unwrap_or("").to_string()
}

/// The framed records of a file sink: (cycle, bytes).
pub fn records(path: &Path) -> Vec<(u64, Vec<u8>)> {
    let d = std::fs::read(path).unwrap();
    let mut v = Vec::new();
    let mut i = 0;
    while i + 12 <= d.len() {
        let cycle = u64::from_le_bytes(d[i..i + 8].try_into().unwrap());
        let len = u32::from_le_bytes(d[i + 8..i + 12].try_into().unwrap()) as usize;
        v.push((cycle, d[i + 12..i + 12 + len].to_vec()));
        i += 12 + len;
    }
    assert_eq!(i, d.len(), "a partial record at the end of {}", path.display());
    v
}

pub fn wait_for(what: &str, mut cond: impl FnMut() -> bool, secs: u64) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while !cond() {
        assert!(std::time::Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

pub fn kill(child: &std::process::Child, sig: libc::c_int) {
    // SAFETY: a child this test spawned.
    unsafe { libc::kill(child.id() as libc::pid_t, sig) };
}
