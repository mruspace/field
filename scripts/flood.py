#!/usr/bin/env python3
"""Flood bench (docs/spec.md, section 11). Three parts, each run by the real
supervisor with the demo task on this machine:

  lock     a new build with the net@500 bug: every output right, and from
           cycle 500 it sends 100 UDP datagrams per request. Brought in all
           at once and by staged update, with task_network = allow and
           deny. A live listener counts the datagrams that arrive.
  backlog  a TCP source 10 and 100 times faster than the cycle, under
           input_backlog = queue and newest. Each record carries its send
           time; the task echoes it, and a TCP collector measures how old
           the input was when its output arrived.
  stall    a TCP output peer that stops reading. The run must end with
           an error at the write deadline instead of hanging. With
           OLD_FIELD_BIN set, the same run of that build is shown too.

Writes lock.csv, backlog.csv and stall.csv to OUT (default: a temporary
directory) and prints them. Usage: python3 -I scripts/flood.py [OUT]
"""

import os
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
F = os.environ.get("FIELD_BIN", os.path.join(ROOT, "target/release/field"))
T = os.environ.get("FIELD_TASK", os.path.join(ROOT, "target/release/field-demo-task"))
OUT = sys.argv[1] if len(sys.argv) > 1 else tempfile.mkdtemp()
os.makedirs(OUT, exist_ok=True)


def field_of(text, name):
    for w in text.replace(",", " ").split():
        if w.startswith(name + "="):
            return w[len(name) + 1 :]
    return ""


def write_conf(d, lines):
    os.makedirs(d, exist_ok=True)
    for f in ("checkpoint", "events.csv", "summary.txt"):
        try:
            os.remove(os.path.join(d, f))
        except FileNotFoundError:
            pass
    path = os.path.join(d, "field.conf")
    with open(path, "w") as f:
        f.write("".join(f"{line}\n" for line in lines + [f'dir = "{d}"']))
    return path


def wrapper(d, args):
    path = os.path.join(d, "new-build.sh")
    with open(path, "w") as f:
        f.write(f'#!/bin/sh\nexec "{T}" {args}\n')
    os.chmod(path, 0o755)
    return path


def lock():
    rows = ["network,rollout,delivered,wrong,alive,update,datagrams_received"]
    for network in ("allow", "deny"):
        for rollout in ("all-at-once", "staged"):
            d = os.path.join(OUT, f"lock-{network}-{rollout}")
            os.makedirs(d, exist_ok=True)
            build = wrapper(d, "--bug net@500")
            rx = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            rx.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8 << 20)
            rx.bind(("127.0.0.1", 0))
            rx.settimeout(0.5)
            count = [0]
            done = threading.Event()

            def listen():
                while not done.is_set():
                    try:
                        rx.recv(64)
                        count[0] += 1
                    except socket.timeout:
                        pass

            th = threading.Thread(target=listen)
            th.start()
            task = [f'task = "{T}"', f'candidate = "{build}"', "candidate_cycles = 500"]
            if rollout == "all-at-once":
                task = [f'task = "{build}"']
            conf = write_conf(
                d,
                task
                + [
                    'input = "tick"',
                    "period_ms = 2",
                    'output = "none"',
                    'on_halt = "exit"',
                    f'kat_input = "{ROOT}/tasks/demo/kat.in"',
                    f'kat_output = "{ROOT}/tasks/demo/kat.out"',
                    f'oracle = "{T}"',
                    "max_cycles = 2000",
                    f'task_network = "{network}"',
                ],
            )
            env = dict(os.environ, FIELD_DEMO_NET="127.0.0.1:%d" % rx.getsockname()[1])
            s = subprocess.run([F, "run", conf], capture_output=True, text=True, env=env).stdout
            time.sleep(1.0)
            done.set()
            th.join()
            rx.close()
            l2 = s.splitlines()[1]
            upd = next((field_of(x, "state") for x in s.splitlines() if x.startswith("update:")), "-")
            rows.append(
                f"{network},{rollout},{field_of(l2, 'delivered')},{field_of(l2, 'wrong')},"
                f"{field_of(l2, 'alive')},{upd},{count[0]}"
            )
    return rows


