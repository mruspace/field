#!/bin/sh
# Scenario checks for the Mru Field supervisor. Each check runs the real
# program with the demo task and asserts on what it wrote. Exit status is
# non-zero if any check fails. Ported from mruspace/flight scripts/scenarios.sh
# where a check applies to a supervisor with a black-box task; see README.
set -eu
cd "$(dirname "$0")/.."

# FIELD_BIN and FIELD_TASK test prebuilt binaries (for example static ARM
# builds under emulation); otherwise the local release build is used.
# FIELD_EMULATED=1 skips the check that reads /proc.
if [ -z "${FIELD_BIN:-}" ]; then
    cargo build --release --quiet
fi
F=${FIELD_BIN:-target/release/field}
T=${FIELD_TASK:-target/release/field-demo-task}
case "$F" in /*) ;; *) F="$PWD/$F" ;; esac
case "$T" in /*) ;; *) T="$PWD/$T" ;; esac
KAT_IN="$PWD/tasks/demo/kat.in"
KAT_OUT="$PWD/tasks/demo/kat.out"

fails=0
pass() { printf 'ok    %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

# field <name> <text>: the value of name=value in text (a summary line or an event line)
field() { printf '%s\n' "$2" | tr ' ,' '\n\n' | sed -n "s/^$1=//p" | head -n 1; }
# counts <line>: summary line 2 without kat_tests, which a resume adds its start-up tests to
counts() { printf '%s\n' "$1" | sed 's/ kat_tests=[0-9]*//'; }

# conf DIR [extra lines...]: write DIR/field.conf with the bench defaults
# (tick source, no output, the oracle on) plus the extra lines
conf() {
    d=$1; shift
    {
        printf 'task = "%s"\ninput = "tick"\nperiod_ms = 0\noutput = "none"\ndir = "%s"\n' "$T" "$d"
        printf 'kat_input = "%s"\nkat_output = "%s"\noracle = "%s"\n' "$KAT_IN" "$KAT_OUT" "$T"
        for l in "$@"; do printf '%s\n' "$l"; done
    } >"$d/field.conf"
}

# run DIR: run the configuration in DIR and print summary line 2
run() { "$F" run "$1/field.conf" | sed -n 2p; }

# faults DIR lines...: write DIR/faults.txt
faults() { d=$1; shift; printf '%s\n' "$@" >"$d/faults.txt"; }

d=$(mktemp -d)

# 1. No faults: every cycle is delivered and correct, both policies.
for p in shrink tmr; do
    conf "$d" "policy = \"$p\"" "max_cycles = 20000"
    out=$(run "$d")
    if [ "$(field delivered "$out")" = 20000 ] && [ "$(field wrong "$out")" = 0 ]; then
        pass "$p: no faults, all 20000 outputs delivered and correct"
    else
        fail "$p: no faults ($out)"
    fi
    rm -f "$d/checkpoint"
done

# 2. Two replicas die: fixed TMR halts at the second loss, the shrinking
#    quorum keeps going on self-check and delivers more.
faults "$d" kill:0@5000 kill:1@10000
conf "$d" 'policy = "tmr"' 'on_halt = "exit"' "max_cycles = 20000" "faults = \"$d/faults.txt\""
t=$(run "$d"); t3=$(sed -n 3p "$d/summary.txt"); rm -f "$d/checkpoint"
conf "$d" 'policy = "shrink"' "max_cycles = 20000" "faults = \"$d/faults.txt\""
s=$(run "$d"); s3=$(sed -n 3p "$d/summary.txt"); rm -f "$d/checkpoint"
[ "$(field mode "$t")" = halted ] && [ "$(field halted_at "$t3")" = 10000 ] && pass "tmr: halts at the second loss" || fail "tmr: halt ($t / $t3)"
[ "$(field halted_at "$s3")" = never ] && pass "shrink: keeps running after the second loss" || fail "shrink: halt ($s3)"
[ "$(field delivered "$s")" -gt "$(field delivered "$t")" ] && pass "shrink: more outputs than tmr" || fail "shrink vs tmr ($s / $t)"

