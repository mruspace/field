#!/bin/sh
# Staged-update bench (docs/spec.md, section 10). Each bad build of the demo
# task (a --bug switch) is brought in two ways on the same 5000 cycles:
#   all-at-once  every slot runs the new build from the start (task = it)
#   staged       the new build comes in by staged update (candidate = it)
# The oracle is the old build, so `wrong` counts outputs delivered that the
# old build would not have given. Writes one directory per run and
# staged.csv to OUT (default: a temporary directory), and prints the CSV.
# Usage: ./scripts/staged.sh [OUT]
set -eu
cd "$(dirname "$0")/.."
if [ -z "${FIELD_BIN:-}" ]; then
    cargo build --release --quiet
fi
F=${FIELD_BIN:-target/release/field}
T=${FIELD_TASK:-target/release/field-demo-task}
# paths stay as given (relative to the repository root, where every run
# starts), so the files a run writes name no machine
out=${1:-$(mktemp -d)}
mkdir -p "$out"

cycles=5000
trial=1000
field() { printf '%s\n' "$2" | tr ' ' '\n' | sed -n "s/^$1=//p" | head -n 1; }

csv="$out/staged.csv"
echo "bug,rollout,delivered,detected,wrong,alive,halted_at,update" >"$csv"
for bug in none wrong@500 hang@500 crash@500 every@97 kat wrong@1500 hang@1500 crash@1500 wrong@2500; do
    for rollout in all-at-once staged; do
        d="$out/$(printf '%s' "$bug" | tr '@' '-')-$rollout"
        rm -rf "$d"; mkdir -p "$d"
        printf '#!/bin/sh\nexec "%s" --bug %s\n' "$T" "$bug" >"$d/new-build.sh"
        chmod +x "$d/new-build.sh"
        {
            if [ "$rollout" = staged ]; then
                printf 'task = "%s"\ncandidate = "%s/new-build.sh"\ncandidate_cycles = %s\n' "$T" "$d" "$trial"
            else
                printf 'task = "%s/new-build.sh"\n' "$d"
            fi
            printf 'input = "tick"\nperiod_ms = 0\noutput = "none"\ndir = "%s"\non_halt = "exit"\n' "$d"
            printf 'kat_input = "tasks/demo/kat.in"\nkat_output = "tasks/demo/kat.out"\noracle = "%s"\n' "$T"
            printf 'max_cycles = %s\nreply_timeout_ms = 200\n' "$cycles"
        } >"$d/field.conf"
        s=$("$F" run "$d/field.conf" || true)
        l2=$(printf '%s\n' "$s" | sed -n 2p)
        l3=$(printf '%s\n' "$s" | sed -n 3p)
        u=$(printf '%s\n' "$s" | grep '^update: ' | tr ' ' '\n' | sed -n 's/^state=//p')
        echo "$bug,$rollout,$(field delivered "$l2"),$(field detected "$l2"),$(field wrong "$l2"),$(field alive "$l2"),$(field halted_at "$l3"),${u:--}" >>"$csv"
        rm -f "$d/checkpoint"
    done
done
cat "$csv"
