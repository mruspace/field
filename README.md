<p align="center">
  <a href="https://mru.space">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://mru.space/assets/readme/mru-github-dark.gif">
      <img src="https://mru.space/assets/readme/mru-github-light.gif" alt="Mru" width="120" height="120">
    </picture>
  </a>
</p>

# Mru Field

Software for hard-to-reach systems on Earth: ocean observatories, buoys and moorings, offshore and remote sites, polar and seismic stations, anything that runs for months between visits. The supervisor runs an operator's task as three replicas on the system's own processor, feeds every replica the same input, delivers only outputs it has checked, retires replicas that fail, keeps going on the ones left, and resumes after a reset or a power cut. Part of [Mru](https://mru.space).

**Status (October 21, 2026): version 0.1.0, built and tested on the bench.** The supervisor implements the [specification](docs/spec.md). It runs on general-purpose processors (Linux x86_64 and ARM64, macOS, and static ARM builds under emulation in CI). It has not run on a board in a system yet, and it has no qualification evidence. The decision rule is the `quorum` crate from [mruspace/flight](https://github.com/mruspace/flight), unchanged, the same code that ran on ESA's OPS-SAT PRETTY satellite on October 18, 22 and 28, 2026. The first deployment, an EMSO ERIC ocean test, is proposed for 2027.

## What it does that a service manager does not

systemd restarts a process that dies. The supervisor catches a process that is alive and wrong, alive and hung, or restarted from zero after a reset: it runs the task three times on the same input, compares the outputs on every cycle, and steps down from vote to compare to self-check as replicas fail. It runs under systemd; it does not replace it.

## The one design decision

Inputs are read once. The supervisor acquires one input record per cycle from a source the operator configures (a serial port, a file, a socket, a command) and hands the same bytes to every replica with a shared state record. The operator's task is a function from an input record and a state record to an output record and the next state record, over its standard input and output, in any language ([the interface](docs/spec.md#3-the-task-interface)). A task that reads a sensor or a clock itself cannot be voted on; `field check-task` finds that out before the task is ever supervised.

## Run it

Needs Rust stable. The build fetches the `quorum` crate from mruspace/flight by tag.

```sh
cargo build --release
target/release/field check-task tasks/demo/field.conf   # the demo task is deterministic
target/release/field run tasks/demo/field.conf          # one cycle per second, until SIGTERM
```

The demo task is the flown demo's payload (a 4 KiB block hashed with FNV-1a) under the task interface. A short run on the bench, with a fault schedule and the oracle that scores every delivered output (tests only):

```sh
cat docs/results/2026-10-21-darwin-arm64/faults.txt
# corrupt:2@30    one bit of replica 2's output flipped once
# hang:2@60       replica 2 stops answering
# stuck:1@120     replica 1 returns one fixed wrong value from here on
target/release/field run docs/results/2026-10-21-darwin-arm64/field.conf
```

```
field=0.1.0 quorum=0.4.0 policy=shrink task=target/release/field-demo-task adapter=none input=tick output=none
cycles=400 delivered=399 detected=1 wrong=0 alive=1 mode=selfcheck last_delivered=400 kat_tests=13
stopped=completed at_cycle=400 halted_at=never started=2026-10-21T14:36:56Z written=2026-10-21T14:36:57Z
checkpoint: every=100 resumes=0 last=400 halted=no
footprint: supervisor_max_rss=2096KiB replica_max_rss=1792KiB cpu=0.028s wall=0.4s
limits: memory=128MB/process cpu_share=none max_run=none reply_timeout=200ms period=0ms
faults: injected=3 caught=3 latency_mean=0.0 latency_max=0 (cycles)
fault: corrupt:2@30 caught=30 latency=0 how=outvoted
fault: hang:2@60 caught=60 latency=0 how=lost
fault: stuck:1@120 caught=120 latency=0 how=diagnose
```

```
cycle,event,detail
30,injected,corrupt:2
30,outvoted,2
60,injected,hang:2
60,lost,2
120,injected,stuck:1
120,retired,1 diagnose
120,detected,compare
400,stopped,completed
```

That run is in [docs/results/2026-10-21-darwin-arm64](docs/results/2026-10-21-darwin-arm64) (Apple M4, 400 cycles as fast as the task answers, reply timeout 200 ms). Replica 2's flipped bit is outvoted on cycle 30 and the replica stays. Replica 2 stops answering on cycle 60 and is lost after the timeout. Replica 1 goes wrong on cycle 120 with two alive: the two disagree, nothing is delivered that cycle, the known-answer test names replica 1, and replica 0 self-checks alone from there. 399 of 400 outputs delivered, none wrong.

Every line in `events.csv` names the cycle, what happened and to which replica. A run of identical events collapses to one line plus its count, so a long run stays small.

## What comes out

In the configured directory: `events.csv`, `summary.txt` (every N cycles and at a stop; every number is a counter from the run) and `checkpoint`. Delivered outputs go to the configured sink with their cycle number. An optional heartbeat writes one line per period, `MRU,<cycle>,<mode>,<alive>,<delivered>,<detected>,<resumes>`, to a file, a serial port or a TCP stream.

## Checkpoint and resume

A checkpoint is written every N cycles and on every cycle in which a replica was lost, retired, cleared or restarted, atomically. After a reset or a power cut the supervisor resumes from it: the event log and a file sink are cut back to the checkpoint's lengths, the counts and the cycle number continue, and a retired replica stays retired. The cycles after the last checkpoint are lost; their inputs were acquired once and cannot be replayed. A checkpoint from another configuration is refused, not overwritten. `scripts/power-cut.sh` kills the supervisor ten times at random cycles and checks all of this.

```sh
./scripts/power-cut.sh
```

```
cut 1 at checkpoint cycle 40
cut 2 at checkpoint cycle 140
cut 3 at checkpoint cycle 240
cut 4 at checkpoint cycle 320
cut 5 at checkpoint cycle 400
cut 6 at checkpoint cycle 460
cut 7 at checkpoint cycle 520
cut 8 at checkpoint cycle 580
cut 9 at checkpoint cycle 620
cut 10 at checkpoint cycle 660
ok    power cuts: 10 resumes, counts equal an uninterrupted run, retired slots kept, no duplicate events
```

That run is in the same results directory. The checkpoint was every 20 cycles and the run 3,000 cycles; the summary after the tenth resume equals the summary of an uninterrupted run, except for the known-answer tests each resume adds at start-up.

## Limits

| Limit | Default | Scope |
|---|---|---|
| Memory per process | 128 MB address space, set in each task process before it starts | Linux |
| CPU share | `cpu_percent`, by sleeping after each cycle so the CPU time of the supervisor and all its task processes stays under the share, start-up included | Linux; elsewhere the cycle's wall time stands in for CPU time |
| Reply timeout | 2,000 ms for the request and the reply; a task that stopped reading cannot block the supervisor on a full pipe | all |
| Clean stop | SIGTERM: finish the cycle, checkpoint, summary, exit 0, within 200 ms of any wait | all |
| Replicas, strikes, window | 3, 3, 200: constants of quorum 0.4.0; the configuration refuses other values | version 0.1.0 |

Known limits, stated next to the claims: three replicas on one processor catch faults in the replicas, not a failure of the processor (a second processor over a link is planned, [section 10](docs/spec.md#10-the-second-processor-later)). On the last replica, a fault that hits both computations alike gets through; only the periodic known-answer test catches a stuck output, after up to `kat_every` cycles. In compare mode, two replicas that disagree and both pass the test lose that cycle, and two that both fail halt the run. Nothing here is qualification evidence.

## Layout

| Path | What |
|---|---|
| [src/supervisor.rs](src/supervisor.rs) | The cycle: acquire, ask, decide, compare bytes, deliver, health, checkpoint |
| [src/replica.rs](src/replica.rs) | A task process: limits before exec, non-blocking requests with a deadline, the known-answer test |
| [src/protocol.rs](src/protocol.rs) | The task interface: framing, reads and writes that give up at a deadline |
| [src/source.rs](src/source.rs), [src/sink.rs](src/sink.rs) | Acquisition and delivery: `tick`, `file:`, `serial:`, `tcp:`, `cmd:`; the heartbeat |
| [src/checkpoint.rs](src/checkpoint.rs), [src/log.rs](src/log.rs), [src/summary.rs](src/summary.rs) | What comes out |
| [src/config.rs](src/config.rs), [src/faults.rs](src/faults.rs), [src/check.rs](src/check.rs) | The configuration, the fault schedule, `check-task` |
| [tasks/demo](tasks/demo) | The demo task, its known-answer records and a bench configuration |
| [tasks/pilot/faults.txt](tasks/pilot/faults.txt) | The pilot's fault schedule, with its expected course |
| [tests](tests), [scripts](scripts) | Integration tests; the scenario checks, the power-cut check, the long bench run |
| [docs/spec.md](docs/spec.md) | The specification, version 0.2, with what version 0.1.0 does |
| [docs/results](docs/results) | The runs the numbers in this file come from |

## Tests

```sh
cargo test --release          # 32 unit tests, 18 integration tests
./scripts/scenarios.sh        # 23 checks, ported from mruspace/flight where they apply
./scripts/power-cut.sh        # ten kills at random cycles, one resume each
```

The scenario checks run in CI on Linux x86_64, Linux ARM64 and macOS, and on static musl builds for aarch64, armv7 and x86_64 under emulation. The flight repository's checks on the radiation sensor and on stuck bits inside the task do not apply here: the task is a black box to the supervisor, and the flight bench measures those.

## Acceptance

Of the five bench steps in [the specification](docs/spec.md#12-acceptance), four are done in this repository: the scenario checks, the determinism test, the fault schedule, and ten power cuts (with SIGKILL as the cut). The seven-day run on a bench twin is not done; `scripts/soak.sh` starts it.

## Related

- [mruspace/flight](https://github.com/mruspace/flight): Mru Flight, the same core for spacecraft, with the demo and its results in orbit
- [docs.mru.space](https://docs.mru.space): the shrinking quorum, what is proved and what is tested
- [mruspace/dusk](https://github.com/mruspace/dusk): the reliability simulator

Changes are in [CHANGELOG.md](CHANGELOG.md). To cite this work, see [CITATION.cff](CITATION.cff).

## Questions or contributions

Pull requests are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md). Email [contact@mru.space](mailto:contact@mru.space).

## License

Code under [Apache License 2.0](./LICENSE). The **Mru** name and mark are
trademarks; see [TRADEMARK.md](./TRADEMARK.md).
