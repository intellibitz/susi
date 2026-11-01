"""Exercise the release-only classifier against real Git trees."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "ci-release-only.py"
NAMES = ["susi", "susi-daemon", "susi-gawd", "susi-gemi", "susi-gmcp", "susi-harness", "susi-server"]
MANIFESTS = ["Cargo.toml"] + [f"crates/{n}/Cargo.toml" for n in NAMES[1:]]
METADATA = [".agents/evidence.json", ".agents/identity.json", ".agents/roadmap.json"]


class ReleaseOnlyBranch(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "fixture")
        self.git("config", "user.email", "fixture@example.test")
        for path, name in zip(MANIFESTS, NAMES):
            self.put(path, f'[package]\nname = "{name}"\nversion = "1.2.3"\n')
        for path in METADATA:
            self.put(path, '{"version":"1.2.3","meaning":"preserve"}\n')
        self.put("README.md", "release 1.2.3\n")
        self.put("Cargo.lock", "version = 4\n" + "".join(
            f'\n[[package]]\nname = "{name}"\nversion = "1.2.3"\n' for name in NAMES))
        self.git("add", "-A")
        self.git("commit", "-qm", "base")
        self.base = self.git("rev-parse", "HEAD").strip()
        for path in MANIFESTS + METADATA + ["README.md", "Cargo.lock"]:
            file = self.root / path
            file.write_bytes(file.read_bytes().replace(b"1.2.3", b"1.2.4"))
        self.commit()

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, stderr=subprocess.PIPE, text=True)

    def put(self, name, contents):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-qm", "change")

    def accepted(self):
        env = dict(os.environ, BASE_REF=self.base)
        return subprocess.run([sys.executable, str(SCRIPT)], cwd=self.root, env=env, check=False).returncode == 0

    def test_exact_release_is_accepted(self):
        self.assertTrue(self.accepted())

    def test_source_change_is_rejected(self):
        self.put("src/lib.rs", "pub fn changed() {}\n")
        self.commit()
        self.assertFalse(self.accepted())

    def test_dependency_change_is_rejected(self):
        path = self.root / "Cargo.lock"
        path.write_text(path.read_text() + '\n[[package]]\nname = "unexpected"\nversion = "9.0.0"\n')
        self.commit()
        self.assertFalse(self.accepted())

    def test_metadata_and_readme_changes_are_rejected(self):
        for name in METADATA + ["README.md"]:
            with self.subTest(name=name):
                bump = self.git("rev-parse", "HEAD").strip()
                path = self.root / name
                path.write_bytes(path.read_bytes() + b"extra\n")
                self.commit()
                self.assertFalse(self.accepted())
                self.git("reset", "--hard", "-q", bump)

    def test_missing_version_change_is_rejected(self):
        self.git("checkout", self.base, "--", "crates/susi-gawd/Cargo.toml")
        self.commit()
        self.assertFalse(self.accepted())

    def test_inconsistent_lock_package_is_rejected(self):
        path = self.root / "Cargo.lock"
        path.write_text(path.read_text().replace('name = "susi-gawd"', 'name = "other"'))
        self.commit()
        self.assertFalse(self.accepted())

    def test_wrong_base_and_invalid_version_fail_closed(self):
        env = dict(os.environ, BASE_REF="refs/heads/missing")
        self.assertNotEqual(subprocess.run([sys.executable, str(SCRIPT)], cwd=self.root, env=env).returncode, 0)
        for name in MANIFESTS + METADATA + ["README.md", "Cargo.lock"]:
            path = self.root / name
            path.write_bytes(path.read_bytes().replace(b"1.2.4", b"1.2.2"))
        self.commit()
        self.assertFalse(self.accepted())


if __name__ == "__main__":
    unittest.main()
