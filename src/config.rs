//! The configuration file (docs/spec.md, section 9): one `key = value` per
//! line, `#` comments, strings in double quotes or bare. Every key has a
//! default; an unknown key or a value the supervisor cannot honor is an
//! error, so a typo cannot silently run the defaults.

use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Shrink,
    Tmr,
}

impl Policy {
    pub fn name(self) -> &'static str {
        match self {
            Policy::Shrink => "shrink",
            Policy::Tmr => "tmr",
        }
    }
    pub fn quorum(self) -> quorum::Policy {
        match self {
            Policy::Shrink => quorum::Policy::Shrink,
            Policy::Tmr => quorum::Policy::Tmr,
        }
    }
}

/// What the supervisor does when no replica can carry the task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnHalt {
    /// Keep running: no cycles, the heartbeat says `halted`, a configured
    /// restart can end it. The default, for a service manager that would
    /// treat an exit as a crash.
    Idle,
    /// Write the summary and exit 0.
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adapter {
    None,
    Cli,
}

/// A source, a sink or a heartbeat target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    None,
    Tick,
    File(PathBuf),
    Serial { dev: PathBuf, baud: u32 },
    Tcp { host: String, port: u16 },
    Cmd(Vec<String>),
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Endpoint::None => write!(f, "none"),
            Endpoint::Tick => write!(f, "tick"),
            Endpoint::File(p) => write!(f, "file:{}", p.display()),
            Endpoint::Serial { dev, baud } => write!(f, "serial:{}:{baud}", dev.display()),
            Endpoint::Tcp { host, port } => write!(f, "tcp:{host}:{port}"),
            Endpoint::Cmd(argv) => write!(f, "cmd:{}", argv.join(" ")),
        }
    }
}

pub fn parse_endpoint(s: &str) -> Result<Endpoint, String> {
    if s == "none" {
        return Ok(Endpoint::None);
    }
    if s == "tick" {
        return Ok(Endpoint::Tick);
    }
    let (kind, rest) = s
        .split_once(':')
        .ok_or_else(|| format!("endpoint '{s}': expected none, tick, file:, serial:, tcp: or cmd:"))?;
    match kind {
        "file" if !rest.is_empty() => Ok(Endpoint::File(PathBuf::from(rest))),
        "serial" => {
            let (dev, baud) =
                rest.rsplit_once(':').ok_or_else(|| format!("endpoint '{s}': expected serial:DEVICE:BAUD"))?;
            let baud = baud.parse().map_err(|_| format!("endpoint '{s}': bad baud rate '{baud}'"))?;
            if dev.is_empty() {
                return Err(format!("endpoint '{s}': empty device"));
            }
            Ok(Endpoint::Serial { dev: PathBuf::from(dev), baud })
        }
        "tcp" => {
            let (host, port) =
                rest.rsplit_once(':').ok_or_else(|| format!("endpoint '{s}': expected tcp:HOST:PORT"))?;
            let port = port.parse().map_err(|_| format!("endpoint '{s}': bad port '{port}'"))?;
            if host.is_empty() {
                return Err(format!("endpoint '{s}': empty host"));
            }
            Ok(Endpoint::Tcp { host: host.to_string(), port })
        }
        "cmd" => {
            let argv: Vec<String> = rest.split_whitespace().map(String::from).collect();
            if argv.is_empty() {
                return Err(format!("endpoint '{s}': empty command"));
            }
            Ok(Endpoint::Cmd(argv))
        }
        _ => Err(format!("endpoint '{s}': expected none, tick, file:, serial:, tcp: or cmd:")),
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub task: PathBuf,
    pub adapter: Adapter,
    pub input: Endpoint,
    pub period_ms: u64,
    pub output: Endpoint,
    pub dir: PathBuf,
    pub kat_input: PathBuf,
    pub kat_output: PathBuf,
    pub replicas: usize,
    pub strikes: usize,
    pub window: u64,
    pub kat_every: u64,
    pub checkpoint_every: u64,
    pub summary_every: u64,
    pub restart_retired: bool,
    pub cooldown_s: u64,
    pub memory_mb: u64,
    pub cpu_percent: f64,
    pub reply_timeout_ms: u64,
    pub heartbeat: Endpoint,
    pub heartbeat_every_s: u64,
    pub policy: Policy,
    pub on_halt: OnHalt,
    pub record_max: u32,
    pub state_max: u32,
    /// 0 is unlimited; tests and the bench cap a run.
    pub max_cycles: u64,
    pub max_seconds: f64,
    /// Test-only: a stored fault schedule.
    pub faults: Option<PathBuf>,
    /// Test-only: a fourth, unfaulted instance of the task that scores delivered outputs.
    pub oracle: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            task: PathBuf::from("/opt/field/task"),
            adapter: Adapter::None,
            input: Endpoint::Serial { dev: PathBuf::from("/dev/ttyS1"), baud: 9600 },
            period_ms: 1000,
            output: Endpoint::File(PathBuf::from("/var/field/out")),
            dir: PathBuf::from("/var/field"),
            kat_input: PathBuf::from("/opt/field/kat.in"),
            kat_output: PathBuf::from("/opt/field/kat.out"),
            replicas: 3,
            strikes: 3,
            window: 200,
            kat_every: 64,
            checkpoint_every: 100,
            summary_every: 1000,
            restart_retired: false,
            cooldown_s: 600,
            memory_mb: 128,
            cpu_percent: 0.0,
            reply_timeout_ms: 2000,
            heartbeat: Endpoint::None,
            heartbeat_every_s: 3600,
            policy: Policy::Shrink,
            on_halt: OnHalt::Idle,
            record_max: 65536,
            state_max: 65536,
            max_cycles: 0,
            max_seconds: 0.0,
            faults: None,
            oracle: None,
        }
    }
}

