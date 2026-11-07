"""Render release-history fixtures with git-cliff before changing cliff.toml."""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CLIFF = os.environ.get("GIT_CLIFF", "git-cliff")


class ChangelogRendering(unittest.TestCase):
    def test_initial_and_subsequent_releases(self):
        with tempfile.TemporaryDirectory(prefix="niri-changelog-") as directory:
            def git(*args):
                return subprocess.run(
                    ["git", "-C", directory, *args], check=True,
                    capture_output=True, text=True,
                ).stdout

            def render(version):
                text = subprocess.run(
                    [CLIFF, "--offline", "--no-exec", "--config",
                     str(ROOT / "cliff.toml"), "--tag", version],
                    cwd=directory, check=True, capture_output=True, text=True,
                ).stdout
                self.assertNotRegex(text, r"\n[ \t]*\n[ \t]*\n")
                self.assertTrue(text.endswith("\n"))
                lines = text.splitlines()
                for i, line in enumerate(lines):
                    if line.startswith("#"):
                        if i:
                            self.assertEqual(lines[i - 1], "")
                        if i + 1 < len(lines):
                            self.assertEqual(lines[i + 1], "")
                return text

            git("init", "--initial-branch=main")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            git("commit", "--allow-empty", "-m", "feat: add browsing")
            git("commit", "--allow-empty", "-m", "docs: hidden notes")
            initial = render("1.0.0")
            self.assertIn("### Added\n\n- Add browsing", initial)
            self.assertNotIn("hidden notes", initial)
            self.assertNotIn("### Fixed", initial)
            self.assertIn("/releases/tag/1.0.0", initial)
            git("tag", "-a", "1.0.0", "-m", "First release")
            for message in (
                "feat: add preview", "perf: speed up listing",
                "deprecate: old format", "remove: old option",
                "fix: repair transfers", "fix(security): restrict access",
                "docs: more hidden notes",
            ):
                git("commit", "--allow-empty", "-m", message)
            following = render("1.1.0")
            for group in ("Added", "Changed", "Deprecated", "Removed", "Fixed", "Security"):
                self.assertIn("### " + group, following)
            self.assertNotIn("hidden notes", following)
            self.assertEqual(len(re.findall(r"^## \[", following, re.M)), 2)
            self.assertIn("/compare/1.0.0...1.1.0", following)


if __name__ == "__main__":
    unittest.main()
