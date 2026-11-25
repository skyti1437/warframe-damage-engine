"""验收纯 Rust 运行包的确定性与隔离安装。"""

import errno
import hashlib
from itertools import islice
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
import zipfile

from tests.test_phase2a_server import BUILD, FINGERPRINT, GOLDEN, HOST, ROOT, SERVER, free_port, ready_server
from tools.package_rust_bundle import native_platform
from tools.phase0_benchmark_and_record import differences, request_json, stable_response


PACKAGER = ROOT / "tools/package_rust_bundle.py"
LEGACY_BUNDLE = ROOT / "downloads/portable-runtime-mac-arm64.zip"
INCARNON_DEFAULTS = f"data/export/{BUILD}/incarnon_defaults.json"
PAYLOAD = {"LICENSE", "THIRD_PARTY_NOTICES.md", INCARNON_DEFAULTS, "wfcore-server"}
ZIP_NAMES = sorted(PAYLOAD | {"manifest.json"})
HEALTH = {"ok": True, "buildLabel": BUILD, "engineFingerprint": FINGERPRINT, "schemaVersion": 3}
GOLDEN_IDS = {
    "health", "catalog", "simulate-braton-standard", "build-burston-auto-tie",
    "analyze-burston-riven", "matrix-burston-31",
}
ENDPOINTS = {f"/api/{name}" for name in ("health", "catalog", "simulate", "build", "analyze", "trait-matrix")}
TIMEOUT = 300