/// Split a line into `key` and `value`, with the comment and quotes removed.
fn split_line(line: &str) -> Result<Option<(&str, String)>, String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let (key, rest) = line.split_once('=').ok_or_else(|| format!("expected key = value, got '{line}'"))?;
    let key = key.trim();
    let rest = rest.trim_start();
    let value = if let Some(q) = rest.strip_prefix('"') {
        let end = q.find('"').ok_or_else(|| format!("{key}: unterminated string"))?;
        let after = q[end + 1..].trim();
        if !after.is_empty() && !after.starts_with('#') {
            return Err(format!("{key}: text after the closing quote"));
        }
        q[..end].to_string()
    } else {
        let bare = rest.split('#').next().unwrap_or("").trim();
        if bare.is_empty() {
            return Err(format!("{key}: empty value"));
        }
        bare.to_string()
    };
    Ok(Some((key, value)))
}

fn int<T: std::str::FromStr>(key: &str, v: &str) -> Result<T, String> {
    v.parse().map_err(|_| format!("{key}: expected a number, got '{v}'"))
}

fn boolean(key: &str, v: &str) -> Result<bool, String> {
    match v {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("{key}: expected true or false, got '{v}'")),
    }
}

fn optional_path(v: &str) -> Option<PathBuf> {
    (v != "none" && !v.is_empty()).then(|| PathBuf::from(v))
}

/// Parse the text of a configuration file. Does not touch the file system.
pub fn parse(text: &str) -> Result<Config, String> {
    let mut c = Config::default();
    for (n, line) in text.lines().enumerate() {
        let Some((key, v)) = split_line(line).map_err(|e| format!("line {}: {e}", n + 1))? else { continue };
        let v = v.as_str();
        let r: Result<(), String> = (|| {
            match key {
                "task" => c.task = PathBuf::from(v),
                "adapter" => {
                    c.adapter = match v {
                        "none" => Adapter::None,
                        "cli" => Adapter::Cli,
                        _ => return Err(format!("adapter: expected none or cli, got '{v}'")),
                    }
                }
                "input" => c.input = parse_endpoint(v)?,
                "period_ms" => c.period_ms = int(key, v)?,
                "output" => c.output = parse_endpoint(v)?,
                "dir" => c.dir = PathBuf::from(v),
                "kat_input" => c.kat_input = PathBuf::from(v),
                "kat_output" => c.kat_output = PathBuf::from(v),
                "replicas" => c.replicas = int(key, v)?,
                "strikes" => c.strikes = int(key, v)?,
                "window" => c.window = int(key, v)?,
                "kat_every" => c.kat_every = int(key, v)?,
                "checkpoint_every" => c.checkpoint_every = int(key, v)?,
                "summary_every" => c.summary_every = int(key, v)?,
                "restart_retired" => c.restart_retired = boolean(key, v)?,
                "cooldown_s" => c.cooldown_s = int(key, v)?,
                "memory_mb" => c.memory_mb = int(key, v)?,
                "cpu_percent" => c.cpu_percent = int(key, v)?,
                "reply_timeout_ms" => c.reply_timeout_ms = int(key, v)?,
                "heartbeat" => c.heartbeat = parse_endpoint(v)?,
                "heartbeat_every_s" => c.heartbeat_every_s = int(key, v)?,
                "policy" => {
                    c.policy = match v {
                        "shrink" => Policy::Shrink,
                        "tmr" => Policy::Tmr,
                        _ => return Err(format!("policy: expected shrink or tmr, got '{v}'")),
                    }
                }
                "on_halt" => {
                    c.on_halt = match v {
                        "idle" => OnHalt::Idle,
                        "exit" => OnHalt::Exit,
                        _ => return Err(format!("on_halt: expected idle or exit, got '{v}'")),
                    }
                }
                "record_max" => c.record_max = int(key, v)?,
                "state_max" => c.state_max = int(key, v)?,
                "max_cycles" => c.max_cycles = int(key, v)?,
                "max_seconds" => c.max_seconds = int(key, v)?,
                "faults" => c.faults = optional_path(v),
                "oracle" => c.oracle = optional_path(v),
                _ => return Err(format!("unknown key '{key}'")),
            }
            Ok(())
        })();
        r.map_err(|e| format!("line {}: {e}", n + 1))?;
    }
    validate(&c)?;
    Ok(c)
}