# 3. A replica gets stuck while three are alive: three strikes in the
#    window, then the known-answer test, then retirement.
faults "$d" stuck:1@4000
conf "$d" "max_cycles = 6000" "faults = \"$d/faults.txt\""
run "$d" >/dev/null; rm -f "$d/checkpoint"
retired=$(sed -n 's/^\([0-9]*\),retired,1 strikes$/\1/p' "$d/events.csv" | head -n 1)
if [ -n "$retired" ] && [ "$retired" -le 4002 ]; then
    pass "shrink: stuck replica retired at cycle $retired by health scoring"
else
    fail "shrink: stuck replica not retired by health scoring"
fi

# 4. A replica gets stuck with two alive: the known-answer test names it.
faults "$d" kill:0@5000 stuck:1@10000
conf "$d" "max_cycles = 12000" "faults = \"$d/faults.txt\""
run "$d" >/dev/null; rm -f "$d/checkpoint"
grep -q '^10000,retired,1 diagnose$' "$d/events.csv" && pass "shrink: stuck replica found by the known-answer test at cycle 10000" || fail "shrink: known-answer test did not find the stuck replica"

# 5. Ten kill schedules: the shrinking quorum never delivers less than fixed TMR.
ok=1
for k in 1 2 3 4 5 6 7 8 9 10; do
    faults "$d" "kill:$((k % 3))@$((k * 700))" "kill:$(((k + 1) % 3))@$((k * 1300))"
    conf "$d" 'policy = "tmr"' 'on_halt = "exit"' "max_cycles = 15000" "faults = \"$d/faults.txt\""
    t=$(run "$d"); rm -f "$d/checkpoint"
    conf "$d" 'policy = "shrink"' "max_cycles = 15000" "faults = \"$d/faults.txt\""
    s=$(run "$d"); rm -f "$d/checkpoint"
    [ "$(field delivered "$s")" -ge "$(field delivered "$t")" ] || { ok=0; echo "  schedule $k: shrink $(field delivered "$s") < tmr $(field delivered "$t")"; }
done
[ "$ok" = 1 ] && pass "shrink delivers at least as much as tmr on ten kill schedules" || fail "shrink below tmr on a kill schedule"

# 6. A replica hangs: lost after the reply timeout, the run goes on.
faults "$d" hang:1@1000
conf "$d" "max_cycles = 3000" "reply_timeout_ms = 200" "faults = \"$d/faults.txt\""
out=$(run "$d"); rm -f "$d/checkpoint"
if grep -q '^1000,lost,1$' "$d/events.csv" && [ "$(field delivered "$out")" -ge 2999 ]; then
    pass "hung replica lost at cycle 1000, run continues"
else
    fail "hung replica ($out)"
fi

# 7. SIGTERM stops the run cleanly: exit 0, summary written, no task left.
conf "$d" "period_ms = 10"
"$F" run "$d/field.conf" >"$d/stdout" & pid=$!
sleep 1
kill -TERM "$pid"
if wait "$pid" && grep -q 'stopped=signal' "$d/summary.txt" && ! pgrep -f "$T" >/dev/null; then
    pass "SIGTERM: clean stop, exit 0, no task process left"
else
    fail "SIGTERM: $(sed -n 3p "$d/summary.txt")"
fi
rm -f "$d/checkpoint"

# 8. A run-time cap stops the run.
conf "$d" "max_seconds = 1"
out=$(run "$d"); rm -f "$d/checkpoint"
grep -q 'stopped=time-limit' "$d/summary.txt" && pass "max_seconds: stopped at the time limit" || fail "max_seconds ($out)"

# 9. CPU share: at 5% the supervisor and its replicas use under 8% of one core.
#    Eight seconds, so the nap that pays for the start-up (seven process
#    starts and their known-answer tests) fits inside the cap under emulation.
conf "$d" "cpu_percent = 5" "max_seconds = 8"
run "$d" >/dev/null; rm -f "$d/checkpoint"
fp=$(sed -n 5p "$d/summary.txt")
cpu=$(field cpu "$fp" | tr -d s); wall=$(field wall "$fp" | tr -d s)
if awk -v c="$cpu" -v w="$wall" 'BEGIN { exit !(c / w < 0.08) }'; then
    pass "cpu share: cpu=${cpu}s over wall=${wall}s at a 5% cap"
