# Mru Field supervisor: specification

Status: **built, version 0.1.0.** Version 0.2 of this document, 21 October 2026. The supervisor in this repository implements it. It has run on the bench on general-purpose processors (Linux x86_64 and ARM64, macOS, and static ARM builds under emulation in CI). It has not run on a board in a system yet. The prototype it grew from is the `quorum` demo in [mruspace/flight](https://github.com/mruspace/flight) (`demo/src/main.rs`), which ran on ESA's OPS-SAT PRETTY satellite on 18 and 22 October 2026 with a fixed task. This specification turned that prototype into a product an operator can run with their own task. Where version 0.1.0 differs from version 0.1 of this document, the text says so.

Words: a **processor** is one computing unit that runs replicas. A **system** is the deployed equipment (a buoy, a station, a turbine controller). **Hard-to-reach** means a repair visit is slow, costly or impossible; the system may be visited once a year or never. **Hardware** failing means any part of the system failing.

## 1. What it is

A supervisor for one task on a hard-to-reach system: one that runs for months between visits, where a visit is slow, costly or impossible. The operator writes the task as a function from an input record to an output record. The supervisor runs that task as three replicas, feeds every replica the same input, compares their outputs on every cycle, and delivers only outputs it has checked. When a replica is wrong, hung or dead, the supervisor retires it and goes on with the ones left: vote on three, compare on two, self-check on one. It survives a reset or a power cut by resuming from a checkpoint. It writes a log that says which replica failed, on which cycle, how it was caught, and what the supervisor did.

It runs under the system's service manager (systemd or similar) as one process on one Linux board. A later version runs replicas on a second processor over a link (section 10).

### What it is not

- **Not a service manager.** systemd restarts a process that dies. The supervisor catches a process that is alive and wrong, alive and hung, or restarted from zero after a reset. The supervisor itself is started by systemd.
- **Not a scheduler.** cron starts things on time. The supervisor runs one task in cycles and does not decide when other things run.
- **Not a watchdog.** A hardware watchdog reboots a stuck board. The supervisor runs above the watchdog and resumes after it fires.
- **Not hardware redundancy, in version 1.** Three replicas on one processor catch software faults and memory faults in the replicas. They do not survive the processor itself failing. That needs a second processor (section 10). Every document says so.

## 2. The one design decision: inputs are read once

Voting needs replicas that give the same output for the same input. A task that reads a sensor itself is not the same task three times: three reads give three values. So the supervisor separates **acquisition** from **processing**:

1. The supervisor acquires one input record per cycle, from a source the operator configures (a serial port, a file, a socket, a command), **once**.
2. It hands the same bytes to every replica.
3. Each replica computes an output record from the input and nothing else: no clock, no random numbers, no reads of its own, no state the supervisor did not give it.
4. The supervisor compares the outputs and delivers one.

The operator's task therefore becomes: **given this input record (and, optionally, this state record), produce this output record (and the next state record).** State lives in the supervisor, not in the replica, so a resumed or replaced replica starts from the same state as the others.

A task that cannot be written this way cannot be supervised this way. The specification says so, and the pilot is chosen so that it can.

## 3. The task interface

A replica is a process the operator writes in any language. The supervisor starts it, talks to it over its standard input and standard output, and stops it by closing its input. The demo used this shape with 16-byte fixed records; the product uses length-prefixed records so the operator's data can be any size.

**Request** (supervisor to replica), one per cycle:

| Field | Size | Meaning |
|---|---|---|
| cycle | 8 bytes | The cycle number, starting at 1 after a fresh start |
| flags | 1 byte | bit 0: compute twice and return both results (self-check); bit 1: this is a known-answer test |
| input length | 4 bytes | |
| input | n bytes | The acquired record, identical for every replica |
| state length | 4 bytes | |
| state | m bytes | The state record from the previous delivered cycle, identical for every replica; empty on the first cycle of a fresh run |

**Response** (replica to supervisor):

| Field | Size | Meaning |
|---|---|---|
| output length | 4 bytes | |
| output | n bytes | The output record |
| state length | 4 bytes | |
| state | m bytes | The next state record |
| second output | same | Present only when the self-check bit was set: the output of the second computation |

All integers are little-endian. The supervisor refuses a length above `record_max` or `state_max` (section 9) and treats the replica as lost.

Rules for the replica:

- It must answer within the reply timeout (section 6) or it is treated as lost.
- It must produce byte-identical outputs for byte-identical requests. `field check-task` checks this before a task is ever supervised: two instances get the same requests, every request goes to the first instance twice, and all three answers must be byte-identical (acceptance 2). The supervisor's known-answer test checks it again at start-up by sending the same request twice.
- It must not read anything but its standard input, must not write anything but its standard output, and must not keep state between requests other than what it is handed. The supervisor enforces what it can (resource limits, a fresh process after retirement) and the operator's acceptance test checks the rest.
- It exits when its standard input closes.

**Known-answer test.** The operator supplies one input record and its correct output record in the configuration. The supervisor sends it to a replica whenever it needs to know whether that replica is healthy: twice at start-up, on three outvotes in a window, on a disagreement between two, and every N cycles on the last replica. The request carries the known-answer flag, the input record from `kat_input`, an empty state record and the current cycle number; the task's answer must not depend on the state or the cycle. A replica that returns the wrong answer, or no answer in time, is retired. The first answer after a process starts gets two seconds more than the reply timeout, once, since it includes the process start.

**Adapters.** For an operator who has a command-line program rather than a server, `adapter = "cli"` runs the program once per request with the input on its standard input and reads the output from its standard output to end of file; the state record is handed back unchanged. Slower, and the program cannot keep state, but no code change. A fault schedule (section 8) needs a long-lived process and is refused with this adapter.

## 4. Modes and what is delivered

The decision rule is the `quorum` crate from the flight repository, unchanged: proved with Kani for every combination of replies, and the same code that ran in orbit. The crate votes on 64-bit values. The supervisor reduces each reply to one value, an FNV-1a hash of the output and the next state behind their lengths, decides on the hashes, and then compares the agreeing replies byte for byte before it delivers. Two replies with the same hash and different bytes are a disagreement (`detected`, with `bytes` in the log). In self-check the two outputs must match byte for byte too.

| Replicas alive | Mode | Delivered when | Detected (nothing delivered) when |
|---|---|---|---|
| 3 | Vote | at least two outputs agree | no two agree |
| 2 | Compare | both agree | they disagree; the known-answer test then names the bad one and it is retired |
| 1 | Self-check | the replica's two computations agree | they disagree |
| 0 | Halted | never | the run has halted and the log says so |

On the cycle a replica is lost, the decision is made with the replies that came back, one step down from the cycle's mode: two replies are compared, and a lone single reply cannot be self-checked, so that cycle is detected. From the next cycle the request carries the self-check flag.

A delivered output goes to the **output sink** the operator configures (a file, a serial port, a socket, a command). The supervisor never delivers an output it has not checked. In self-check mode a fault that hits both computations alike gets through. The flight repository's known-answer coverage campaign measured this with the common-mode share as an input to the injection: at a share of 0.05, 23 of 400 flips on the last replica got through, each as one wrong result that nothing flags (`bench/results/2026-10-11-darwin-arm64-kat.json`, the demo task, one Apple M4). The share is an input, not a measured physical rate.

In compare mode two gaps remain, documented in the flight repository: when both replicas pass the known-answer test nothing is retired and the cycle is lost, and when both fail, both are retired and the run halts.

## 5. Health, retirement and recovery

- **Strikes.** A replica that loses a vote gets a strike. Three strikes in a window of 200 cycles trigger the known-answer test. A pass clears the strikes (the faults were transient). A fail retires the replica. In version 0.1.0 the three strikes and the window of 200 are constants of the `quorum` crate (`STRIKES`, `STRIKE_WINDOW` in quorum 0.4.0); the configuration accepts `strikes` and `window` and refuses any other value. The same holds for `replicas = 3`.
- **Lost.** A replica that does not answer within the reply timeout, exits, or answers with a length over the limit is retired at once and logged as lost.
- **Retired replicas are replaced, optionally.** `restart_retired = true` starts a fresh process in a retired slot after `cooldown_s` seconds, since on one processor a software fault is often transient. The cool-down is kept as Unix seconds in the checkpoint, so it survives a reset; if the clock went backwards after a reset the cool-down starts again. The fresh process passes the known-answer test twice before it is marked alive, joins at the next cycle with the shared state, and its strikes are cleared. The log records `restarted`. The default is off, so the behavior matches the flown demo until the pilot shows restarts are safe.
- **Nothing is ever delivered on the strength of a replica under test.** A disagreement between two is detected before the test runs; a failed periodic test on the last replica turns that cycle's result into a detection.

The periodic test every 64 cycles is configuration (`kat_every`).

## 6. Limits, enforced by the supervisor on itself

| Limit | Default | How |
|---|---|---|
| Memory per process | 128 MB | address-space limit; the supervisor sets it on itself and in each task process before the task starts (Linux) |
| CPU share of the supervisor | 0, uncapped (5% in the flown demo) | sleep after each cycle so that work takes at most the share, start-up included. Work is the CPU time of the supervisor and all its task processes on Linux (the replicas compute in parallel); elsewhere the wall time of the cycle, less the wait for a lost replica |
| Reply timeout | 2,000 ms | a replica that is late is lost. The timeout covers the request too: a replica that stopped reading leaves its pipe full, and the supervisor's write gives up at the deadline instead of blocking. The reply deadline for every replica starts after the last request of the cycle was sent, so one blocked send costs the cycle one timeout and costs the healthy replicas nothing |
| Cycle period | 1,000 ms | the minimum time between cycles; the supervisor never runs faster than the source produces, and `0` is as fast as the source |
| Run time | unlimited | `max_seconds` and `max_cycles` cap a run for tests and the bench |
| Clean stop | SIGTERM or SIGINT | finish the cycle, checkpoint, write the summary, exit 0; every wait looks at the flag at least every 200 ms |

The supervisor is user space only. No drivers, no device files beyond those the operator configures for acquisition and output, no privileged calls, no network unless configured. It cannot reboot the board. A task process gets `SIGPIPE` back at its default and, on Linux, dies with the supervisor (`PR_SET_PDEATHSIG`), so a task that ignores end of input cannot outlive a supervisor that was killed.

## 7. Checkpoint and resume

Every N cycles (default 100), and on every cycle in which a replica was lost, retired, cleared or restarted, the supervisor writes a checkpoint: the cycle number, the counts, which replica slots are alive or retired and when they were retired, the strike record, the current state record, the length of the event log and of a file output sink at that point, the open run of identical events, and whether the run has halted. The write is atomic (write, sync, rename, sync the directory), so a power cut leaves the old checkpoint or the new one.

On start, if a checkpoint from the same configuration exists, the supervisor resumes: it cuts the event log and a file output sink back to the checkpoint's lengths, writes one `resumed` line, starts replicas for the slots that were alive, writes a checkpoint at once (so a second cut before the next one cannot erase the resume), and continues the counts and the cycle number from the checkpoint. The cycles between the last checkpoint and the cut are lost: their inputs were acquired once and cannot be replayed. A serial or TCP sink has already received those cycles' outputs, so cycle numbers in that window can repeat there; a file sink is cut back and never repeats. The log records how many times the run resumed, and the pilot configuration sets `checkpoint_every` low enough for the lost window to be acceptable (each checkpoint is two syncs).

A checkpoint from a different configuration is refused with exit 2, not overwritten. The configuration's identity is the keys that decide results: `task`, `adapter`, `input`, `output`, `policy`, `replicas`, `strikes`, `window`, `kat_every`, `record_max`, `state_max`, the known-answer records and the fault schedule by content, and `oracle`. The keys that only tune a run (period, directory, checkpoint and summary intervals, restarts, limits, heartbeat, caps) are not in it, so an operator can retune without losing the run.

A run that halts keeps its checkpoint with the halt in it. By default the supervisor then idles: no cycles, the heartbeat says `halted`, and a configured restart of a retired replica is the way out. A restart of the process logs `resumed` as halted and idles again. `on_halt = "exit"` ends the process with the summary instead, for a service manager that should see the halt.

The demo's checkpoint was tested with a kill in CI and in orbit, in OPS-SAT run 2 on 22 October 2026 (killed at tick 900, resumed once with the retired replica still retired, 0 wrong); the board bench with resets and power cuts has not been run yet. The product's checkpoint is tested with ten kills at random cycles in `scripts/power-cut.sh` (acceptance 4).

## 8. What comes out

All in one output directory the operator configures (`dir`), except the delivered outputs, which go to the output sink:

- **Delivered outputs** to the output sink, in order, with the cycle number. A file or TCP sink gets `cycle` (8 bytes), `length` (4 bytes) and the bytes; a serial sink gets a text line `<cycle>,<bytes>`, so the task keeps its output ASCII there; a command sink runs the program once per output with the bytes on its standard input and `FIELD_CYCLE` in its environment.
- **`events.csv`**, the event log: `cycle,event,detail`. Events: `outvoted`, `retired` (with why: `strikes`, `diagnose`, `periodic`, `startup`, `restart`), `cleared`, `lost`, `restarted`, `detected` (with the mode, and `bytes` when the hashes agreed and the bytes did not), `wrong` (when a scoring oracle exists, tests only), `halted`, `stopped` (with why), `resumed`, `progress`, `injected` (a scheduled fault, tests only) and `source` (a command source that failed, or a serial line cut at `record_max`). Runs of identical events on consecutive cycles collapse to the first line plus a closing line with the count, so a long run stays small. The file is handed to the kernel after every cycle and synced at every checkpoint.
- **`summary.txt`**, written every N cycles (`summary_every`), at a halt and at a stop: cycles run, outputs delivered, disagreements detected, replicas alive and the mode, when and why the run stopped, the checkpoint and resume counts, footprint, limits, the heartbeat counts, and for a fault schedule how fast the supervisor reacted to each fault it caught.
- **A heartbeat**, optional, one line per period (`heartbeat_every_s`, default one hour) to a file, a serial port or a TCP stream: `MRU,<cycle>,<mode>,<alive>,<delivered>,<detected>,<resumes>`. A file holds the latest line only.
- **The checkpoint**, `checkpoint`, one `key=value` per line.

Every number in a summary comes from the run. Nothing is estimated.

**Sources.** `tick` gives the cycle number as an 8-byte record, for a self-contained task. `file:PATH` reads length-prefixed records and ends the run cleanly at end of file. `tcp:HOST:PORT` connects once and reads the same records. `serial:DEVICE:BAUD` reads one line per record (raw mode, 8N1, no flow control). `cmd:PROGRAM ARGS` runs the program once per cycle and takes its standard output as the record; a failure is logged and the cycle is not consumed.

**Fault schedule**, for tests and a self-contained pilot (`faults = PATH`): one fault per line, `kind:replica@cycle`, `#` comments. `kill` sends the process SIGKILL; `hang` sends SIGSTOP, so it never answers; `stuck` replaces every output of the replica from that cycle on with one fixed wrong value, and stays with the slot across resumes and restarts; `corrupt` flips one bit of the output once, in the first computation only; `corrupt2` flips both computations alike. The faults act on the process or the reply bytes, never inside the task, so any task can be put under them. A test-only `oracle` runs a fourth, unfaulted instance of the task and scores every delivered output.

## 9. Configuration

One file. Fields, with defaults:

```
task        = "/opt/field/task"        # the replica executable
adapter     = "none"                   # or "cli" for a run-once program
input       = "serial:/dev/ttyS1:9600" # acquisition source; or tick, file:, tcp:, cmd:
period_ms   = 1000                     # minimum cycle period; 0 is as fast as the source
output      = "file:/var/field/out"    # delivered outputs; or serial:, tcp:, cmd:, none
dir         = "/var/field"             # events, summary, checkpoint
kat_input   = "/opt/field/kat.in"      # known-answer test input record
kat_output  = "/opt/field/kat.out"     # and its correct output
replicas    = 3                        # fixed in 0.1.0
strikes     = 3                        # fixed in 0.1.0
window      = 200                      # fixed in 0.1.0
kat_every   = 64
checkpoint_every = 100
summary_every    = 1000
restart_retired  = false
cooldown_s  = 600
memory_mb   = 128
cpu_percent = 0                        # 0 is uncapped
reply_timeout_ms = 2000
heartbeat   = "none"                   # or serial:/tcp:/file:
heartbeat_every_s = 3600
policy      = "shrink"                 # or "tmr": vote, compare, halt on one
on_halt     = "idle"                   # or "exit"
record_max  = 65536                    # largest input or output record, bytes
state_max   = 65536                    # largest state record, bytes
max_cycles  = 0                        # 0 is unlimited; tests and the bench
max_seconds = 0
faults      = "none"                   # a fault schedule; tests and pilots
oracle      = "none"                   # a scoring instance of the task; tests
```

The keys that decide results are part of the checkpoint identity (section 7): change one and the run starts fresh after the old checkpoint is moved away.

## 10. The second processor (later)

Version 1 runs on one board. The design keeps the path open: a replica slot may point at a remote replica host over a serial or Ethernet link, the request and response records are the same bytes, and the reply timeout covers the link. In the code every slot talks through one interface with one implementation today, a local process over its standard input and output. With one replica on a second board, a failure of the first processor leaves a replica that can carry the task until the next visit. The supervisor itself is then the single point of failure, and the step after that is two supervisors with a lease. None of this is built; the record format and the configuration are designed so that it can be.

## 11. The first deployment: EMSO W1M3A

The pilot in the EMSO ERIC physical access programme (proposal due 31 October 2026; kick-off March 2027; twelve weeks in the water May to August 2027) deploys one Mru Field node on the W1M3A spar buoy off Genoa, operated by CNR. Node: one industrial Linux board in a DIN-rail case, under 0.25 A at 24 V, on one relay-switched channel that CNR can cut at any time, with an optional one-line heartbeat into the buoy's control system.

What the pilot runs: the supervisor with the self-contained demo task (`field-demo-task`, the flown demo's payload) and the stored fault schedule in `tasks/pilot/faults.txt`, in matched periods under `policy = "shrink"` and `policy = "tmr"`, plus the real power cuts the buoy applies on its own schedule and when the battery runs low. That proves the supervisor survives the environment and the resets, and gives real failure data from a real system. It does not yet run CNR's own acquisition under the supervisor; that is the step after the pilot, and the task interface above is designed so that it can be done without changing the supervisor.

## 12. Acceptance

Before any deployment, on the bench:

1. Every scenario check from the flight repository that applies to a supervisor with a black-box task, run against the supervisor with the demo task. **Done:** `scripts/scenarios.sh`, 23 checks, in CI on Linux x86_64, Linux ARM64, macOS and static ARM builds under emulation. The flight checks on the radiation sensor and on stuck bits inside the task do not apply; the flight bench measures those.
2. The operator's task passes the determinism test: the same request twice gives the same response, across 10,000 requests. **Done:** `field check-task`, run on the demo task in the checks.
3. A fault schedule with a hung replica, a stuck replica and a corrupted output, every event on its cycle, zero wrong outputs delivered where the rule says none. **Done:** `tests/faults.rs` and the scenario checks.
4. Ten power cuts at random cycles; every resume continues the counts and keeps every retired slot retired; no event in the log twice. **Done:** `scripts/power-cut.sh`, with SIGKILL as the cut; a real power cut on a board is not done.
5. Seven days unattended on the bench twin with the real cycle period and the real sources, footprint within limits for the whole run. **Not done:** `scripts/soak.sh` starts the run; the bench twin does not exist yet.

## 13. Open questions

- The state record's maximum size: `state_max`, 64 KiB by default, and the state is in the checkpoint. Larger state, or state in a separate file, is open.
- Whether restarting retired replicas should be on by default after the pilot. Still off; the pilot configuration turns it on, since its schedule retires all three replicas.
- The heartbeat format CNR prefers. The line above is the proposal; the field order is stable.
- How a task that needs the time of day gets it: as part of the input record, acquired once, is the proposed answer. A `cmd:` source can prepend it.

## Sources

- The prototype: [mruspace/flight, `demo/src/main.rs`](https://github.com/mruspace/flight/blob/main/demo/src/main.rs), and its [operations note](https://github.com/mruspace/flight/blob/main/docs/opssat.md) with the measured runs in orbit.
- The decision rule and its proofs: [the `quorum` crate](https://github.com/mruspace/flight/tree/main/quorum), tag v0.4.0.
- Known-answer coverage: the flight repository README, "The honest cost", and `bench/results/2026-10-11-darwin-arm64-kat.json`.
- The code: `src/` in this repository; the cycle is `src/supervisor.rs`.
- The EMSO proposal and the W1M3A node sheet (private, finance repository).
