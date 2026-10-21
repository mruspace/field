#!/bin/sh
# Acceptance 5: the long bench run. start|stop|status for the supervisor on
# the demo task with the real cycle period, in the background, so a bench
# can run for days. Everything it writes goes to OUT_DIR.
set -eu
cd "$(dirname "$0")/.."
OUT_DIR=${OUT_DIR:-out/soak}
F=${FIELD_BIN:-target/release/field}
T=${FIELD_TASK:-target/release/field-demo-task}
PERIOD_MS=${PERIOD_MS:-1000}
CPU_PERCENT=${CPU_PERCENT:-5}
case "$1" in
start)
    mkdir -p "$OUT_DIR"
    [ -x "$F" ] || cargo build --release --quiet
    {
        printf 'task = "%s"\ninput = "tick"\nperiod_ms = %s\noutput = "file:%s/delivered"\ndir = "%s"\n' "$PWD/$T" "$PERIOD_MS" "$PWD/$OUT_DIR" "$PWD/$OUT_DIR"
        printf 'kat_input = "%s/tasks/demo/kat.in"\nkat_output = "%s/tasks/demo/kat.out"\n' "$PWD" "$PWD"
        printf 'cpu_percent = %s\nsummary_every = 3600\ncheckpoint_every = 100\n' "$CPU_PERCENT"
        [ -n "${FAULTS:-}" ] && printf 'faults = "%s"\nrestart_retired = true\ncooldown_s = 600\n' "$FAULTS"
    } >"$OUT_DIR/field.conf"
    nohup "$F" run "$OUT_DIR/field.conf" >"$OUT_DIR/stdout.txt" 2>"$OUT_DIR/stderr.txt" &
    echo $! >"$OUT_DIR/pid"
    echo "started pid $(cat "$OUT_DIR/pid"), writing to $OUT_DIR"
    ;;
stop)
    pid=$(cat "$OUT_DIR/pid")
    kill -TERM "$pid"
    i=0; while kill -0 "$pid" 2>/dev/null && [ $i -lt 300 ]; do sleep 0.1; i=$((i + 1)); done
    kill -0 "$pid" 2>/dev/null && kill -KILL "$pid"
    rm -f "$OUT_DIR/pid"
    sed -n '2,5p' "$OUT_DIR/summary.txt"
    ;;
status)
    if [ -f "$OUT_DIR/pid" ] && kill -0 "$(cat "$OUT_DIR/pid")" 2>/dev/null; then
        echo "running, pid $(cat "$OUT_DIR/pid")"
    else
        echo "not running"
    fi
    [ -f "$OUT_DIR/summary.txt" ] && sed -n '2,5p' "$OUT_DIR/summary.txt"
    ;;
*)
    echo "usage: scripts/soak.sh start|stop|status  (OUT_DIR, PERIOD_MS, CPU_PERCENT, FAULTS)"
    exit 2
    ;;
esac
