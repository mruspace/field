//! The supervisor: start-up, the cycle, retirement, checkpoint and resume,
//! halt, restarts and the clean stop (docs/spec.md, sections 4 to 8).
//!
//! Each cycle: acquire one input record, hand it to every live replica with
//! the shared state, decide with the `quorum` crate on a hash of each reply,
//! compare the agreeing replies byte for byte, deliver one, and keep the
//! health record. A replica that is wrong, hung or dead is retired. The
//! decision rule is the flight repository's `quorum` crate, unchanged.

#![allow(clippy::needless_range_loop)]

use crate::checkpoint::{Caught, Checkpoint};
use crate::config::{Config, OnHalt, Policy};
use crate::faults::{Fault, Kind};
use crate::hash::reply_value;
use crate::log::EventLog;
use crate::protocol::{Limits, Response, FLAG_SELF_CHECK};
use crate::replica::{Kat, ProcessLimits, Replica};
use crate::sink::{Line, Sink};
use crate::source::{Acquired, Source};
use crate::stop;
use crate::summary::{self, Footprint, Report};
use quorum::{decide, Health, Mode, Outcome, Reply, REPLICAS};
use std::path::Path;
use std::time::{Duration, Instant};

/// True every `period` cycles. (Written with `checked_rem` so the code
/// builds on Rust 1.86 and passes the newer `is_multiple_of` lint.)
pub fn every(cycle: u64, period: u64) -> bool {
    cycle.checked_rem(period) == Some(0)
}

pub fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Vote => "vote",
        Mode::Compare => "compare",
        Mode::SelfCheck => "selfcheck",
        Mode::Halted => "halted",
    }
}

enum Flow {
    Continue,
    /// Why the run stopped, and the exit code.
    Stop(String, i32),
    Halt,
}

struct Run {
    cfg: Config,
    kat: Kat,
    faults: Vec<Fault>,
    faults_configured: bool,
    limits: Limits,
    timeout: Duration,
    oracle: Option<Replica>,
    replicas: [Option<Replica>; REPLICAS],
    health: Health,
    cp: Checkpoint,
    log: EventLog,
    sink: Box<dyn Sink>,
    source: Box<dyn Source>,
    heartbeat: Option<Box<dyn Line>>,
    heartbeat_sent: u64,
    heartbeat_failed: u64,
    next_heartbeat: Instant,
    start: Instant,
    deadline: Option<Instant>,
    last_cycle_start: Option<Instant>,
    halted_at: Option<u64>,
    /// Set at the stop, before the replicas are ended, for the summary.
    alive_at_end: Option<usize>,
    oracle_configured: bool,
}

/// Run under a configuration; returns the exit code.
pub fn run(cfg: Config) -> i32 {
    match start(cfg) {
        Ok(mut run) => run.main_loop(),
        Err((msg, code)) => {
            eprintln!("field: {msg}");
            code
        }
    }
}

fn read_file(what: &str, path: &Path) -> Result<Vec<u8>, (String, i32)> {
    std::fs::read(path).map_err(|e| (format!("{what} {}: {e}", path.display()), 2))
}

