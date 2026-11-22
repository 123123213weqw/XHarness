#!/usr/bin/env python3
"""Build the opt-in container Browser Use ZIP from an already-built bundle."""

import argparse
import hashlib
import json
import stat
import zipfile
from pathlib import Path

PLUGIN = Path(__file__).resolve().parents[2] / "plugins" / "browser-use"
MODULES = ("playwright", "playwright-core")
CONTAINER = ("launcher.mjs", "worker.mjs", "settings.mjs", "docker.mjs", "backend.mjs")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    if not (PLUGIN / "dist" / "protocol.js").is_file():
        parser.error("build plugins/browser-use first")
    lock = json.loads((PLUGIN / "package-lock.json").read_text())["packages"]
    entries = {".mcp.json": PLUGIN / "container/mcp.json"}
    root_files = [PLUGIN / name for name in ("package.json", ".claude-plugin/plugin.json")]
    runtime = [*root_files, *sorted((PLUGIN / "dist").rglob("*.js"))]
    runtime += [PLUGIN / "container" / name for name in CONTAINER]
    for name in MODULES:
        root = PLUGIN / "node_modules" / name
        installed = json.loads((root / "package.json").read_text())["version"]
        if installed != lock[f"node_modules/{name}"]["version"]:
            parser.error(f"{name} does not match the Browser Use lock")
        for path in [root, *sorted(root.rglob("*"))]:
            if path.is_symlink():
                parser.error("symlinks are not allowed in the browser bundle")
            if path.is_file():
                runtime.append(path)
    entries.update((path.relative_to(PLUGIN).as_posix(), path) for path in runtime)
    if len(entries) > 2048 or sum(path.stat().st_size for path in entries.values()) > 128 * 2**20:
        parser.error("bundle exceeds Host installation limits")

    # No workspace, operator config, profile, browser state or dev dependencies
    # are members of this archive. Fixed metadata gives reproducible digests.
    with zipfile.ZipFile(args.output, "x", compression=zipfile.ZIP_DEFLATED) as out:
        for name, path in sorted(entries.items()):
            if not stat.S_ISREG(path.lstat().st_mode):
                parser.error("bundle member is not a regular file")
            member = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
            member.create_system = 3
            member.external_attr = (stat.S_IFREG | 0o644) << 16
            out.writestr(member, path.read_bytes(), zipfile.ZIP_DEFLATED)
    print(hashlib.sha256(args.output.read_bytes()).hexdigest())


if __name__ == "__main__":
    main()