else
    fail "cpu share: cpu=${cpu}s over wall=${wall}s"
fi

# 10. Memory limit: the supervisor and every task process carry the address
#     space limit (Linux only; /proc is not there under emulation).
if [ "$(uname -s)" = Linux ] && [ -z "${FIELD_EMULATED:-}" ]; then
    conf "$d" "period_ms = 100"
    "$F" run "$d/field.conf" >/dev/null & pid=$!
    sleep 1
    ok=1
    for p in "$pid" $(pgrep -P "$pid"); do
        grep -q '^Max address space *134217728 *134217728' "/proc/$p/limits" || ok=0
    done
    [ "$(pgrep -P "$pid" | wc -l)" -ge 3 ] || ok=0
    kill -TERM "$pid"; wait "$pid" || true
    [ "$ok" = 1 ] && pass "memory: 128 MB address-space limit on the supervisor and each replica" || fail "memory limit not found in /proc"
    rm -f "$d/checkpoint"
fi

# 11. A malformed schedule is refused with exit 2.
for bad in 'kill:3@1' 'burn:0@1' 'stuckbit:0@1/out.0.0' 'kill:0'; do
    faults "$d" "$bad"
    conf "$d" "faults = \"$d/faults.txt\""
    if "$F" run "$d/field.conf" >/dev/null 2>&1; then fail "schedule '$bad' accepted"; else [ $? = 2 ] && pass "schedule '$bad' refused with exit 2" || fail "schedule '$bad': wrong exit"; fi
done

# 12. Footprint: every process stays under its 128 MB limit by a wide margin.
conf "$d" "max_cycles = 5000"
run "$d" >/dev/null; rm -f "$d/checkpoint"
fp=$(sed -n 5p "$d/summary.txt")
s=$(field supervisor_max_rss "$fp" | tr -d KiB); r=$(field replica_max_rss "$fp" | tr -d KiB)
[ "$s" -lt 131072 ] && [ "$r" -lt 131072 ] && pass "footprint: supervisor ${s} KiB, replica ${r} KiB" || fail "footprint ($fp)"

# 13. Progress lines every N cycles; the last one matches the summary.
conf "$d" "max_cycles = 20000" "summary_every = 1000"
out=$(run "$d"); rm -f "$d/checkpoint"
n=$(grep -c ',progress,' "$d/events.csv")
last=$(grep ',progress,' "$d/events.csv" | tail -n 1)
if [ "$n" = 20 ] && [ "$(field delivered "$last")" = "$(field delivered "$out")" ]; then
    pass "progress: 20 lines, the last matches the summary"
else
    fail "progress: $n lines ($last / $out)"
fi

# 14. The last replica gets stuck: only the periodic known-answer test can
#     catch it, within one period.
faults "$d" kill:0@5000 kill:1@8000 stuck:2@10000
conf "$d" "max_cycles = 12000" 'on_halt = "exit"' "faults = \"$d/faults.txt\""
run "$d" >/dev/null; rm -f "$d/checkpoint"
det=$(grep '^faults:' "$d/summary.txt")
if [ "$(field caught "$det")" = 1 ] && [ "$(field latency_max "$det")" -le 64 ]; then
    pass "stuck last replica caught by the periodic test within 64 cycles ($det)"
else
    fail "stuck last replica ($det)"
fi