fn start(cfg: Config) -> Result<Run, (String, i32)> {
    let kat = Kat { input: read_file("kat_input", &cfg.kat_input)?, output: read_file("kat_output", &cfg.kat_output)? };
    if kat.input.len() > cfg.record_max as usize {
        return Err((format!("kat_input is {} bytes, over record_max {}", kat.input.len(), cfg.record_max), 2));
    }
    let (faults_text, faults) = match &cfg.faults {
        Some(p) => {
            let (text, faults) = crate::faults::load(p).map_err(|e| (e, 2))?;
            (Some(text), faults)
        }
        None => (None, Vec::new()),
    };
    let identity = crate::config::identity(&cfg, &kat.input, &kat.output, faults_text.as_deref());

    std::fs::create_dir_all(&cfg.dir).map_err(|e| (format!("dir {}: {e}", cfg.dir.display()), 1))?;
    let limits = ProcessLimits { memory_mb: cfg.memory_mb };
    limits.apply();
    stop::install_handlers();

    let now = summary::now_unix();
    let cp_path = cfg.dir.join("checkpoint");
    let mut cp = match std::fs::read_to_string(&cp_path) {
        Err(_) => Checkpoint::fresh(identity, now),
        Ok(text) => match Checkpoint::from_text(&text) {
            Some(c) if c.config == identity => c,
            _ => {
                return Err((
                    format!(
                        "checkpoint in {} is from another configuration; move it away to start fresh",
                        cfg.dir.display()
                    ),
                    2,
                ))
            }
        },
    };
    let resuming = cp.cycle > 0 || cp.resumes > 0 || cp.halted || cp.log_len > 0;
    if resuming {
        cp.resumes += 1;
    }
    let mut log = EventLog::open(&cfg.dir.join("events.csv"), resuming.then(|| (cp.log_len, cp.streak.take())))
        .map_err(|e| (format!("events.csv: {e}"), 1))?;
    if resuming {
        let detail = if cp.halted { format!("{} halted", cp.resumes) } else { cp.resumes.to_string() };
        log.event(cp.cycle, "resumed", &detail);
        log.flush();
    }

    let timeout = Duration::from_millis(cfg.reply_timeout_ms);
    let sink = crate::sink::open(&cfg.output, resuming.then_some(cp.out_len), timeout)
        .map_err(|e| (format!("output {e}"), 1))?;
    let source = crate::source::open(&cfg.input).map_err(|e| (format!("input {e}"), 1))?;
    let heartbeat = crate::sink::open_line(&cfg.heartbeat).map_err(|e| (format!("heartbeat {e}"), 1))?;

    let start = Instant::now();
    let proto = Limits { record_max: cfg.record_max, state_max: cfg.state_max };
    let mut run = Run {
        faults_configured: cfg.faults.is_some(),
        deadline: (cfg.max_seconds > 0.0).then(|| start + Duration::from_secs_f64(cfg.max_seconds)),
        next_heartbeat: start,
        cfg,
        kat,
        faults,
        limits: proto,
        timeout,
        oracle: None,
        replicas: [None, None, None],
        health: Health::from_raw(cp.strikes, cp.strike_count),
        cp,
        log,
        sink,
        source,
        heartbeat,
        heartbeat_sent: 0,
        heartbeat_failed: 0,
        start,
        last_cycle_start: None,
        halted_at: None,
        alive_at_end: None,
        oracle_configured: false,
    };
    if run.cp.halted {
        run.halted_at = Some(run.cp.cycle);
    } else {
        for i in 0..REPLICAS {
            if run.cp.alive[i] {
                run.start_replica(i, "startup").map_err(|e| (e, 1))?;
            }
        }
    }
    if let Some(path) = run.cfg.oracle.clone() {
        let mut o = Replica::spawn(&path, run.cfg.adapter, limits)
            .map_err(|e| (format!("oracle {}: {e}", path.display()), 2))?;
        if !o.kat(&run.kat, run.cp.cycle, run.limits, run.timeout) {
            return Err((format!("oracle {}: failed the known-answer test", path.display()), 2));
        }
        run.oracle = Some(o);
        run.oracle_configured = true;
    }
    run.log.flush();
    // a resume is checkpointed at once, so a second cut before the next
    // checkpoint cannot erase the `resumed` line and its count
    if resuming {
        run.checkpoint();
    }
    // the start-up (spawning, the known-answer tests) counts as work under
    // the CPU share, so the whole life of the process stays under it
    run.throttle(run.start.elapsed(), Duration::ZERO);
    Ok(run)
}

impl Run {
    fn alive(&self) -> usize {
        self.replicas.iter().filter(|r| r.is_some()).count()
    }

    fn policy(&self) -> quorum::Policy {
        self.cfg.policy.quorum()
    }