def sha256_file(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


class Phase4aPackagingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.platform = native_platform()
        if cls.platform not in {"darwin-arm64", "linux-x86_64", "linux-arm64"}:
            raise unittest.SkipTest(f"无独立运行包目标 {cls.platform}")

        work = Path(cls.enterClassContext(tempfile.TemporaryDirectory(prefix="phase4a-")))
        cls.bundle = work / f"portable-rust-{cls.platform}.zip"
        cls.metadata = work / "metadata.json"
        cls.binary = Path(os.environ.get("WFCORE_SERVER_BINARY", SERVER)).resolve()
        cls.package(cls.bundle, "--metadata-out", str(cls.metadata), "--verify")
        with zipfile.ZipFile(LEGACY_BUNDLE) as archive:
            cls.legacy = json.loads(archive.read("manifest.json"))
        cls.upstream = sorted(set(cls.legacy["upstreamFiles"]) - {
            "data/arena/score-identity-baseline.json.gz", "data/sources/wiki/stance_data_2026-08-23.lua",
        })

    @classmethod
    def package(cls, output, *flags):
        result = subprocess.run(
            [sys.executable, str(PACKAGER), "--binary", str(cls.binary), "--output", str(output), *flags],
            cwd=cls.bundle.parent, capture_output=True, text=True, timeout=TIMEOUT,
        )
        if result.returncode:
            raise AssertionError(f"运行包打包失败（{result.returncode}）\n{result.stderr}")

    def test_deterministic_zip_and_metadata(self):
        second = self.bundle.parent / "repeat" / self.bundle.name
        self.package(second, "--platform", self.platform)
        size = self.bundle.stat().st_size
        digest = sha256_file(self.bundle)
        self.assertLess(size, 10_000_000)
        self.assertEqual(sha256_file(second), digest)

        identity = {
            "runtime": "rust-standalone", "binary": "wfcore-server", "platform": self.platform,
            "fingerprint": FINGERPRINT, "runtimeDigest": "3ea749df5c3de728", "dataVersion": BUILD,
            "upstream": "d62e4ae56387e241d512dcf9176f3bfd3d426de7",
        }
        metadata = json.loads(self.metadata.read_bytes())
        self.assertEqual(metadata, dict(identity, name=self.bundle.name, sha256=digest, size=size))
        with zipfile.ZipFile(self.bundle) as archive:
            self.assertEqual(archive.namelist(), ZIP_NAMES)
            manifest = json.loads(archive.read("manifest.json"))
            self.assertEqual(set(manifest), identity.keys() | {"upstreamFiles", "files"})
            self.assertEqual({key: manifest[key] for key in identity}, identity)
            self.assertNotIn("python", manifest)
            self.assertEqual(len(manifest["upstreamFiles"]), 42)
            self.assertEqual(manifest["upstreamFiles"], self.upstream)
            self.assertEqual(len(manifest["files"]), 46)
            self.assertEqual(set(manifest["files"]), PAYLOAD | set(self.upstream))

            for entry in archive.infolist():
                name = entry.filename
                mode = 0o755 if name == "wfcore-server" else 0o644
                self.assertEqual(entry.date_time, (2026, 8, 13, 0, 0, 0), name)
                self.assertEqual(entry.compress_type, zipfile.ZIP_DEFLATED, name)
                self.assertEqual(entry.create_system, 3, name)
                self.assertEqual(entry.external_attr >> 16, stat.S_IFREG | mode, name)
                if name in PAYLOAD:
                    self.assertEqual(hashlib.sha256(archive.read(entry)).hexdigest(), manifest["files"][name], name)
                    source = self.binary if name == "wfcore-server" else ROOT / name
                    self.assertEqual(sha256_file(source), manifest["files"][name], name)
            for name in [*self.upstream, INCARNON_DEFAULTS]:
                self.assertEqual(sha256_file(ROOT / name), self.legacy["files"][name], name)
                self.assertEqual(manifest["files"][name], self.legacy["files"][name], name)
        print(f"{self.bundle.name} {size} bytes，SHA-256 {digest}", flush=True)

    def install(self, directory):
        with zipfile.ZipFile(self.bundle) as archive:
            self.assertEqual(archive.namelist(), ZIP_NAMES)
            manifest = json.loads(archive.read("manifest.json"))
            self.assertEqual(manifest["upstreamFiles"], self.upstream)
            for entry in archive.infolist():
                name = entry.filename
                destination = directory / PurePosixPath(name)
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(archive.read(entry))
                if name != "manifest.json":
                    self.assertEqual(sha256_file(destination), manifest["files"][name], name)

        for name in manifest["upstreamFiles"]:
            path = PurePosixPath(name)
            self.assertTrue(name.startswith("data/") and path.as_posix() == name and ".." not in path.parts)
            self.assertNotIn("\\", name)
            destination = directory / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / path, destination)
            self.assertEqual(sha256_file(destination), manifest["files"][name], name)

        (directory / "wfcore-server").chmod(0o755)
        installed = {path.relative_to(directory).as_posix() for path in directory.rglob("*") if path.is_file()}
        self.assertEqual(installed, set(ZIP_NAMES) | set(self.upstream))
        self.assertFalse(list(directory.rglob("*.py")))
        self.assertFalse((directory / "vendor").exists())

    def probe(self, directory):
        return json.loads(subprocess.check_output(["./wfcore-server", "--probe"], cwd=directory, timeout=TIMEOUT))

    def requests(self, directory, cases):
        try:
            port = free_port()
        except OSError as error:
            if error.errno not in (errno.EACCES, errno.EPERM, errno.EAFNOSUPPORT):
                raise
            print("沙盒禁止监听 TCP，隔离安装改用 --request-batch。", flush=True)
        else:
            print("隔离安装通过 TCP HTTP 验收。", flush=True)
            with ready_server(["./wfcore-server"], port, cwd=directory):
                url = f"http://{HOST}:{port}"
                results = []
                for case in cases:
                    status, response = request_json(url, case)
                    results.append({"id": case["id"], "status": status, "response": response})
                return results

        requests_path = self.bundle.parent / "requests.json"
        requests_path.write_text(json.dumps(cases, ensure_ascii=False, allow_nan=False), encoding="utf-8")
        output = subprocess.check_output(
            ["./wfcore-server", "--request-batch", str(requests_path)], cwd=directory, timeout=TIMEOUT,
        )
        return json.loads(output)

    def test_isolated_probe_and_all_six_endpoints(self):
        with tempfile.TemporaryDirectory(prefix="phase4a-install-") as directory:
            directory = Path(directory)
            self.install(directory)
            self.assertFalse(list(differences(HEALTH, self.probe(directory))))

            golden = json.loads(GOLDEN.read_bytes())["cases"]
            selected = [entry for entry in golden if entry["id"] in GOLDEN_IDS]
            self.assertEqual({entry["id"] for entry in selected}, GOLDEN_IDS)
            self.assertEqual({entry["path"] for entry in selected}, ENDPOINTS)
            cases = [{key: value for key, value in case.items() if key != "expected"} for case in selected]
            results = self.requests(directory, cases)
            self.assertEqual([row["id"] for row in results], [entry["id"] for entry in selected])
            for entry, result in zip(selected, results, strict=True):
                with self.subTest(case=entry["id"]):
                    actual = {"status": result["status"], "response": stable_response(entry["path"], result["response"])}
                    mismatches = list(islice(differences(entry["expected"], actual), 24))
                    self.assertFalse(mismatches, "\n".join(mismatches))

            # 改动隔离目录中 source-lock 的一个结尾空白字节，JSON 内容保持等价。
            source_lock = directory / "data/source-lock.json"
            original = source_lock.read_bytes()
            self.assertTrue(original.endswith(b"\n"))
            source_lock.write_bytes(original[:-1] + b" ")
            changed = self.probe(directory)
            new_digest = changed["engineFingerprint"]
            self.assertRegex(new_digest, r"\A[0-9a-f]{16}\Z")
            self.assertNotEqual(new_digest, FINGERPRINT)
            self.assertNotEqual(new_digest, "3ea749df5c3de728")
            self.assertFalse(list(differences(dict(HEALTH, engineFingerprint=new_digest), changed)))


if __name__ == "__main__":
    unittest.main()
