# Changelog

All notable changes to Mru Field. Versions follow [Semantic Versioning](https://semver.org).

## 0.2.0 (2026-11-14)

### Added

- Staged updates (docs/spec.md, section 10). `candidate` names a new build of the task and `candidate_cycles` (default 1000) the clean cycles per trial. The new build comes in one slot at a time: slot 2 against two slots on the old build, then slot 1, where an output is delivered only if the slot left on the old build gave the same bytes, then slot 0. Until the last switch, one fault of a slot on the new build (an outvote, a lost reply, a failed known-answer test, a cycle with no agreement, a disagreement with the old build) rejects it and starts the old build again in every slot that had it. A lost old replica pauses the update until three replicas are alive. The checkpoint keeps the build of each slot, the trial and the candidate's hash, so a resume continues the trial; a changed or removed candidate puts every slot back on the old build. `candidate` events in `events.csv` and an `update:` line in `summary.txt`.
- `field-demo-task --bug KIND`: bad builds for the tests (`crash@N`, `hang@N`, `wrong@N`, `every@M`, `kat`).
- `tests/staged.rs` (6 tests), scenario checks 24 to 26, and `scripts/staged.sh`, which brings each bad build in all at once and by staged update. Results in docs/results/2026-11-14-darwin-arm64.

### Unchanged

- The decision rule: the `quorum` crate from mruspace/flight v0.4.0, unchanged. Staged updates live in the supervisor.
- A 0.1.0 checkpoint resumes under 0.2.0 with no update under way.

## 0.1.0 (2026-10-21)

### Added

- The supervisor, `field`: one process on one Linux board that runs an operator's task as three replicas, acquires one input record per cycle from a configured source (`tick`, `file:`, `serial:`, `tcp:`, `cmd:`) and hands the same bytes to every replica with a shared state record, decides with the `quorum` crate from mruspace/flight v0.4.0 (unchanged, as a pinned git dependency) on a hash of each reply and then compares the agreeing replies byte for byte, delivers one output per cycle to a configured sink, and retires a replica that is outvoted three times in 200 cycles and fails its known-answer test, that disagrees in compare mode and fails the test, that fails the periodic test alone, or that does not answer in time. The task interface of docs/spec.md section 3: length-prefixed records over the task's standard input and output, a self-check flag, a known-answer flag, and a `cli` adapter that runs a program once per request.
- Checkpoint and resume: a checkpoint every N cycles and on every change of which replicas are alive, written atomically; a resume cuts the event log and a file sink back to the checkpoint's lengths and continues the counts and the cycle number; a checkpoint from another configuration is refused; a halted run keeps its checkpoint and, by default, idles with its heartbeat saying `halted` (`on_halt = exit` ends the process instead). Retired replicas can be restarted after a cool-down (`restart_retired`, off by default).
- `events.csv`, `summary.txt` every N cycles and at a stop, and an optional one-line heartbeat to a file, a serial port or a TCP stream.
- Limits the supervisor enforces: an address-space limit set in each task process before it starts (Linux), a CPU share by sleeping after each cycle, a reply deadline per cycle that covers the request too (a task that stopped reading cannot hang the supervisor on a full pipe), and a clean stop on SIGTERM within a fraction of a second from any wait.
- For tests and a self-contained pilot: a stored fault schedule (`kill`, `hang`, `stuck`, `corrupt`, `corrupt2`) that acts on the task's process or on its reply bytes, never inside the task; a test-only oracle; `policy = tmr` for matched fixed-TMR periods.
- `field check-task`: the determinism test of acceptance 2 (two instances, every request twice, byte-identical answers).
- `field-demo-task`: the flight demo's payload as a task under the interface, with its known-answer files; `tasks/pilot/faults.txt`, the OPS-SAT run 2 lifecycle schedule without the faults inside the task.
- Tests: 32 unit tests, 18 integration tests, 23 scenario checks ported from mruspace/flight where they apply to a supervisor with a black-box task, and the ten-power-cut check of acceptance 4. CI on Linux x86_64, Linux ARM64 and macOS, static musl builds for aarch64, armv7 and x86_64 with the checks run under emulation.