    /// Spawn the task into slot `i` and run its known-answer test twice. On
    /// a failure the slot is retired. `Err` only when the task cannot be
    /// started at all.
    fn start_replica(&mut self, i: usize, reason: &str) -> Result<bool, String> {
        let limits = ProcessLimits { memory_mb: self.cfg.memory_mb };
        let mut r = match Replica::spawn(&self.cfg.task, self.cfg.adapter, limits) {
            Ok(r) => r,
            Err(e) if reason == "startup" => return Err(format!("task {}: {e}", self.cfg.task.display())),
            Err(_) => {
                self.retire_slot(i, reason);
                return Ok(false);
            }
        };
        // a stuck fault from the schedule is persistent: a fresh process in
        // the slot has it too, after a resume or a restart
        r.injected.stuck =
            self.faults.iter().any(|f| f.kind == Kind::Stuck && f.replica == i && f.cycle <= self.cp.cycle);
        self.cp.kat_tests += 2;
        let cycle = self.cp.cycle;
        // the first answer includes the process start: two seconds more than
        // the reply timeout, once
        let first = self.timeout + Duration::from_secs(2);
        let ok = r.kat(&self.kat, cycle, self.limits, first) && r.kat(&self.kat, cycle, self.limits, self.timeout);
        if ok {
            self.replicas[i] = Some(r);
            self.cp.alive[i] = true;
            self.cp.retired_at[i] = None;
            self.health.clear(i);
            Ok(true)
        } else {
            r.retire();
            self.retire_slot(i, reason);
            Ok(false)
        }
    }

    /// Mark slot `i` retired and log it.
    fn retire_slot(&mut self, i: usize, why: &str) {
        if let Some(r) = self.replicas[i].take() {
            r.retire();
        }
        self.cp.alive[i] = false;
        self.cp.retired_at[i] = Some(summary::now_unix());
        let cycle = self.cp.cycle;
        self.log.event(cycle, "retired", &format!("{i} {why}"));
    }

    /// Restart retired slots whose cool-down has passed. True if any came back.
    fn try_restarts(&mut self) -> bool {
        if !self.cfg.restart_retired {
            return false;
        }
        let now = summary::now_unix();
        let mut any = false;
        for i in 0..REPLICAS {
            if self.cp.alive[i] {
                continue;
            }
            let Some(at) = self.cp.retired_at[i] else { continue };
            if now < at {
                // the clock went backwards after a reset: wait a full cool-down again
                self.cp.retired_at[i] = Some(now);
                continue;
            }
            // whole seconds: strictly after at + cooldown_s is at least
            // cooldown_s seconds after the retirement, whatever the fractions
            if now <= at + self.cfg.cooldown_s {
                continue;
            }
            if self.start_replica(i, "restart").unwrap_or(false) {
                let cycle = self.cp.cycle;
                self.log.event(cycle, "restarted", &i.to_string());
                any = true;
            }
        }
        any
    }

    fn checkpoint(&mut self) {
        match self.log.sync() {
            Ok(len) => self.cp.log_len = len,
            Err(e) => eprintln!("field: events.csv: {e}"),
        }
        match self.sink.sync() {
            Ok(len) => self.cp.out_len = len,
            Err(e) => eprintln!("field: output: {e}"),
        }
        (self.cp.strikes, self.cp.strike_count) = self.health.to_raw();
        self.cp.streak = self.log.streak();
        if let Err(e) = self.cp.save(&self.cfg.dir.join("checkpoint")) {
            eprintln!("field: checkpoint: {e}");
        }
    }

    fn summary_text(&self, stopped: &str) -> String {
        let (supervisor_kib, cpu_self) = summary::usage_of(libc::RUSAGE_SELF);
        let (replica_kib, cpu_children) = summary::usage_of(libc::RUSAGE_CHILDREN);
        let alive = self.alive_at_end.unwrap_or_else(|| self.alive());
        let mode = if self.cp.halted { Mode::Halted } else { quorum::mode(self.policy(), alive) };
        summary::text(&Report {
            cfg: &self.cfg,
            cp: &self.cp,
            mode: mode_name(mode),
            alive,
            stopped,
            halted_at: self.halted_at,
            footprint: Footprint {
                supervisor_kib,
                replica_kib,
                cpu_s: cpu_self + cpu_children,
                wall_s: self.start.elapsed().as_secs_f64(),
            },
            now_unix: summary::now_unix(),
            heartbeat: self.heartbeat.as_ref().map(|_| (self.heartbeat_sent, self.heartbeat_failed)),
            faults_configured: self.faults_configured,
            oracle: self.oracle_configured,
        })
    }