# 15. A reset: kill -9 the supervisor mid-run, plant a stale event line past
#     the checkpoint, start again. The resumed run continues the counts to
#     the same result as an uninterrupted run, the retired slot stays
#     retired, the stale line is gone, and a foreign checkpoint is refused.
faults "$d" stuck:1@100
conf "$d" "max_cycles = 2000" "checkpoint_every = 50" "faults = \"$d/faults.txt\""
want=$(run "$d"); rm -f "$d/checkpoint"
conf "$d" "max_cycles = 2000" "checkpoint_every = 50" "period_ms = 2" "faults = \"$d/faults.txt\""
"$F" run "$d/field.conf" >/dev/null & pid=$!
sleep 1
kill -KILL "$pid"; wait "$pid" 2>/dev/null || true
sleep 0.3
cp_cycle=$(sed -n 's/^cycle=//p' "$d/checkpoint")
printf '%s,lost,0\n' "$((cp_cycle + 10))" >>"$d/events.csv"
conf "$d" "max_cycles = 2000" "checkpoint_every = 50" "faults = \"$d/faults.txt\""
got=$(run "$d")
retired=$(grep -c ',retired,1 strikes$' "$d/events.csv")
stale=$(grep -c "^$((cp_cycle + 10)),lost,0$" "$d/events.csv" || true)
resumed=$(grep -c "^$cp_cycle,resumed,1$" "$d/events.csv")
conf "$d" "max_cycles = 2000" 'policy = "tmr"' "faults = \"$d/faults.txt\""
if "$F" run "$d/field.conf" >/dev/null 2>&1; then foreign=accepted; else foreign=$?; fi
if [ "$(counts "$got")" = "$(counts "$want")" ] && [ "$retired" = 1 ] && [ "$stale" = 0 ] && [ "$resumed" = 1 ] && [ "$foreign" = 2 ] &&
    [ "$(head -n 1 "$d/events.csv")" = cycle,event,detail ]; then
    pass "reset: killed after cycle $cp_cycle, resumed to the same counts, stale line removed, foreign checkpoint refused"
else
    fail "reset: want '$want' got '$got', retired=$retired stale=$stale resumed=$resumed foreign=$foreign"
fi
rm -f "$d/checkpoint"

# 16. The last replica hangs: caught at once, the run halts.
faults "$d" kill:0@10 kill:1@20 hang:2@100
conf "$d" "max_cycles = 300" "reply_timeout_ms = 200" 'on_halt = "exit"' "faults = \"$d/faults.txt\""
run "$d" >/dev/null; rm -f "$d/checkpoint"
grep -q '^fault: hang:2@100 caught=100 latency=0 how=lost$' "$d/summary.txt" && grep -q '^100,halted,0$' "$d/events.csv" && pass "hung last replica: lost at once, run halted" || fail "hung last replica: $(grep '^fault' "$d/summary.txt")"

# 17. A bit flip on the last replica: caught when it hits one computation,
#     through when it hits both alike (the honest cost of self-check).
faults "$d" kill:0@10 kill:1@20 corrupt:2@500
conf "$d" "max_cycles = 600" "faults = \"$d/faults.txt\""
one=$(run "$d"); rm -f "$d/checkpoint"
faults "$d" kill:0@10 kill:1@20 corrupt2:2@500
conf "$d" "max_cycles = 600" "faults = \"$d/faults.txt\""
two=$(run "$d"); rm -f "$d/checkpoint"
# (cycle 20, when the lone survivor's single reply could not be self-checked
# yet, is detected in both runs; the flip at 500 adds one in the first)
if [ "$(field detected "$one")" = 2 ] && [ "$(field wrong "$one")" = 0 ] && [ "$(field detected "$two")" = 1 ] && [ "$(field wrong "$two")" = 1 ] && grep -q 'caught=0' "$d/summary.txt"; then
    pass "self-check: a flip in one computation is detected; in both it gets through"
else
    fail "self-check flips ($one / $two)"
fi

# 18. Outvotes are logged one per cycle, and a run of them collapses.
faults "$d" corrupt:2@50 stuck:1@100
conf "$d" "max_cycles = 300" "faults = \"$d/faults.txt\""
run "$d" >/dev/null; rm -f "$d/checkpoint"
if grep -q '^50,outvoted,2$' "$d/events.csv" && grep -q '^102,outvoted,1 repeated 3x cycles 100-102$' "$d/events.csv" &&
    grep -q '^102,retired,1 strikes$' "$d/events.csv" && [ "$(grep -c ',outvoted,' "$d/events.csv")" = 3 ]; then
    pass "outvoted: one line per outvote, a run collapses to one line plus its count"
else
    fail "outvoted lines: $(grep ',outvoted,' "$d/events.csv" | tr '\n' ' ')"
fi