fn validate(c: &Config) -> Result<(), String> {
    let fixed = |what: &str, got: u64, want: u64| {
        if got != want {
            Err(format!("{what} = {got}: fixed at {want} by quorum 0.4.0; configurable in a later release"))
        } else {
            Ok(())
        }
    };
    fixed("replicas", c.replicas as u64, quorum::REPLICAS as u64)?;
    fixed("strikes", c.strikes as u64, quorum::STRIKES as u64)?;
    fixed("window", c.window, quorum::STRIKE_WINDOW)?;
    if c.kat_every == 0 {
        return Err("kat_every must be at least 1".into());
    }
    if c.checkpoint_every == 0 {
        return Err("checkpoint_every must be at least 1".into());
    }
    if c.reply_timeout_ms == 0 {
        return Err("reply_timeout_ms must be at least 1".into());
    }
    if c.record_max == 0 {
        return Err("record_max must be at least 1".into());
    }
    if c.input == Endpoint::None {
        return Err("input = none: the supervisor needs a source".into());
    }
    if c.output == Endpoint::Tick {
        return Err("output = tick: not a sink".into());
    }
    match &c.heartbeat {
        Endpoint::Tick | Endpoint::Cmd(_) => {
            return Err(format!("heartbeat = {}: use serial:, tcp:, file: or none", c.heartbeat))
        }
        _ => {}
    }
    if c.faults.is_some() && c.adapter == Adapter::Cli {
        return Err(
            "faults with adapter = cli: kill and hang need a long-lived task process; use adapter = none".into()
        );
    }
    if !(0.0..=100.0).contains(&c.cpu_percent) {
        return Err(format!("cpu_percent = {}: expected 0 to 100", c.cpu_percent));
    }
    Ok(())
}