    /// The progress line and summary.txt.
    fn report(&mut self) {
        let alive = self.alive();
        let mut detail = format!("delivered={} detected={} alive={alive}", self.cp.delivered, self.cp.detected);
        if self.oracle.is_some() {
            detail.push_str(&format!(" wrong={}", self.cp.wrong));
        }
        let cycle = self.cp.cycle;
        self.log.event(cycle, "progress", &detail);
        let text = self.summary_text("running");
        if let Err(e) = summary::write(&self.cfg.dir, &text) {
            eprintln!("field: summary.txt: {e}");
        }
    }

    fn heartbeat_if_due(&mut self, mode: Mode) {
        let Some(hb) = self.heartbeat.as_mut() else { return };
        if Instant::now() < self.next_heartbeat {
            return;
        }
        self.next_heartbeat = Instant::now() + Duration::from_secs(self.cfg.heartbeat_every_s.max(1));
        let alive = self.replicas.iter().filter(|r| r.is_some()).count();
        let line = format!(
            "MRU,{},{},{alive},{},{},{}",
            self.cp.cycle,
            mode_name(mode),
            self.cp.delivered,
            self.cp.detected,
            self.cp.resumes
        );
        match hb.line(&line) {
            Ok(()) => self.heartbeat_sent += 1,
            Err(e) => {
                self.heartbeat_failed += 1;
                eprintln!("field: heartbeat: {e}");
            }
        }
    }

    fn main_loop(&mut self) -> i32 {
        loop {
            let flow = if self.cp.halted { self.idle() } else { self.cycle() };
            match flow {
                Flow::Continue => {}
                Flow::Halt => {
                    self.halted_at = Some(self.cp.cycle);
                    self.cp.halted = true;
                    self.checkpoint();
                    if self.cfg.on_halt == OnHalt::Exit {
                        return self.finish("halted", 0);
                    }
                    let text = self.summary_text("running");
                    let _ = summary::write(&self.cfg.dir, &text);
                }
                Flow::Stop(why, code) => return self.finish(&why, code),
            }
        }
    }

    /// The halted loop: nothing is acquired or delivered; the heartbeat says
    /// `halted`; a restart of a retired replica, if configured, ends it.
    fn idle(&mut self) -> Flow {
        loop {
            if stop::requested() {
                return Flow::Stop("signal".into(), 0);
            }
            if let Some(d) = self.deadline {
                if Instant::now() >= d {
                    return Flow::Stop("time-limit".into(), 0);
                }
            }
            if self.try_restarts() {
                self.cp.halted = false;
                self.halted_at = None;
                self.checkpoint();
                return Flow::Continue;
            }
            self.log.flush();
            self.heartbeat_if_due(Mode::Halted);
            std::thread::sleep(stop::SLICE);
        }
    }

