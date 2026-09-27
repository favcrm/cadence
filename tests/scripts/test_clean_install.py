"""CAD-317: adversarial acquisition/receipt validation for clean-install.

No repository binary executes here. GitHub calls are mocked; the actual
installer runs against local compact archives containing a harmless shell
executable. A simulated Darwin uname tests wiring, not native platform proof.
"""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
import zipfile
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[2] / "scripts/clean-install.py"


def make_tarball(tmp, tag, sha, target, run_id=7, attempt=1,
                 binary=b"fake-cadence", members=None, manifest_over=None):
    binary_sha = hashlib.sha256(binary).hexdigest()
    manifest = {
        "source_sha": sha, "version": tag, "run_id": run_id,
        "run_attempt": attempt, "rustc": "rustc 1.0", "cargo": "cargo 1.0",
        "features": ["ui"], "target": target,
        "runner": {"x86_64-linux": "ubuntu-22.04",
                   "aarch64-linux": "ubuntu-22.04-arm",
                   "aarch64-macos": "macos-14"}[target],
        "checks": ["fmt", "clippy", "test", "build", "ui"],
        "sha256": binary_sha, "built_at": "2026-01-01T00:00:00Z",
    }
    manifest.update(manifest_over or {})
    files = members if members is not None else {
        "cadence": binary,
        "cadence.sha256": f"{binary_sha}  cadence\n".encode(),
        "manifest.json": json.dumps(manifest).encode(),
    }
    tar_path = Path(tmp) / f"cadence-{tag}-{target}.tar.gz"
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        for name, data in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    tar_path.write_bytes(buf.getvalue())
    sha_path = Path(str(tar_path) + ".sha256")
    sha_path.write_text(
        f"{hashlib.sha256(buf.getvalue()).hexdigest()}  {tar_path.name}\n")
    return tar_path, sha_path, binary_sha