/// Read and parse a configuration file.
pub fn load(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The run's identity: a hash of the keys that decide results, with the
/// known-answer records and the fault schedule by content. A checkpoint
/// resumes only a run with the same identity. Keys that only tune a run
/// (period, limits, directories, restarts, heartbeat, caps) are left out,
/// so an operator can retune without losing the run.
pub fn identity(c: &Config, kat_in: &[u8], kat_out: &[u8], faults_text: Option<&str>) -> u64 {
    let adapter = match c.adapter {
        Adapter::None => "none",
        Adapter::Cli => "cli",
    };
    let text = format!(
        "task={}\nadapter={adapter}\ninput={}\noutput={}\npolicy={}\nreplicas={}\nstrikes={}\nwindow={}\nkat_every={}\nrecord_max={}\nstate_max={}\nkat_input={:016x}\nkat_output={:016x}\nfaults={}\noracle={}\n",
        c.task.display(),
        c.input,
        c.output,
        c.policy.name(),
        c.replicas,
        c.strikes,
        c.window,
        c.kat_every,
        c.record_max,
        c.state_max,
        crate::hash::of(kat_in),
        crate::hash::of(kat_out),
        faults_text.map_or("-".to_string(), |t| format!("{:016x}", crate::hash::of(t.as_bytes()))),
        c.oracle.as_ref().map_or("-".to_string(), |p| p.display().to_string()),
    );
    crate::hash::of(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
# a comment
task        = "/opt/field/task"        # the replica executable
adapter     = "cli"
input       = "serial:/dev/ttyS1:9600"
period_ms   = 500
output      = file:/var/field/out
dir         = "/var/field"
kat_input   = "/opt/field/kat.in"
kat_output  = "/opt/field/kat.out"
replicas    = 3
strikes     = 3
window      = 200
kat_every   = 32
checkpoint_every = 50
restart_retired  = true
cooldown_s  = 60
memory_mb   = 64
cpu_percent = 5
reply_timeout_ms = 1500
heartbeat   = "tcp:10.0.0.2:5000"
heartbeat_every_s = 60
policy = "tmr"
on_halt = "exit"
record_max = 1024
state_max = 16
max_cycles = 10
max_seconds = 2
oracle = "/opt/field/task"
"#;

    #[test]
    fn defaults_are_the_spec_table() {
        let c = parse("").unwrap();
        assert_eq!(c.task, PathBuf::from("/opt/field/task"));
        assert_eq!(c.input, Endpoint::Serial { dev: "/dev/ttyS1".into(), baud: 9600 });
        assert_eq!(c.output, Endpoint::File("/var/field/out".into()));
        assert_eq!((c.period_ms, c.kat_every, c.checkpoint_every), (1000, 64, 100));
        assert_eq!((c.strikes, c.window, c.replicas), (3, 200, 3));
        assert_eq!((c.memory_mb, c.cpu_percent, c.reply_timeout_ms, c.cooldown_s), (128, 0.0, 2000, 600));
        assert!(!c.restart_retired);
        assert_eq!(c.heartbeat, Endpoint::None);
        assert_eq!(c.policy, Policy::Shrink);
    }

    #[test]
    fn a_full_file_parses() {
        let c = parse(FULL).unwrap();
        assert_eq!(c.adapter, Adapter::Cli);
        assert_eq!(c.period_ms, 500);
        assert_eq!(c.output, Endpoint::File("/var/field/out".into()));
        assert_eq!(c.heartbeat, Endpoint::Tcp { host: "10.0.0.2".into(), port: 5000 });
        assert_eq!((c.kat_every, c.checkpoint_every, c.cooldown_s), (32, 50, 60));
        assert!(c.restart_retired);
        assert_eq!(c.cpu_percent, 5.0);
        assert_eq!(c.policy, Policy::Tmr);
        assert_eq!(c.on_halt, OnHalt::Exit);
        assert_eq!((c.record_max, c.state_max, c.max_cycles), (1024, 16, 10));
        assert_eq!(c.max_seconds, 2.0);
        assert_eq!(c.oracle, Some(PathBuf::from("/opt/field/task")));
        assert_eq!(c.faults, None);
    }

    #[test]
    fn bad_input_is_refused() {
        for (text, needle) in [
            ("nonsense = 1", "unknown key"),
            ("strikes = 4", "fixed at 3"),
            ("window = 100", "fixed at 200"),
            ("replicas = 2", "fixed at 3"),
            ("kat_every = 0", "kat_every"),
            ("checkpoint_every = 0", "checkpoint_every"),
            ("input = \"none\"", "needs a source"),
            ("input = \"udp:1\"", "endpoint"),
            ("input = \"serial:/dev/ttyS1\"", "serial:DEVICE:BAUD"),
            ("output = \"tick\"", "not a sink"),
            ("heartbeat = \"cmd:echo\"", "heartbeat"),
            ("faults = \"f.txt\"\nadapter = \"cli\"", "long-lived"),
            ("policy = \"mean\"", "policy"),
            ("on_halt = \"reboot\"", "on_halt"),
            ("restart_retired = yes", "true or false"),
            ("period_ms = fast", "number"),
            ("task = \"unterminated", "unterminated"),
            ("task =", "empty value"),
            ("just text", "key = value"),
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.contains(needle), "{text}: {e}");
        }
    }

    #[test]
    fn endpoints_parse_and_print() {
        for s in ["none", "tick", "file:/a/b", "serial:/dev/ttyUSB0:115200", "tcp:host:9", "cmd:prog -x 1"] {
            assert_eq!(parse_endpoint(s).unwrap().to_string(), s);
        }
        assert_eq!(parse_endpoint("cmd:  a   b ").unwrap(), Endpoint::Cmd(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn identity_covers_the_keys_that_decide_results() {
        let base = parse(FULL).unwrap();
        let id = |c: &Config| identity(c, b"in", b"out", None);
        let same = id(&base);
        let mut tuned = base.clone();
        tuned.period_ms = 1;
        tuned.dir = "/tmp/x".into();
        tuned.checkpoint_every = 7;
        tuned.summary_every = 7;
        tuned.restart_retired = false;
        tuned.cooldown_s = 1;
        tuned.memory_mb = 1;
        tuned.cpu_percent = 50.0;
        tuned.reply_timeout_ms = 1;
        tuned.heartbeat = Endpoint::None;
        tuned.heartbeat_every_s = 1;
        tuned.max_cycles = 1;
        tuned.max_seconds = 1.0;
        tuned.on_halt = OnHalt::Idle;
        assert_eq!(id(&tuned), same, "tuning keys change nothing");
        let mut other = base.clone();
        other.policy = Policy::Shrink;
        assert_ne!(id(&other), same);
        let mut other = base.clone();
        other.kat_every = 64;
        assert_ne!(id(&other), same);
        let mut other = base.clone();
        other.task = "/opt/other".into();
        assert_ne!(id(&other), same);
        assert_ne!(identity(&base, b"in2", b"out", None), same);
        assert_ne!(identity(&base, b"in", b"out2", None), same);
        assert_ne!(identity(&base, b"in", b"out", Some("kill:0@1")), same);
    }
}
