#!/usr/bin/env python3
"""Derive the pilot per-container profile from the exact V100 Docker baseline."""

import argparse
import hashlib
import json
import os
from pathlib import Path

BASE_SHA256 = "01536f1d1df938ae611eba20d6349e0de7a99b6ecdee1549427a0b01b8301e28"
NS_MASK = 0x7E020000  # Moby's existing amd64 clone namespace mask.
USER, PID, NET, SIGCHLD = 0x10000000, 0x20000000, 0x40000000, 17


def allow(name, value, mask=None):
    arg = {"index": 0, "value": value, "op": "SCMP_CMP_EQ"}
    if mask is not None:
        arg.update(value=mask, valueTwo=value, op="SCMP_CMP_MASKED_EQ")
    return {
        "names": [name],
        "action": "SCMP_ACT_ALLOW",
        "args": [arg],
        "includes": {"arches": ["amd64"]},
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    baseline = args.baseline.read_bytes()
    if hashlib.sha256(baseline).hexdigest() != BASE_SHA256:
        parser.error("not the reviewed Docker 28.5.2 amd64 profile source")
    profile = json.loads(baseline)
    assert profile["defaultAction"] == "SCMP_ACT_ERRNO"
    # Chromium probes user namespaces, starts a zygote in its own
    # user/PID/network namespace and forks renderers into child PID namespaces.
    # This is additive to Moby, with no extra outer-container capabilities.
    profile["syscalls"] += [
        *(allow("clone", flags, NS_MASK) for flags in
          (USER, USER | PID, USER | NET, USER | PID | NET, PID | NET)),
        allow("unshare", USER),
        # chroot remains capability-checked by the kernel, in the nested
        # Chromium sandbox. The outer container drops every capability.
        {"names": ["chroot"], "action": "SCMP_ACT_ALLOW", "includes": {"arches": ["amd64"]}},
        allow("clone", PID | SIGCHLD),
    ]
    value = (json.dumps(profile, indent=2) + "\n").encode()
    with os.fdopen(os.open(args.output, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "wb") as out:
        out.write(value)
    print(hashlib.sha256(value).hexdigest())


if __name__ == "__main__":
    main()