def backlog():
    rows = ["rate_per_s,backlog,cycles,delivered,skipped,age_median_ms,age_max_ms,age_last_ms,supervisor_max_rss_kib"]
    period_ms, cycles = 10, 1000
    for rate in (1000, 10000):
        for mode in ("queue", "newest"):
            d = os.path.join(OUT, f"backlog-{rate}-{mode}")
            os.makedirs(d, exist_ok=True)
            # the echo task's known answer is its input
            with open(os.path.join(d, "kat.in"), "wb") as f:
                f.write(bytes(16))
            with open(os.path.join(d, "kat.out"), "wb") as f:
                f.write(bytes(16))
            task = wrapper(d, "--echo")
            src = socket.socket()
            src.bind(("127.0.0.1", 0))
            src.listen(1)
            dst = socket.socket()
            dst.bind(("127.0.0.1", 0))
            dst.listen(1)
            stop = threading.Event()
            ages = []

            def feed():
                c, _ = src.accept()
                seq, t0, batch = 0, time.monotonic(), max(1, rate // 1000)
                while not stop.is_set():
                    due = int((time.monotonic() - t0) * rate)
                    if seq >= due:
                        time.sleep(0.0005)
                        continue
                    buf = b"".join(
                        struct.pack("<IQQ", 16, seq + i, time.monotonic_ns()) for i in range(min(batch, due - seq))
                    )
                    try:
                        c.sendall(buf)
                    except OSError:
                        break
                    seq += min(batch, due - seq)
                c.close()

            def collect():
                c, _ = dst.accept()
                data = b""
                while True:
                    chunk = c.recv(65536)
                    if not chunk:
                        break
                    now = time.monotonic_ns()
                    data += chunk
                    while len(data) >= 12 + 16:
                        _, n = struct.unpack("<QI", data[:12])
                        _, sent = struct.unpack("<QQ", data[12 : 12 + 16])
                        ages.append((now - sent) / 1e6)
                        data = data[12 + n :]
                c.close()

            tf, tc = threading.Thread(target=feed), threading.Thread(target=collect)
            tf.start()
            tc.start()
            conf = write_conf(
                d,
                [
                    f'task = "{task}"',
                    f'input = "tcp:127.0.0.1:{src.getsockname()[1]}"',
                    f'output = "tcp:127.0.0.1:{dst.getsockname()[1]}"',
                    f"period_ms = {period_ms}",
                    f'input_backlog = "{mode}"',
                    f'kat_input = "{d}/kat.in"',
                    f'kat_output = "{d}/kat.out"',
                    f"max_cycles = {cycles}",
                    "record_max = 64",
                ],
            )
            s = subprocess.run([F, "run", conf], capture_output=True, text=True).stdout
            stop.set()
            tf.join()
            tc.join()
            src.close()
            dst.close()
            l2 = s.splitlines()[1]
            fp = next(x for x in s.splitlines() if x.startswith("footprint:"))
            skipped = next((field_of(x, "skipped") for x in s.splitlines() if x.startswith("input:")), "0")
            rss = field_of(fp, "supervisor_max_rss").removesuffix("KiB")
            rows.append(
                f"{rate},{mode},{field_of(l2, 'cycles')},{field_of(l2, 'delivered')},{skipped},"
                f"{statistics.median(ages):.1f},{max(ages):.1f},{ages[-1]:.1f},{rss}"
            )
    return rows


def stall():
    rows = ["build,outcome,seconds,stopped,exit"]
    builds = [("this", F)]
    if os.environ.get("OLD_FIELD_BIN"):
        builds.insert(0, ("old", os.environ["OLD_FIELD_BIN"]))
    for name, binary in builds:
        d = os.path.join(OUT, f"stall-{name}")
        peer = socket.socket()
        # a small receive buffer, so the stall comes after a few outputs
        peer.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
        peer.bind(("127.0.0.1", 0))
        peer.listen(1)
        held = []
        th = threading.Thread(target=lambda: held.append(peer.accept()[0]))
        th.start()
        conf = write_conf(
            d,
            [
                f'task = "{T}"',
                'input = "tick"',
                "period_ms = 0",
                f'output = "tcp:127.0.0.1:{peer.getsockname()[1]}"',
                f'kat_input = "{ROOT}/tasks/demo/kat.in"',
                f'kat_output = "{ROOT}/tasks/demo/kat.out"',
                "max_seconds = 30",
            ],
        )
        t0 = time.monotonic()
        p = subprocess.Popen([binary, "run", conf], stdout=subprocess.PIPE, text=True)
        try:
            out, _ = p.communicate(timeout=60)
            secs = time.monotonic() - t0
            stopped = next((field_of(x, "stopped") for x in out.splitlines() if x.startswith("stopped=")), "-")
            rows.append(f"{name},ended,{secs:.1f},{stopped},{p.returncode}")
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()
            rows.append(f"{name},hung (killed at 60 s; the 30 s run cap never ran),60.0,-,killed")
        th.join()
        for c in held:
            c.close()
        peer.close()
    return rows


for name, part in (("lock", lock), ("backlog", backlog), ("stall", stall)):
    rows = part()
    # the checkpoints are spent; the configuration, events and summary stay
    for d in os.listdir(OUT):
        try:
            os.remove(os.path.join(OUT, d, "checkpoint"))
        except (FileNotFoundError, NotADirectoryError):
            pass
    with open(os.path.join(OUT, f"{name}.csv"), "w") as f:
        f.write("\n".join(rows) + "\n")
    print("\n".join(rows) + "\n")