class CleanInstallTests(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("clean_install", SCRIPT)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)
        self.sha = "a" * 40
        self.tag = "v0.1.0-beta.1"
        self.run = {
            "id": 36331102399, "run_attempt": 1,
            "path": ".github/workflows/ci.yml", "head_sha": self.sha,
            "head_branch": self.tag, "event": "push",
            "status": "in_progress", "conclusion": None,
            "repository": {"full_name": "favcrm/cadence"},
            "head_repository": {"full_name": "favcrm/cadence"},
        }
        self.jobs = [
            {"name": n, "status": "completed", "conclusion": "success"}
            for n in ["fmt", "clippy", "test", "build", "ui", "release-gate",
                      "release-build (x86_64-linux, ubuntu-22.04)",
                      "release-build (aarch64-linux, ubuntu-22.04-arm)",
                      "release-build (aarch64-macos, macos-14)"]
        ]

    # --- inputs ------------------------------------------------------------

    def test_strict_input_scalars(self):
        self.module.valid_inputs("favcrm/cadence", self.tag, self.sha,
                                 "x86_64-linux")
        for repo, tag, sha, target in [
            ("fork/cadence!", self.tag, self.sha, "x86_64-linux"),
            ("favcrm/cadence", "v0.1.0;rm -rf /", self.sha, "x86_64-linux"),
            ("favcrm/cadence", "v0.1.0..x", self.sha, "x86_64-linux"),
            ("favcrm/cadence", "release", self.sha, "x86_64-linux"),
            ("favcrm/cadence", self.tag, "main", "x86_64-linux"),
            ("favcrm/cadence", self.tag, self.sha[:-1] + "g", "x86_64-linux"),
            ("favcrm/cadence", self.tag, self.sha, "x86_64-windows"),
        ]:
            with self.subTest(repo=repo, tag=tag, sha=sha, target=target):
                with self.assertRaises(ValueError):
                    self.module.valid_inputs(repo, tag, sha, target)

    # --- run identity --------------------------------------------------------

    def test_in_progress_exact_tag_run_is_accepted(self):
        self.module.validate_run(self.run, "favcrm/cadence", 36331102399,
                                 self.sha, self.tag, 1)

    def test_foreign_forged_or_wrong_run_is_refused(self):
        bad = [
            {"id": 1},
            {"repository": {"full_name": "fork/cadence"}},
            {"head_repository": {"full_name": "fork/cadence"}},
            {"path": ".github/workflows/staging.yml"},
            {"event": "pull_request"},
            {"event": "workflow_dispatch"},
            {"head_sha": "b" * 40},
            {"head_branch": "main"},
            {"head_branch": "v9.9.9"},
            {"run_attempt": 2},
            {"run_attempt": "1"},
            {"status": "completed", "conclusion": "failure"},
            {"status": "completed", "conclusion": "cancelled"},
        ]
        for patch in bad:
            with self.subTest(patch=patch):
                run = dict(self.run, **patch)
                if "conclusion" not in patch and run.get("status") == "completed":
                    run["conclusion"] = "failure"
                with self.assertRaises(ValueError):
                    self.module.validate_run(run, "favcrm/cadence",
                                             36331102399, self.sha, self.tag, 1)

    def test_completed_success_attempt_matches(self):
        run = dict(self.run, status="completed", conclusion="success")
        self.module.validate_run(run, "favcrm/cadence", 36331102399,
                                 self.sha, self.tag, 1)

    def test_waiting_identity_still_requires_successful_jobs(self):
        self.module.validate_run(dict(self.run, status="waiting"),
                                 "favcrm/cadence", 36331102399,
                                 self.sha, self.tag, 1)
        failed = [dict(j, conclusion="failure") if j["name"] == "ui"
                  else j for j in self.jobs]
        with self.assertRaises(ValueError):
            self.module.validate_jobs(failed)

    def test_canonical_repository_and_strict_types(self):
        for repo in ("foreign/repo", None, [], True):
            with self.subTest(repo=repo), self.assertRaises(ValueError):
                self.module.valid_inputs(repo, self.tag, self.sha,
                                         "x86_64-linux")
        for value in (True, "1", 0, -1):
            with self.subTest(attempt=value), self.assertRaises(ValueError):
                self.module.validate_run(self.run, "favcrm/cadence",
                                         36331102399, self.sha, self.tag, value)

    def test_environment_is_an_allowlist(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory(dir="/tmp") as tmp:
            path, _ = self.module.restricted_path(tmp)
            with patch.dict(os.environ, {"PROVIDER_PRIVATE_KEY": "secret",
                                         "LD_PRELOAD": "/bad",
                                         "BASH_ENV": "/bad",
                                         "HTTPS_PROXY": "https://bad"}):
                env = self.module.clean_env(tmp, path)
            self.assertEqual(set(env), {"HOME", "PATH", "TMPDIR", "LANG",
                                       "LC_ALL", "TZ", "XDG_STATE_HOME",
                                       "XDG_CONFIG_HOME", "XDG_DATA_HOME",
                                       "XDG_CACHE_HOME", "XDG_RUNTIME_DIR"})

    # --- job inventory -------------------------------------------------------

    def test_full_gate_and_matrix_inventory_is_accepted(self):
        self.module.validate_jobs(list(self.jobs))

    def test_missing_failed_duplicate_or_ambiguous_jobs_are_refused(self):
        variants = [
            [j for j in self.jobs if j["name"] != "clippy"],
            [j for j in self.jobs if j["name"] != "release-gate"],
            self.jobs + [{"name": "ui", "status": "completed",
                          "conclusion": "success"}],
            [dict(j, conclusion="failure") if j["name"] == "test" else j
             for j in self.jobs],
            [dict(j, status="in_progress") if j["name"] == "build" else j
             for j in self.jobs],
            [j for j in self.jobs
             if not j["name"].startswith("release-build (aarch64-macos")],
            self.jobs + [{"name": "release-build (x86_64-linux, ubuntu-22.04)",
                          "status": "completed", "conclusion": "success"}],
            [dict(j, name="release-build (x86_64-linux, ubuntu-24.04)")
             if j["name"].startswith("release-build (x86_64") else j
             for j in self.jobs],
            [],
            [{"name": "release-gate"}],
        ]
        for jobs in variants:
            with self.subTest(count=len(jobs)):
                with self.assertRaises(ValueError):
                    self.module.validate_jobs(jobs)

    # --- artifact inventory ----------------------------------------------------

    def test_exactly_one_live_artifact(self):
        inv = [{"name": "release-v0.1.0-beta.1-x86_64-linux", "id": 5,
                "expired": False}]
        self.assertEqual(
            self.module.find_artifact(inv, "release-v0.1.0-beta.1-x86_64-linux")["id"], 5)
        for inventory in [
            [],
            [{"name": "release-v0.1.0-beta.1-x86_64-linux", "expired": True}],
            [{"name": "release-v0.1.0-beta.1-x86_64-linux", "expired": False},
             {"name": "release-v0.1.0-beta.1-x86_64-linux", "expired": False}],
            [{"name": "release-v0.1.0-beta.1-aarch64-linux", "expired": False}],
        ]:
            with self.subTest(inventory=inventory):
                with self.assertRaises(ValueError):
                    self.module.find_artifact(
                        inventory, "release-v0.1.0-beta.1-x86_64-linux")

    # --- tarball verification ----------------------------------------------------

    def test_authentic_tarball_verifies(self):
        with tempfile.TemporaryDirectory() as tmp:
            tar_path, sha_path, binary_sha = make_tarball(
                tmp, self.tag, self.sha, "x86_64-linux")
            result = self.module.verify_tarball(
                tar_path, sha_path, self.tag, "x86_64-linux", self.sha, 7, 1)
            self.assertEqual(result["binary_sha256"], binary_sha)

    def test_tarball_forgery_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            cases = {}
            # Outer checksum mismatch: rewrite the .sha256 to another digest.
            tar_path, sha_path, _ = make_tarball(tmp, self.tag, self.sha,
                                                 "x86_64-linux")
            sha_path.write_text(f"{'0' * 64}  x\n")
            cases["outer-mismatch"] = (tar_path, sha_path)
            for label, over in [
                ("wrong-tag", {"version": "v9.9.9"}),
                ("wrong-source", {"source_sha": "b" * 40}),
                ("wrong-target", {"target": "aarch64-linux"}),
                ("wrong-run", {"run_id": 8}),
                ("wrong-attempt", {"run_attempt": 2}),
                ("no-ui", {"features": []}),
                ("wrong-checks", {"checks": ["fmt"]}),
            ]:
                d = Path(tmp) / label
                d.mkdir()
                p, s, _ = make_tarball(d, self.tag, self.sha, "x86_64-linux",
                                       manifest_over=over)
                cases[label] = (p, s)
            for label, (p, s) in cases.items():
                with self.subTest(label=label):
                    with self.assertRaises((ValueError, json.JSONDecodeError)):
                        self.module.verify_tarball(
                            p, s, self.tag, "x86_64-linux", self.sha, 7, 1)

    def test_tarball_extra_traversal_and_inner_mismatch_are_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            binary = b"fake-cadence"
            binary_sha = hashlib.sha256(binary).hexdigest()
            manifest = json.dumps({"source_sha": self.sha, "version": self.tag,
                                   "run_id": 7, "run_attempt": 1,
                                   "rustc": "r", "cargo": "c",
                                   "features": ["ui"], "target": "x86_64-linux",
                                   "runner": "ubuntu-22.04",
                                   "checks": ["fmt", "clippy", "test", "build", "ui"],
                                   "sha256": binary_sha,
                                   "built_at": "t"}).encode()
            bad_members = {
                "extra": {"cadence": binary,
                          "cadence.sha256": f"{binary_sha}  cadence\n".encode(),
                          "manifest.json": manifest, "evil.sh": b"rm -rf /"},
                "inner-mismatch": {"cadence": binary,
                                   "cadence.sha256": b"0" * 64 + b"  cadence\n",
                                   "manifest.json": manifest},
            }
            for label, members in bad_members.items():
                d = Path(tmp) / label
                d.mkdir()
                p, s, _ = make_tarball(d, self.tag, self.sha, "x86_64-linux",
                                       members=members)
                with self.subTest(label=label):
                    with self.assertRaises(ValueError):
                        self.module.verify_tarball(
                            p, s, self.tag, "x86_64-linux", self.sha, 7, 1)

    # --- restricted PATH / clean env ----------------------------------------------

    def test_restricted_path_excludes_toolchain(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, linked = self.module.restricted_path(tmp)
            for tool in self.module.FORBIDDEN_ON_PATH:
                self.assertNotIn(tool, os.listdir(Path(tmp) / "bin"))
            env = self.module.clean_env(tmp, path)
            self.assertEqual(env["PATH"], path)
            self.assertTrue(env["HOME"].startswith(tmp))
            for key in env:
                self.assertFalse(key.startswith("CADENCE_"), key)
                self.assertFalse(key.startswith("GITHUB_"), key)
                self.assertNotIn("TOKEN", key)

    def test_clean_env_scrubs_inherited_tokens(self):
        old = dict(os.environ)
        try:
            os.environ.update({"CADENCE_ALIAS": "x", "GH_TOKEN": "t",
                               "GITHUB_TOKEN": "t", "TMUX": "x",
                               "MY_SECRET": "s"})
            with tempfile.TemporaryDirectory() as tmp:
                path, _ = self.module.restricted_path(tmp)
                env = self.module.clean_env(tmp, path)
            for key in ("CADENCE_ALIAS", "GH_TOKEN", "GITHUB_TOKEN",
                        "TMUX", "MY_SECRET"):
                self.assertNotIn(key, env)
        finally:
            os.environ.clear()
            os.environ.update(old)

    def test_manifest_boolean_run_identity_is_not_an_integer(self):
        with tempfile.TemporaryDirectory() as tmp:
            p, s, _ = make_tarball(tmp, self.tag, self.sha, "x86_64-linux",
                                   run_id=1, manifest_over={"run_id": True})
            with self.assertRaises(ValueError):
                self.module.verify_tarball(p, s, self.tag, "x86_64-linux",
                                           self.sha, 1, 1)

    def candidate_fixture(self, root, target="x86_64-linux"):
        """Handwritten shell only: never a product/native binary."""
        binary = (
            "#!/bin/sh\n"
            '[ "$1" = "--version" ] || exit 90\n'
            '[ -z "$PROVIDER_PRIVATE_KEY" ] || exit 91\n'
            '[ -z "$GH_TOKEN" ] || exit 92\n'
            'for tool in cargo rustc rustup node npm pnpm gh; do\n'
            '  if command -v "$tool" >/dev/null 2>&1; then exit 93; fi\n'
            'done\n'
            f"printf '%s\\n' 'cadence {self.tag[1:]}+{self.sha}'\n"
        ).encode()
        p, s, binary_sha = make_tarball(root, self.tag, self.sha, target, binary=binary)
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, "w") as z:
            z.writestr(p.name, p.read_bytes())
            z.writestr(s.name, s.read_bytes())
        run = dict(self.run, id=7, status="waiting")
        jobs = [dict(j, id=index + 1) for index, j in enumerate(self.jobs)]
        artifact = {"id": 55, "name": f"release-{self.tag}-{target}", "expired": False}
        def api(endpoint):
            if endpoint.endswith("/actions/runs/7"):
                return run
            if "/git/ref/tags/" in endpoint:
                return {"ref": f"refs/tags/{self.tag}",
                        "object": {"type": "commit", "sha": self.sha}}
            if "/compare/" in endpoint:
                return {"status": "ahead"}
            if "/attempts/1/jobs?" in endpoint:
                return {"total_count": len(jobs), "jobs": jobs}
            if "/artifacts?" in endpoint:
                return {"total_count": 1, "artifacts": [artifact]}
            raise AssertionError(endpoint)
        def gh(*args, **kwargs):
            if any("/actions/artifacts/55/zip" in str(a) for a in args):
                return archive.getvalue()
            if any("/contents/scripts/install.sh?" in str(a) for a in args):
                return (SCRIPT.parent / "install.sh").read_bytes()
            raise AssertionError(args)
        return api, gh, binary_sha, artifact

    def fixture_harness(self, root, target="x86_64-linux"):
        api, gh, binary_sha, _ = self.candidate_fixture(root, target)
        package = root / "package"
        with patch.object(self.module, "api", api), patch.object(self.module, "gh", gh):
            receipt = self.module.acquire_candidate(self.tag, self.sha, target, 7, 1, package)
        self.assertFalse(receipt["public_url_proven"])
        asset = f"cadence-{self.tag}-{target}.tar.gz"
        mirror = root / "mirror" / self.tag
        mirror.mkdir(parents=True)
        for name in (asset, asset + ".sha256"):
            self.assertTrue((package / name).is_file())
            (mirror / name).write_bytes((package / name).read_bytes())
        with patch.dict(os.environ, {"PROVIDER_PRIVATE_KEY": "fixture-secret",
                                     "GH_TOKEN": "fixture-token", "LD_PRELOAD": "/bad",
                                     "BASH_ENV": "/bad"}):
            result = self.module.harness(
                package / "install.sh", self.tag, self.sha, target,
                binary_sha, "candidate", expected_manifest=package / "manifest.json", base_url=(root / "mirror").as_uri(),
                root=root, evidence_path=root / "evidence.json")
        self.assertTrue(result["ok"], result)
        self.assertFalse(result["full_cad317_acceptance"])
        self.assertEqual(set(result["installs"]), {"default", "custom-prefix"})
        for installation in result["installs"].values():
            expected = installation["state"]["link_target"]
            self.assertTrue(expected.endswith(f"/releases/{self.tag}/cadence"))
            self.assertEqual(installation["state"]["link"], expected)
        for step in result["steps"]:
            self.assertEqual(step["exit"], 0)
            self.assertTrue({"argv", "stdout", "stderr"}.issubset(step))
        return result

    def test_waiting_acquisition_and_actual_installer_fixture(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            self.fixture_harness(Path(temporary))

    def test_simulated_macos_is_fixture_only_with_pending_daemon(self):
        original = self.module.restricted_path
        def simulated_uname(directory):
            path, tools = original(directory)
            uname = Path(path) / "uname"
            uname.unlink()
            uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; *) exit 94;; esac\n')
            uname.chmod(0o755)
            return path, tools
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary, \
                patch.object(self.module, "restricted_path", simulated_uname):
            result = self.fixture_harness(Path(temporary), "aarch64-macos")
            self.assertIn("CAD-315", result["pending"]["macos-daemon"])

    def test_changed_run_and_duplicate_artifact_do_not_wait(self):
        for scenario in ("failed-run", "duplicate-artifact"):
            with self.subTest(scenario=scenario), tempfile.TemporaryDirectory(dir="/tmp") as temporary:
                root = Path(temporary)
                api, gh, _, artifact = self.candidate_fixture(root)
                calls = 0
                def changed(endpoint):
                    nonlocal calls
                    value = api(endpoint)
                    if endpoint.endswith("/actions/runs/7"):
                        calls += 1
                        if scenario == "failed-run" and calls > 1:
                            value = dict(value, status="completed", conclusion="failure")
                    if scenario == "duplicate-artifact" and "/artifacts?" in endpoint:
                        value = {"total_count": 2, "artifacts": [artifact, dict(artifact, id=56)]}
                    return value
                with patch.object(self.module, "api", changed), patch.object(self.module, "gh", gh), \
                        patch.object(self.module.time, "sleep") as sleep:
                    with self.assertRaises(ValueError):
                        self.module.acquire_candidate(self.tag, self.sha, "x86_64-linux", 7, 1, root / "out")
                    sleep.assert_not_called()

    def test_actual_installer_wrong_link_and_rerun_changes_fail(self):
        for scenario in ("wrong-link", "rerun-link", "rerun-mode"):
            with self.subTest(scenario=scenario), tempfile.TemporaryDirectory(dir="/tmp") as temporary:
                root = Path(temporary)
                api, gh, digest, _ = self.candidate_fixture(root)
                package = root / "package"
                with patch.object(self.module, "api", api), patch.object(self.module, "gh", gh):
                    self.module.acquire_candidate(self.tag, self.sha, "x86_64-linux", 7, 1, package)
                mirror = root / "mirror" / self.tag
                mirror.mkdir(parents=True)
                asset = f"cadence-{self.tag}-x86_64-linux.tar.gz"
                for name in (asset, asset + ".sha256"):
                    (mirror / name).write_bytes((package / name).read_bytes())
                original = self.module.run
                installs = 0
                def altered(argv, env=None, cwd=None, timeout=120):
                    nonlocal installs
                    record = original(argv, env, cwd, timeout)
                    if argv[0] == "sh":
                        installs += 1
                        link = Path(env["HOME"]) / ".local/bin/cadence"
                        binary = Path(env["XDG_DATA_HOME"]) / "cadence/releases" / self.tag / "cadence"
                        if scenario == "wrong-link" and installs == 1:
                            wrong = Path(env["HOME"]) / "same-bytes-wrong-target"
                            wrong.write_bytes(binary.read_bytes())
                            wrong.chmod(0o755)
                            link.unlink()
                            link.symlink_to(wrong)
                        elif scenario == "rerun-link" and installs == 2:
                            target = os.readlink(link)
                            old = link.lstat().st_ino
                            link.unlink()
                            link.symlink_to(target)
                            # Ensure a distinct inode even on aggressively reused filesystems.
                            if link.lstat().st_ino == old:
                                os.utime(link, ns=(0, 0), follow_symlinks=False)
                        elif scenario == "rerun-mode" and installs == 2:
                            binary.chmod(0o700)
                    return record
                with patch.object(self.module, "run", altered):
                    result = self.module.harness(
                        package / "install.sh", self.tag, self.sha, "x86_64-linux",
                        digest, "candidate", expected_manifest=package / "manifest.json", base_url=(root / "mirror").as_uri(), root=root)
                self.assertFalse(result["ok"], result)
                self.assertEqual(installs, 1 if scenario == "wrong-link" else 2)
                self.assertNotIn("custom-prefix", result["installs"])
                self.assertTrue(all("exit" in s and "stdout" in s for s in result["steps"]))

    def test_duplicate_archives_and_incomplete_inventories_fail_closed(self):
        archive = io.BytesIO()
        with tarfile.open(fileobj=archive, mode="w:gz") as tar:
            for name in ("cadence", "cadence", "cadence.sha256", "manifest.json"):
                entry = tarfile.TarInfo(name)
                entry.size = 1
                tar.addfile(entry, io.BytesIO(b"x"))
        with self.assertRaises(ValueError):
            self.module.tar_members(archive.getvalue(), self.module.TAR_MEMBERS)
        for data in ({"total_count": 2, "jobs": [{"id": 1}]},
                     {"total_count": 2, "jobs": [{"id": 1}, {"id": 1}]},
                     {"total_count": True, "jobs": []}):
            with self.subTest(data=data), patch.object(self.module, "api", return_value=data):
                with self.assertRaises(ValueError):
                    self.module.paged("repos/favcrm/cadence/jobs", "jobs")

    def test_installed_manifest_must_equal_verified_package_on_each_prefix(self):
        for forged_prefix in ("default", "custom"):
            with self.subTest(prefix=forged_prefix), tempfile.TemporaryDirectory(dir="/tmp") as temporary:
                root = Path(temporary)
                api, gh, digest, _ = self.candidate_fixture(root)
                package = root / "package"
                with patch.object(self.module, "api", api), patch.object(self.module, "gh", gh):
                    self.module.acquire_candidate(self.tag, self.sha, "x86_64-linux", 7, 1, package)
                mirror = root / "mirror" / self.tag
                mirror.mkdir(parents=True)
                asset = f"cadence-{self.tag}-x86_64-linux.tar.gz"
                for name in (asset, asset + ".sha256"):
                    (mirror / name).write_bytes((package / name).read_bytes())
                original = self.module.run
                seen = set()
                def forged(argv, env=None, cwd=None, timeout=120):
                    record = original(argv, env, cwd, timeout)
                    if argv[0] == "sh":
                        custom = "--prefix" in argv
                        prefix = (Path(argv[argv.index("--prefix") + 1]) if custom
                                  else Path(env["XDG_DATA_HOME"]) / "cadence")
                        if prefix not in seen and custom == (forged_prefix == "custom"):
                            seen.add(prefix)
                            path = prefix / "releases" / self.tag / "manifest.json"
                            manifest = json.loads(path.read_text())
                            manifest.update(run_id=999, run_attempt=999,
                                            rustc="forged toolchain", built_at="forged timestamp")
                            path.write_text(json.dumps(manifest))
                    return record
                with patch.object(self.module, "run", forged):
                    result = self.module.harness(
                        package / "install.sh", self.tag, self.sha, "x86_64-linux",
                        digest, "candidate", expected_manifest=package / "manifest.json", base_url=(root / "mirror").as_uri(), root=root)
                self.assertFalse(result["ok"], result)
                self.assertIn("manifest", result["failure"]["detail"])

    def test_public_mode_cannot_use_candidate_mirror(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            with self.assertRaises(ValueError), patch.object(self.module, "run") as run:
                self.module.harness("unused", self.tag, self.sha, "x86_64-linux",
                                    "a" * 64, "published", expected_manifest="unused", base_url=Path(temporary).as_uri())
            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