    fn cycle(&mut self) -> Flow {
        // 1, 2: stop, caps
        if stop::requested() {
            return Flow::Stop("signal".into(), 0);
        }
        if let Some(d) = self.deadline {
            if Instant::now() >= d {
                return Flow::Stop("time-limit".into(), 0);
            }
        }
        if self.cfg.max_cycles > 0 && self.cp.cycle >= self.cfg.max_cycles {
            return Flow::Stop("completed".into(), 0);
        }
        // 3: restarts
        if self.try_restarts() {
            self.checkpoint();
        }
        // 4: mode
        let alive = self.alive();
        let mode = quorum::mode(self.policy(), alive);
        if mode == Mode::Halted {
            let cycle = self.cp.cycle;
            self.log.event(cycle, "halted", &alive.to_string());
            return Flow::Halt;
        }
        // 5: pace
        if let Some(prev) = self.last_cycle_start {
            if !stop::sleep_until(prev + Duration::from_millis(self.cfg.period_ms)) {
                return Flow::Stop("signal".into(), 0);
            }
        }
        // 6: acquire
        let next_cycle = self.cp.cycle + 1;
        let input = match self.source.next(next_cycle, self.cfg.record_max, self.timeout) {
            Ok(Acquired::Record(r)) => r,
            Ok(Acquired::Overlong(r, len)) => {
                self.log.event(self.cp.cycle, "source", &format!("overlong {len}"));
                r
            }
            Ok(Acquired::End) => return Flow::Stop("end-of-input".into(), 0),
            Ok(Acquired::Stop) => return Flow::Stop("signal".into(), 0),
            Ok(Acquired::Failed(why)) => {
                let cycle = self.cp.cycle;
                self.log.event(cycle, "source", &why);
                self.log.flush();
                self.last_cycle_start = Some(Instant::now());
                return Flow::Continue;
            }
            Err(e) => return Flow::Stop(format!("input-error {e}"), 1),
        };
        // 7
        let cycle = next_cycle;
        let cycle_start = Instant::now();
        self.last_cycle_start = Some(cycle_start);
        let mut lost_wait = Duration::ZERO;
        let mut flagged = [false; REPLICAS];
        let mut how: [&str; REPLICAS] = ["outvoted"; REPLICAS];
        let mut state_changed = false;
        // 8: scheduled faults
        let due: Vec<Fault> = self.faults.iter().filter(|f| f.cycle == cycle).copied().collect();
        for f in due {
            let Some(r) = self.replicas[f.replica].as_mut() else { continue };
            self.log.event(cycle, "injected", &format!("{}:{}", f.kind.name(), f.replica));
            if f.kind != Kind::Kill && self.cp.fault_at[f.replica].is_none() {
                self.cp.fault_at[f.replica] = Some(cycle);
                self.cp.faults_injected += 1;
            }
            match f.kind {
                Kind::Kill => r.signal(libc::SIGKILL),
                Kind::Hang => r.signal(libc::SIGSTOP),
                Kind::Stuck => r.injected.stuck = true,
                Kind::Corrupt | Kind::Corrupt2 => r.injected.corrupt = Some(f.kind),
            }
        }
        // 9: ask, pipelined. A send has the reply timeout to complete: a
        // replica that stopped reading leaves its pipe full and is lost
        // there. The reply deadline for every replica starts after the last
        // send, so a blocked send costs the cycle one timeout and costs the
        // healthy replicas nothing.
        let self_check = mode == Mode::SelfCheck;
        let flags = if self_check { FLAG_SELF_CHECK } else { 0 };
        let mut replies = [Reply::Absent; REPLICAS];
        let mut resps: [Option<Response>; REPLICAS] = [None, None, None];
        let mut lost = [false; REPLICAS];
        for i in 0..REPLICAS {
            let Some(r) = self.replicas[i].as_mut() else { continue };
            let sent = Instant::now();
            if r.send(cycle, flags, &input, &self.cp.state, sent + self.timeout).is_err() {
                lost[i] = true;
                lost_wait += sent.elapsed();
            }
        }
        let deadline = Instant::now() + self.timeout;
        for i in 0..REPLICAS {
            let Some(r) = self.replicas[i].as_mut() else { continue };
            if !lost[i] {
                let asked = Instant::now();
                match r.receive(self_check, self.limits, deadline) {
                    Ok(mut resp) => {
                        crate::faults::inject_bytes(&mut r.injected, &mut resp);
                        let first = reply_value(&resp.output, &resp.state);
                        replies[i] = match &resp.second {
                            Some(second) => Reply::Pair(first, reply_value(second, &resp.state)),
                            None => Reply::Single(first),
                        };
                        resps[i] = Some(resp);
                        continue;
                    }
                    Err(_) => lost_wait += asked.elapsed(),
                }
            }
            // died, hung or answered nonsense
            self.replicas[i].take().unwrap().retire();
            self.cp.alive[i] = false;
            self.cp.retired_at[i] = Some(summary::now_unix());
            flagged[i] = true;
            how[i] = "lost";
            state_changed = true;
            self.log.event(cycle, "lost", &i.to_string());
        }
        let oracle_output = match self.oracle.as_mut() {
            Some(o) => match o.ask(cycle, 0, &input, &self.cp.state, self.limits, self.timeout) {
                Ok(resp) => Some(resp.output),
                Err(e) => return Flow::Stop(format!("input-error oracle lost ({e})"), 2),
            },
            None => None,
        };
        // 10: decide
        let verdict = decide(self.policy(), &replies);
        let mut outcome = verdict.outcome;
        let mut detected_logged = false;
        // 11: the hashes agreed; the bytes must too
        let mut chosen: Option<usize> = None;
        if let Outcome::Deliver(h) = outcome {
            let first = |r: &Reply| match r {
                Reply::Single(v) | Reply::Pair(v, _) => Some(*v),
                Reply::Absent => None,
            };
            let agreeing: Vec<usize> = (0..REPLICAS).filter(|i| first(&replies[*i]) == Some(h)).collect();
            let c = agreeing[0];
            chosen = Some(c);
            let same_bytes = |a: &Response, b: &Response| a.output == b.output && a.state == b.state;
            let cr = resps[c].as_ref().unwrap();
            let mut agree = agreeing.iter().all(|i| same_bytes(resps[*i].as_ref().unwrap(), cr));
            if self_check {
                agree &= cr.second.as_ref() == Some(&cr.output);
            }
            if !agree {
                outcome = Outcome::Detected;
                chosen = None;
                self.log.event(cycle, "detected", &format!("{} bytes", mode_name(verdict.mode)));
                detected_logged = true;
            }
        }
        // 12: dissent
        for i in 0..REPLICAS {
            if verdict.dissent[i] && self.replicas[i].is_some() {
                self.log.event(cycle, "outvoted", &i.to_string());
            }
            flagged[i] |= verdict.dissent[i] || (replies[i] != Reply::Absent && outcome == Outcome::Detected);
        }
        // 13: health, diagnosis, the periodic test
        if self.cfg.policy == Policy::Shrink {
            for i in 0..REPLICAS {
                if !verdict.dissent[i] || self.replicas[i].is_none() || !self.health.strike(i, cycle) {
                    continue;
                }
                self.cp.kat_tests += 1;
                let ok = self.replicas[i].as_mut().unwrap().kat(&self.kat, cycle, self.limits, self.timeout);
                if ok {
                    self.health.clear(i);
                    self.log.event(cycle, "cleared", &i.to_string());
                } else {
                    self.retire_now(i, cycle, "strikes");
                    flagged[i] = true;
                    how[i] = "strikes";
                    state_changed = true;
                }
            }
            if verdict.diagnose {
                for i in 0..REPLICAS {
                    if self.replicas[i].is_none() {
                        continue;
                    }
                    self.cp.kat_tests += 1;
                    if !self.replicas[i].as_mut().unwrap().kat(&self.kat, cycle, self.limits, self.timeout) {
                        self.retire_now(i, cycle, "diagnose");
                        flagged[i] = true;
                        how[i] = "diagnose";
                        state_changed = true;
                    }
                }
            }
            if mode == Mode::SelfCheck && every(cycle, self.cfg.kat_every) {
                for i in 0..REPLICAS {
                    if self.replicas[i].is_none() {
                        continue;
                    }
                    self.cp.kat_tests += 1;
                    if !self.replicas[i].as_mut().unwrap().kat(&self.kat, cycle, self.limits, self.timeout) {
                        self.retire_now(i, cycle, "periodic");
                        flagged[i] = true;
                        how[i] = "periodic";
                        state_changed = true;
                        outcome = Outcome::Detected;
                        chosen = None;
                    }
                }
            }
        }
        // 14: detection bookkeeping
        for i in 0..REPLICAS {
            if let (Some(at), true) = (self.cp.fault_at[i], flagged[i]) {
                let how = how[i];
                let kind = self
                    .faults
                    .iter()
                    .rfind(|f| f.replica == i && f.cycle <= cycle && f.kind != Kind::Kill)
                    .map_or(Kind::Corrupt, |f| f.kind);
                self.cp.caught.push(Caught { kind, replica: i, at, caught: cycle, how: how.to_string() });
                self.cp.fault_at[i] = None;
            }
        }
        // 15: deliver or not
        match (outcome, chosen) {
            (Outcome::Deliver(_), Some(c)) => {
                let resp = resps[c].take().unwrap();
                if let Err(e) = self.sink.deliver(cycle, &resp.output) {
                    self.cp.cycle = cycle;
                    return Flow::Stop(format!("sink-error {e}"), 1);
                }
                self.cp.state = resp.state;
                self.cp.delivered += 1;
                self.cp.last_delivered = cycle;
                if let Some(truth) = oracle_output {
                    if truth != resp.output {
                        self.cp.wrong += 1;
                        self.log.event(cycle, "wrong", mode_name(verdict.mode));
                    }
                }
            }
            _ => {
                self.cp.detected += 1;
                if !detected_logged {
                    // the mode the decision was made in: on the cycle a
                    // replica is lost, that is one step down from the cycle's
                    self.log.event(cycle, "detected", mode_name(verdict.mode));
                }
            }
        }
        // 16
        self.cp.cycle = cycle;
        self.heartbeat_if_due(mode);
        // 17: progress and summary
        if every(cycle, self.cfg.summary_every) {
            self.report();
        }
        // 18: checkpoint, else just hand the log to the kernel
        if state_changed || every(cycle, self.cfg.checkpoint_every) {
            self.checkpoint();
        } else {
            self.log.flush();
        }
        // 19: CPU share
        self.throttle(cycle_start.elapsed(), lost_wait);
        Flow::Continue
    }

