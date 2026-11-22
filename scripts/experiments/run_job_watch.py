#!/usr/bin/env python3
"""Run an isolated real three-stage Job and restart only its experimental Host."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid

INPUTS = (
    "crates/xharness-jobs/src/lib.rs",
    "crates/xharness-jobs/src/continuation.rs",
    "crates/xharness-host/src/job_continuation.rs",
    "crates/xharness-host/src/driver.rs",
    "crates/xharness-session/src/goal.rs",
    "crates/xharness-session/src/event.rs",
)
WORKER_TEST = "job_continuation::job_watch_experiment::isolated_host_worker"


def digest(data):
    return hashlib.sha256(data).hexdigest()


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def write_json(path, payload):
    tmp = path.with_suffix(".tmp")
    with tmp.open("x", encoding="utf-8") as f:
        json.dump(payload, f, ensure_ascii=False, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)
    sync_dir(path.parent)


def wake(root, message=b"wake"):
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as sock:
            sock.sendto(message, str(root / "watch.sock"))
            return True
    except (FileNotFoundError, ConnectionRefusedError):
        return False


def producer(root, source):
    run_id = (root / "EXPERIMENT_ONLY").read_text()
    output = root / "artifacts"

    def gate():
        assert sys.stdin.readline() == "next\n"

    def publish(seq, name, stage):
        write_json(root / "receipts" / f"{seq:03}.json", {
            "run_id": run_id, "seq": seq, "stage": stage, "worker_pid": os.getpid(),
            "sha256": digest((output / name).read_bytes()),
        })
        print(json.dumps({"pid": os.getpid(), "seq": seq, "stage": stage,
                          "notified_live": wake(root), "t_ns": time.time_ns()}), flush=True)

    print("PRODUCER_STARTED", os.getpid(), flush=True)
    gate()
    inventory = {rel: digest((source / rel).read_bytes()) for rel in INPUTS}
    write_json(output / "inventory.json", inventory)
    publish(1, "inventory.json", "inventory_ready")

    gate()
    tmp = output / "archive.tmp"
    with tarfile.open(tmp, "w:gz") as tar:
        for rel in INPUTS:
            tar.add(source / rel, arcname=rel, recursive=False)
        tar.add(output / "inventory.json", arcname="inventory.json")
    with tmp.open("rb") as f:
        os.fsync(f.fileno())
    os.replace(tmp, output / "snapshot.tar.gz")
    sync_dir(output)
    publish(2, "snapshot.tar.gz", "archive_ready")

    gate()
    with tarfile.open(output / "snapshot.tar.gz", "r:gz") as tar:
        assert set(tar.getnames()) == {*INPUTS, "inventory.json"}
        assert all(m.isfile() for m in tar.getmembers())
        assert json.load(tar.extractfile("inventory.json")) == inventory
        checked = {rel: digest(tar.extractfile(rel).read()) for rel in INPUTS}
        assert checked == inventory
    write_json(output / "validation.json", {"status": "verified", "checked": checked,
                                            "archive_sha256": digest((output / "snapshot.tar.gz").read_bytes())})
    publish(3, "validation.json", "completed")


def until(check, label, active=(), timeout=25):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        for proc in active:
            assert proc.poll() is None, (label, "process exited", proc.pid, proc.returncode)
        r = check()
        if r:
            return r
        time.sleep(.04)
    raise AssertionError(f"timeout: {label}")


def read_rows(path):
    if not path.exists():
        return []
    raw = path.read_bytes()
    return [json.loads(line) for line in raw.splitlines()[:raw.count(b"\n")] if line.startswith(b"{")]


def session_state(root):
    rows = read_rows(root / "sessions" / "job-follow.jsonl")
    kinds = []
    inbox = []
    def visit(node):
        if isinstance(node, dict):
            if node.get("type") in ("turn/start", "turn/end"):
                kinds.append(node["type"])
            if node.get("type") == "agent/inbox/spliced":
                inbox.extend(m["id"] for m in node["data"].get("inserted", [])
                             if m["id"].startswith("watch:"))
            for v in node.values():
                visit(v)
        elif isinstance(node, list):
            for v in node:
                visit(v)
    visit(rows)
    return {"starts": kinds.count("turn/start"), "ends": kinds.count("turn/end"), "inbox": inbox}


def run_case(binary, source, case, quiet_seconds):
    root = Path(tempfile.mkdtemp(prefix="xh-watch-", dir="/tmp"))
    run_id = uuid.uuid4().hex
    (root / "EXPERIMENT_ONLY").write_text(run_id)
    for folder in ("receipts", "artifacts", "verified"):
        (root / folder).mkdir()
    print("RUN_ROOT", root, flush=True)
    procs = []
    host_pids, measurements, checkpoints = [], [], []
    try:
        with (root / "producer.log").open("ab") as out:
            job = subprocess.Popen([sys.executable, __file__, "--producer",
                                    str(root), "--source", str(source)], stdin=subprocess.PIPE,
                                   stdout=out, stderr=subprocess.STDOUT)
        procs.append(job)

        def boot(phase):
            log = root / f"host-{len(host_pids) + 1}.log"
            with log.open("ab") as out:
                host = subprocess.Popen([str(binary), WORKER_TEST, "--ignored", "--exact",
                                         "--nocapture", "--test-threads=1"],
                                        stdout=out, stderr=subprocess.STDOUT,
                                        env={**os.environ, "XH_WATCH_ROOT":str(root), "XH_WATCH_PHASE":phase})
            procs.append(host)
            host_pids.append(host.pid)
            until(lambda: f"WATCH_READY {host.pid}" in log.read_text(), f"Host ready ({log})", [host])
            return host, log

        def provider():
            return read_rows(root / "provider.jsonl")

        def idle(label, calls, turns, host=None):
            before = time.monotonic()
            rows = provider()
            assert len(rows) == calls, (label, rows)
            time.sleep(quiet_seconds)
            if host is not None:
                assert host.poll() is None
            after = provider()
            state = session_state(root)
            assert after == rows, (label, rows, after)
            assert (state["starts"], state["ends"]) == (turns, turns), (label, state)
            measurements.append({"label":label, "seconds":round(time.monotonic()-before, 3),
                                 "new_provider_calls":len(after)-len(rows), "turns":turns})

        def settled(n, host):
            state = until(lambda: (s if (s := session_state(root))["ends"] == n+1 else None),
                          f"closed turn after checkpoint {n}", [host])
            assert state["starts"] == n+1, state
            assert len(provider()) == 1+2*n, provider()
            assert state["inbox"] == [f"watch:{run_id}:{i}" for i in range(1, n+1)], state
            if n:
                expected = json.loads((root / "receipts" / f"{n:03}.json").read_text())
                actual = json.loads((root / "verified" / f"{n:03}.json").read_text())
                assert actual == expected
                assert len(list((root / "verified").glob("*.json"))) == n

        def checkpoint(n, host=None):
            assert job.poll() is None and job.stdin is not None
            job.stdin.write(b"next\n")
            job.stdin.flush()
            row = until(lambda: next((r for r in read_rows(root / "producer.log") if r["seq"] == n), None),
                        f"real Job checkpoint {n}", [] if host is None else [host])
            assert row["pid"] == job.pid
            assert row["notified_live"] == (host is not None), row
            if host is None:
                assert len(provider()) == (5 if case == "follow" else 1), provider()
            else:
                settled(n, host)
                row["verification_ms_after_receipt"] = round(
                    ((root / "verified" / f"{n:03}.json").stat().st_mtime_ns-row["t_ns"])/1e6, 1)
            checkpoints.append(row)

        a, log = boot("initial")
        settled(0, a)
        idle("initial_wait", 1, 1, a)
        if case == "stop":
            assert wake(root, b"stop")
            until(lambda: "WATCH_STOPPED" in log.read_text(), "explicit Stop", [a])
            a.kill()
            a.wait(timeout=5)
            for n in range(1, 4):
                checkpoint(n)
            assert job.wait(timeout=5) == 0
            c, log = boot("restore")
            idle("stopped_then_restarted_no_wake", 1, 1, c)
            assert not list((root / "verified").glob("*.json"))
        elif case == "backlog":
            a.kill()
            a.wait(timeout=5)
            for n in range(1, 4):
                checkpoint(n)
            assert job.wait(timeout=5) == 0
            boot_started = time.time_ns()
            c, _ = boot("restore")
            settled(3, c)
            assert [r["seq"] for r in provider()] == [None, 1, 1, 2, 2, 3, 3]
            for row in checkpoints:
                row["catchup_ms_after_new_host_start"] = round(
                    ((root / "verified" / f'{row["seq"]:03}.json').stat().st_mtime_ns-boot_started)/1e6,1)
            assert wake(root) and wake(root)
            idle("offline_backlog_replays_in_order_once", 7, 4, c)
        else:
            checkpoint(1, a)
            assert wake(root) and wake(root)
            idle("duplicate_signals_are_inert", 3, 2, a)
            a.kill()
            a.wait(timeout=5)
            assert job.poll() is None
            b, _ = boot("restore")
            idle("restart_while_same_job_is_still_waiting", 3, 2, b)
            checkpoint(2, b)
            b.kill()
            b.wait(timeout=5)
            checkpoint(3)
            assert job.wait(timeout=5) == 0
            # The worker completed while the socket's owner was absent.
            # No signal is injected here: boot recovery reads its journal.
            boot_started = time.time_ns()
            c, _ = boot("restore")
            settled(3, c)
            checkpoints[-1]["catchup_ms_after_new_host_start"] = round(
                ((root / "verified" / "003.json").stat().st_mtime_ns
                 - boot_started)/1e6, 1
            )
            idle("replayed_offline_terminal_checkpoint_once", 7, 4, c)
            c.kill()
            c.wait(timeout=5)
            c, _ = boot("restore")
            idle("second_restart_does_not_replay", 7, 4, c)
            assert len(set(host_pids)) == 4
        assert wake(root, b"quit")
        assert c.wait(timeout=10) == 0
        state = session_state(root)
        return {"case":case, "root":str(root), "run_id":run_id,
                "job_pid":job.pid, "exit_code":job.returncode, "host_pids":host_pids,
                "provider_calls":provider(), "turns":state, "quiet_windows":measurements,
                "checkpoints":checkpoints, "archive_bytes":(root / "artifacts" / "snapshot.tar.gz").stat().st_size,
                "verified_artifacts":list(sorted(p.name for p in (root / "verified").glob("*.json")))}
    finally:
        for p in reversed(procs):
            if p.poll() is None:
                p.kill()
                p.wait(timeout=5)
            if p.stdin:
                p.stdin.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--producer", type=Path)
    parser.add_argument("--quiet-seconds", type=int, default=3)
    parser.add_argument("--cases", nargs="+", choices=("follow", "backlog", "stop"), default=["follow","backlog","stop"])
    args = parser.parse_args()
    if args.producer:
        producer(args.producer, args.source)
    else:
        assert args.binary and args.out and 1 <= args.quiet_seconds <= 120
        report = {"provider":"scripted fixture, real Host/Agent/ToolExecutor", "real_job":True,
                  **{case:run_case(args.binary,args.source,case,args.quiet_seconds) for case in args.cases}}
        write_json(args.out, report)
        print(json.dumps(report, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
