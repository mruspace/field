#!/bin/sh
# Acceptance 4: ten power cuts at random cycles. The supervisor is killed
# with SIGKILL ten times during one run and started again each time; the
# final run goes to completion. Every resume must continue the counts and
# keep every retired slot retired, and no event may be in the log twice.
set -eu
cd "$(dirname "$0")/.."
if [ -z "${FIELD_BIN:-}" ]; then
    cargo build --release --quiet
fi
F=${FIELD_BIN:-target/release/field}
T=${FIELD_TASK:-target/release/field-demo-task}
case "$F" in /*) ;; *) F="$PWD/$F" ;; esac
case "$T" in /*) ;; *) T="$PWD/$T" ;; esac
d=$(mktemp -d)
printf 'stuck:1@200\nhang:2@700\n' >"$d/faults.txt"
conf() {
    {
        printf 'task = "%s"\ninput = "tick"\nperiod_ms = %s\noutput = "file:%s/out"\ndir = "%s"\n' "$T" "$1" "$d" "$d"
        printf 'kat_input = "%s/tasks/demo/kat.in"\nkat_output = "%s/tasks/demo/kat.out"\noracle = "%s"\n' "$PWD" "$PWD" "$T"
        printf 'max_cycles = 3000\ncheckpoint_every = 20\nreply_timeout_ms = 200\nfaults = "%s/faults.txt"\n' "$d"
    } >"$d/field.conf"
}
field() { printf '%s\n' "$2" | tr ' ' '\n' | sed -n "s/^$1=//p" | head -n 1; }
# the resumed run adds two start-up known-answer tests per live replica at each resume
counts() { printf '%s\n' "$1" | sed 's/ kat_tests=[0-9]*//'; }

conf 0
want=$("$F" run "$d/field.conf" | sed -n 2p)
rm -f "$d/checkpoint" "$d/out" "$d/events.csv"

# ten cuts over at most 15 s of pauses; at 10 ms per cycle the run cannot
# reach its 3000 cycles before the tenth cut
conf 10
i=0
while [ $i -lt 10 ]; do
    "$F" run "$d/field.conf" >/dev/null & pid=$!
    # wait until this start is durable: the first checkpoint of a fresh run,
    # or the checkpoint a resume writes at once (its `resumed` line is in the
    # synced log by then); a cut before that leaves nothing to resume
    j=0
    while [ $j -lt 100 ]; do
        if [ $i = 0 ]; then [ -f "$d/checkpoint" ] && break
        else [ "$(grep -c ',resumed,' "$d/events.csv" 2>/dev/null || echo 0)" -ge $i ] && break; fi
        sleep 0.05; j=$((j + 1))
    done
    # then a pause of 0.3 to 1.4 s, different for each cut and each run
    sleep "$(awk -v n=$(( (i * 7919 + $$) % 12 )) 'BEGIN { printf "%.1f", 0.3 + n / 10 }')"
    kill -KILL "$pid"; wait "$pid" 2>/dev/null || true
    pkill -f "$T" 2>/dev/null || true
    sleep 0.2
    i=$((i + 1))
    echo "cut $i at checkpoint cycle $(sed -n 's/^cycle=//p' "$d/checkpoint")"
done
conf 0
got=$("$F" run "$d/field.conf" | sed -n 2p)

ok=1
resumes=$(grep -c ',resumed,' "$d/events.csv")
[ "$resumes" = 10 ] || { ok=0; echo "expected 10 resumed lines, got $resumes"; }
[ "$(grep -c ',retired,1 strikes$' "$d/events.csv")" = 1 ] || { ok=0; echo "retired,1 not exactly once"; }
[ "$(grep -c ',lost,2$' "$d/events.csv")" = 1 ] || { ok=0; echo "lost,2 not exactly once"; }
[ "$(counts "$got")" = "$(counts "$want")" ] || { ok=0; echo "counts differ: want '$want' got '$got'"; }
dups=$(grep -v ',progress,' "$d/events.csv" | sort | uniq -d | wc -l | tr -d ' ')
[ "$dups" = 0 ] || { ok=0; echo "$dups duplicated event line(s)"; grep -v ',progress,' "$d/events.csv" | sort | uniq -d; }
# the output file: one record per delivered cycle, strictly increasing
n=$(python3 - "$d/out" <<'PY' || echo "-"
import struct, sys
d = open(sys.argv[1], 'rb').read(); i = 0; n = 0; last = 0; bad = 0
while i < len(d):
    c, l = struct.unpack_from('<QI', d, i); i += 12 + l; n += 1
    if c <= last: bad += 1
    last = c
print(f"{n} {bad}")
PY
)
if [ "$n" != "-" ]; then
    set -- $n
    [ "$1" = "$(field delivered "$got")" ] && [ "$2" = 0 ] || { ok=0; echo "output file: $1 records, $2 out of order, delivered=$(field delivered "$got")"; }
else
    echo "note: python3 not found, the output file was not checked"
fi
rm -rf "$d"
if [ "$ok" = 1 ]; then
    echo "ok    power cuts: 10 resumes, counts equal an uninterrupted run, retired slots kept, no duplicate events"
else
    echo "FAIL  power cuts"
    exit 1
fi
