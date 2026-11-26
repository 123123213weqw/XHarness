#!/usr/bin/env python3
"""The retired auth bridge must not ship; retained native IPC stays main-only."""
from pathlib import Path
import json
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
DESKTOP = ROOT / "apps/desktop/src-tauri"


class RetiredAccountBoundary(unittest.TestCase):
    def test_no_account_handlers_callbacks_or_deep_link_dependency(self):
        config = json.loads((DESKTOP / "tauri.conf.json").read_text())
        self.assertNotIn("deep-link", config["plugins"])
        self.assertEqual(config["identifier"], "com.xlang.xharness")
        manifest = tomllib.loads((DESKTOP / "Cargo.toml").read_text())
        self.assertNotIn("tauri-plugin-deep-link", manifest["dependencies"])
        lock = tomllib.loads((DESKTOP / "Cargo.lock").read_text())
        self.assertNotIn("tauri-plugin-deep-link", [package["name"] for package in lock["package"]])
        for file in ("src/managed_account.rs", "src/account_callback.rs"):
            self.assertFalse((DESKTOP / file).exists())
        for file in ("build.rs", "src/lib.rs"):
            source = (DESKTOP / file).read_text()
            self.assertNotRegex(source, r"desktop_account_|managed_account|install_callbacks|tauri_plugin_deep_link|XHARNESS_ACCOUNT_ORIGIN")

    def test_no_capability_can_call_account_and_main_is_still_separate(self):
        for file in (DESKTOP / "capabilities").glob("*.json"):
            caps = json.loads(file.read_text())
            for permission in caps["permissions"]:
                name = permission if isinstance(permission, str) else permission["identifier"]
                self.assertNotRegex(name, r"desktop-account-|(?:^|:)deep-link")
        main = json.loads((DESKTOP / "capabilities/desktop-main.json").read_text())
        self.assertEqual(main["webviews"], ["main"])
        self.assertEqual(main["remote"]["urls"], ["http://127.0.0.1:*"])
        for command in ["status", "browser-navigate", "check-update"]:
            self.assertIn("allow-desktop-" + command, main["permissions"])
        self.assertIn("tauri_plugin_single_instance::init", (DESKTOP / "src/lib.rs").read_text())


if __name__ == "__main__":
    unittest.main()
