# Mru Field supervisor: specification

Status: **planned, not built.** Version 0.1 of this document, 21 October 2026. Nothing in this repository runs yet. The prototype it grows from is the `quorum` demo in [mruspace/flight](https://github.com/mruspace/flight) (`demo/src/main.rs`), which ran on ESA's OPS-SAT PRETTY satellite on 18 October 2026 with a fixed task. This specification turns that prototype into a product an operator can run with their own task.

Words: a **processor** is one computing unit that runs replicas. A **system** is the deployed equipment (a buoy, a station, a turbine controller). **Hardware** failing means any part of the system failing.

## 1. What it is

A supervisor for one task on an unattended system. The operator writes the task as a function from an input record to an output record. The supervisor runs that task as three replicas, feeds every replica the same input, compares their outputs on every cycle, and delivers only outputs it has checked. When a replica is wrong, hung or dead, the supervisor retires it and goes on with the ones left: vote on three, compare on two, self-check on one. It survives a reset or a power cut by resuming from a checkpoint. It writes a log that says which replica failed, on which cycle, how it was caught, and what the supervisor did.

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

A replica is a process the operator writes in any language. The supervisor starts it, talks to it over its standard input and standard output, and stops it by closing its input. The demo already uses this shape with 16-byte fixed records; the product uses length-prefixed records so the operator's data can be any size.

**Request** (supervisor to replica), one per cycle:

| Field | Size | Meaning |
|---|---|---|
| cycle | 8 bytes | The cycle number, starting at 1 after a fresh start |
| flags | 1 byte | bit 0: compute twice and return both results (self-check); bit 1: this is a known-answer test |
| input length | 4 bytes | |
| input | n bytes | The acquired record, identical for every replica |
| state length | 4 bytes | |
| state | m bytes | The state record from the previous delivered cycle, identical for every replica |

**Response** (replica to supervisor):

| Field | Size | Meaning |
|---|---|---|
| output length | 4 bytes | |
| output | n bytes | The output record |
| state length | 4 bytes | |
| state | m bytes | The next state record |
| second output | same | Present only when the self-check bit was set: the output of the second computation |

Rules for the replica:

- It must answer within the reply timeout (section 6) or it is treated as lost.
- It must produce byte-identical outputs for byte-identical requests. The supervisor's known-answer test checks this at start-up by sending the same request twice.
- It must not read anything but its standard input, must not write anything but its standard output, and must not keep state between requests other than what it is handed. The supervisor enforces what it can (resource limits, a fresh process after retirement) and the operator's acceptance test checks the rest.
- It exits when its standard input closes.

**Known-answer test.** The operator supplies one input record and its correct output record in the configuration. The supervisor sends it to a replica whenever it needs to know whether that replica is healthy: on three outvotes in a window, on a disagreement between two, and every N cycles on the last replica. A replica that returns the wrong answer is retired.

**Adapters.** For an operator who has a command-line program rather than a server, a shipped adapter runs the program once per request with the input on its standard input and reads the output from its standard output. Slower, but no code change.

## 4. Modes and what is delivered

The decision rule is the `quorum` crate from the flight repository, unchanged: proved with Kani for every combination of replies, and the same code that ran in orbit.

| Replicas alive | Mode | Delivered when | Detected (nothing delivered) when |
|---|---|---|---|
| 3 | Vote | at least two outputs agree | no two agree |
| 2 | Compare | both agree | they disagree; the known-answer test then names the bad one and it is retired |
| 1 | Self-check | the replica's two computations agree | they disagree |
| 0 | Halted | never | the run has halted and the log says so |

A delivered output goes to the **output sink** the operator configures (a file, a serial port, a socket, a command). The supervisor never delivers an output it has not checked. In self-check mode a fault that hits both computations alike gets through; the specification says this, with the measured rate from the flight repository's known-answer coverage campaign, and does not hide it.

## 5. Health, retirement and recovery

- **Strikes.** A replica that loses a vote gets a strike. Three strikes in a window of 200 cycles trigger the known-answer test. A pass clears the strikes (the faults were transient). A fail retires the replica.
- **Lost.** A replica that does not answer within the reply timeout is retired at once and logged as lost.
- **Retired replicas are replaced, optionally.** Version 1 may restart a retired replica as a fresh process after a configurable cool-down, since on one processor a software fault is often transient. The restarted replica joins at the next cycle with the shared state. The log records it. The default is off, so the behaviour matches the flown demo until the pilot shows restarts are safe.
- **Nothing is ever delivered on the strength of a replica under test.**

The numbers (three strikes, window 200, test every 64 cycles) are the demo's defaults and are configuration in the product.

## 6. Limits, enforced by the supervisor on itself

| Limit | Default | How |
|---|---|---|
| Memory per process | 128 MB | address-space limit set by each process on itself |
| CPU share of the supervisor | configurable, 5% in the flown demo | sleep after each cycle so that work takes at most the share; start-up included |
| Reply timeout | 2,000 ms | a replica that is late is lost |
| Cycle period | configurable | the acquisition rate; the supervisor never runs faster than the source produces |
| Run time | unlimited by default | a cap for tests |
| Clean stop | SIGTERM | finish the cycle, write the summary, exit 0 |

The supervisor is user space only. No drivers, no device files beyond those the operator configures for acquisition and output, no privileged calls, no network unless configured. It cannot reboot the board.

## 7. Checkpoint and resume

Every N cycles (default 100) the supervisor writes a checkpoint: the cycle number, the counts, which replica slots are alive or retired, the strike record, the current state record, and the length of the event log at that point. The write is atomic (write, sync, rename), so a power cut leaves the old checkpoint or the new one. On start, if a checkpoint from the same configuration exists, the supervisor resumes: it cuts the event log back to the checkpoint's length, writes one `resumed` line, starts replicas for the slots that were alive, and continues the counts. A checkpoint from a different configuration is refused, not overwritten.

This is the demo's checkpoint, already tested on the board bench with resets and power cuts and in the OPS-SAT run 2 package with a planned kill. The product adds the state record to it.

## 8. What comes out

All in one output directory the operator configures:

- **Delivered outputs** to the output sink, in order, with the cycle number.
- **`events.csv`**, the event log: `cycle,event,detail`. Events: `outvoted`, `retired`, `cleared`, `lost`, `restarted`, `detected`, `wrong` (when a scoring oracle exists, tests only), `halted`, `stopped`, `resumed`, `progress`. Runs of identical events on consecutive cycles collapse to the first line plus a closing line with the count, so a long run stays small.
- **`summary.txt`**, written at a clean stop and every N cycles: cycles run, outputs delivered, disagreements detected, replicas alive, when and why the run stopped, footprint, limits, and how fast the supervisor reacted to each fault it caught.
- **A heartbeat**, optional, one line per period to a configured sink, for a host control system that wants to know the node is alive (the EMSO node's control system can take one over serial or Ethernet).

Every number in a summary comes from the run. Nothing is estimated.

## 9. Configuration

One file. Fields, with defaults:

```
task        = "/opt/field/task"        # the replica executable
adapter     = "none"                   # or "cli" for a run-once program
input       = "serial:/dev/ttyS1:9600" # acquisition source; or file:, tcp:, cmd:
period_ms   = 1000                     # cycle period
output      = "file:/var/field/out"    # delivered outputs
dir         = "/var/field"             # events, summary, checkpoint
kat_input   = "/opt/field/kat.in"      # known-answer test input record
kat_output  = "/opt/field/kat.out"     # and its correct output
replicas    = 3
strikes     = 3
window      = 200
kat_every   = 64
checkpoint_every = 100
restart_retired  = false
cooldown_s  = 600
memory_mb   = 128
cpu_percent = 0                        # 0 is uncapped
reply_timeout_ms = 2000
heartbeat   = "none"                   # or serial:/tcp:/file:
```

The configuration is part of the checkpoint identity: change it and the run starts fresh.

## 10. The second processor (later)

Version 1 runs on one board. The design keeps the path open: a replica slot may point at a remote replica host over a serial or Ethernet link, the request and response records are the same bytes, and the reply timeout covers the link. With one replica on a second board, a failure of the first processor leaves a replica that can carry the task until the next visit. The supervisor itself is then the single point of failure, and the step after that is two supervisors with a lease. None of this is built; the record format and the configuration are designed so that it can be.

## 11. The first deployment: EMSO W1M3A

The pilot in the EMSO ERIC physical access programme (proposal due 31 October 2026; kick-off March 2027; twelve weeks in the water May to August 2027) deploys one Mru Field node on the W1M3A spar buoy off Genoa, operated by CNR. Node: one industrial Linux board in a DIN-rail case, under 0.25 A at 24 V, on one relay-switched channel that CNR can cut at any time, with an optional one-line heartbeat into the buoy's control system.

What the pilot runs: the supervisor with a self-contained task and a stored fault schedule, plus the real power cuts the buoy applies on its own schedule and when the battery runs low. That proves the supervisor survives the environment and the resets, and gives real failure data from a real system. It does not yet run CNR's own acquisition under the supervisor; that is the step after the pilot, and the task interface above is designed so that it can be done without changing the supervisor.

## 12. Acceptance

Before any deployment, on the bench:

1. Every scenario check from the flight repository, run against the supervisor with the demo task.
2. The operator's task passes the determinism test: the same request twice gives the same response, across 10,000 requests.
3. A fault schedule with a hung replica, a stuck replica and a corrupted output, every event on its cycle, zero wrong outputs delivered where the rule says none.
4. Ten power cuts at random cycles; every resume continues the counts and keeps every retired slot retired; no event in the log twice.
5. Seven days unattended on the bench twin with the real cycle period and the real sources, footprint within limits for the whole run.

## 13. Open questions

- The state record's maximum size and whether large state belongs in the checkpoint or in a separate file.
- Whether restarting retired replicas should be on by default after the pilot.
- The heartbeat format CNR prefers.
- How a task that needs the time of day gets it: as part of the input record, acquired once, is the proposed answer.

## Sources

- The prototype: [mruspace/flight, `demo/src/main.rs`](https://github.com/mruspace/flight/blob/main/demo/src/main.rs), and its [operations note](https://github.com/mruspace/flight/blob/main/docs/opssat.md) with the measured runs in orbit.
- The decision rule and its proofs: [the `quorum` crate](https://github.com/mruspace/flight/tree/main/quorum).
- Known-answer coverage: the flight repository README, "Fault on the last replica".
- The EMSO proposal and the W1M3A node sheet (private, finance repository).
