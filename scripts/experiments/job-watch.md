# Job watch restart experiment

This is an opt-in Linux experiment, **not** a shipped Host feature. It uses a
real independent process to hash six tracked source files, archive them, and
validate the archive. The XHarness Host and its durable inbox are real; the
provider is scripted and no model endpoint or credential is needed.

On the authorized Rust build server (see repository AGENTS.md), in a source
checkout of this commit:

```sh
receipts=$(mktemp -d /tmp/job-watch-results-XXXXXX)
cargo test --locked -p xharness-host --lib --no-run --message-format=json > "$receipts/build.jsonl"
bin=$(python3 -c 'import json,sys; print(next(row["executable"] for row in map(json.loads, open(sys.argv[1])) if row.get("executable")))' "$receipts/build.jsonl")
python3 scripts/experiments/run_job_watch.py --source "$PWD" --binary "$bin" \
  --out "$receipts/results.json"
```

Use `--quiet-seconds 8` for longer quiet windows, or
`--cases follow backlog stop` to select cases. The harness starts all processes
it operates on and only kills or stops those processes. The reports identify
their private scratch directories.

- **Follow:** complete two checkpoints online with a Host restart between them;
  complete the Job while Host is offline; restore, then restart again to check
  that each checkpoint produces just one follow-up.
- **Backlog:** all three receipts arrive offline; verify three ordered
  follow-ups with no duplicates.
- **Stop:** stop the conversation before the Job finishes; restore and verify
  no model execution. **This does not cancel the subscription:** pending
  notifications can still be recorded.

The producer durably writes a receipt before sending its Unix datagram; Host
replays receipts on startup and uses its durable inbox to deduplicate by run
and sequence. This does not establish reliable delivery of a lost *online*
notification, waiting across a machine outage, exactly-once side effects, or
recovery from a crash during a follow-up. There is no new production watcher,
scheduler, or cancellation behavior.