    /// CPU share: after `work` of computation, sleep so that the work took at
    /// most `cpu_percent` of the wall time. `lost_wait`, time spent waiting
    /// for a replica that never answered, is not work. The nap never runs
    /// past the run-time cap and ends early on a stop.
    fn throttle(&self, work: Duration, lost_wait: Duration) {
        let p = self.cfg.cpu_percent;
        if p <= 0.0 || p >= 100.0 {
            return;
        }
        let mut nap = work.saturating_sub(lost_wait).mul_f64((100.0 - p) / p);
        if let Some(d) = self.deadline {
            nap = nap.min(d.saturating_duration_since(Instant::now()));
        }
        stop::sleep_until(Instant::now() + nap);
    }

    /// Retire a replica mid-cycle (a failed known-answer test).
    fn retire_now(&mut self, i: usize, cycle: u64, why: &str) {
        if let Some(r) = self.replicas[i].take() {
            r.retire();
        }
        self.cp.alive[i] = false;
        self.cp.retired_at[i] = Some(summary::now_unix());
        self.log.event(cycle, "retired", &format!("{i} {why}"));
    }

    fn finish(&mut self, why: &str, code: i32) -> i32 {
        let cycle = self.cp.cycle;
        self.log.event(cycle, "stopped", why);
        self.log.close_streak();
        self.checkpoint();
        self.alive_at_end = Some(self.alive());
        // the replicas end before the footprint is read: getrusage counts
        // children once they are reaped
        for r in self.replicas.iter_mut().filter_map(Option::take) {
            r.stop(Duration::from_millis(500));
        }
        if let Some(o) = self.oracle.take() {
            o.stop(Duration::from_millis(500));
        }
        let text = self.summary_text(why.split(' ').next().unwrap_or(why));
        if let Err(e) = summary::write(&self.cfg.dir, &text) {
            eprintln!("field: summary.txt: {e}");
        }
        print!("{text}");
        code
    }
}