# 19. The CPU-share nap leaves out the wait for a lost replica: with a 1 s
#     reply timeout and a 5% share, 200 cycles still finish in seconds.
faults "$d" hang:1@50
conf "$d" "max_cycles = 200" "cpu_percent = 5" "reply_timeout_ms = 1000" "faults = \"$d/faults.txt\""
start=$(date +%s)
run "$d" >/dev/null; rm -f "$d/checkpoint"
wall=$(( $(date +%s) - start ))
grep -q '^50,lost,1$' "$d/events.csv" && [ "$wall" -lt 12 ] && pass "lost wait is not work: 200 cycles with a hang in ${wall}s at a 5% share" || fail "lost wait: ${wall}s"

# 20. The pilot schedule with restarts: the run halts when the last replica
#     is retired by the periodic test, the hung replica comes back after the
#     cool-down, the stuck ones fail their tests again, nothing wrong delivered.
conf "$d" "max_cycles = 1400" "restart_retired = true" "cooldown_s = 2" "reply_timeout_ms = 200" "faults = \"$PWD/tasks/pilot/faults.txt\""
out=$(run "$d"); rm -f "$d/checkpoint"
if grep -q '^1280,retired,0 periodic$' "$d/events.csv" && grep -q '^1280,halted,0$' "$d/events.csv" &&
    grep -q '^1280,restarted,2$' "$d/events.csv" && grep -q ',retired,1 restart' "$d/events.csv" &&
    [ "$(field wrong "$out")" = 0 ] && grep -q 'stopped=completed' "$d/summary.txt"; then
    pass "pilot schedule: halt at 1280, replica 2 back after the cool-down, stuck ones stay retired, wrong=0"
else
    fail "pilot schedule ($out; $(grep -E 'halted|restarted|retired' "$d/events.csv" | tr '\n' ' '))"
fi

# 21. A halt is kept across a restart of the supervisor: one halted line,
#     resumed as halted, the heartbeat says so.
faults "$d" kill:0@10 kill:1@20 kill:2@30
conf "$d" "period_ms = 1" "heartbeat = \"file:$d/hb\"" "heartbeat_every_s = 1" "faults = \"$d/faults.txt\""
"$F" run "$d/field.conf" >/dev/null & pid=$!
i=0; while ! grep -q 'halted=1' "$d/checkpoint" 2>/dev/null && [ $i -lt 100 ]; do sleep 0.1; i=$((i + 1)); done
sleep 1.3
kill -TERM "$pid"; wait "$pid" || true
"$F" run "$d/field.conf" >/dev/null & pid=$!
sleep 0.6
kill -TERM "$pid"; wait "$pid" || true
if [ "$(grep -c ',halted,' "$d/events.csv")" = 1 ] && grep -q '^30,resumed,1 halted$' "$d/events.csv" && grep -q '^MRU,30,halted,0,' "$d/hb"; then
    pass "halt kept across a restart: one halted line, resumed as halted, heartbeat says halted"
else
    fail "halt across restart: $(grep -E 'halted|resumed' "$d/events.csv" | tr '\n' ' ') hb=$(cat "$d/hb")"
fi
rm -f "$d/checkpoint"

# 22. The cli adapter delivers the same as the server protocol.
printf '#!/bin/sh\nexec "%s" --once\n' "$T" >"$d/once.sh"; chmod +x "$d/once.sh"
conf "$d" "max_cycles = 200" "output = \"file:$d/out-server\""
run "$d" >/dev/null; rm -f "$d/checkpoint"
conf "$d" "max_cycles = 200" "output = \"file:$d/out-cli\"" 'adapter = "cli"' "task = \"$d/once.sh\"" "oracle = \"$d/once.sh\""
out=$(run "$d"); rm -f "$d/checkpoint"
cmp -s "$d/out-server" "$d/out-cli" && [ "$(field delivered "$out")" = 200 ] && pass "cli adapter: same 200 outputs as the server protocol" || fail "cli adapter ($out)"

# 23. The determinism test passes on the demo task.
conf "$d"
out=$("$F" check-task "$d/field.conf" --requests 10000)
[ "$out" = "requests=10000 mismatches=0 lost=0" ] && pass "check-task: $out" || fail "check-task: $out"

rm -rf "$d"
if [ "$fails" -gt 0 ]; then
    echo "$fails check(s) failed"
    exit 1
fi
echo "all checks passed"
