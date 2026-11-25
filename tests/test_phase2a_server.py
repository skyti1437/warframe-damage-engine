"""验收独立 Rust 服务、Phase 0 黄金摘要和完整目录。"""

from contextlib import contextmanager
import errno
import http.client
from itertools import islice
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest

from tools.phase0_benchmark_and_record import catalog_summary, differences


ROOT = Path(__file__).resolve().parents[1]
CRATE = ROOT / "rust" / "wfcore-server"
SERVER = ROOT / "rust" / "target" / "release" / ("wfcore-server.exe" if os.name == "nt" else "wfcore-server")
GOLDEN = ROOT / "tests" / "golden" / "phase0_golden.json"
HOST = "127.0.0.1"
BUILD = "2026.08.13.13.24"
CONTENT_TYPE = "application/json; charset=utf-8"
TIMEOUT = 15


def request(port, method, path, *, body=None, length=None):
    # 使用显式头，http.client.request 会为部分 POST 自动填 Content-Length。
    connection = http.client.HTTPConnection(HOST, port, timeout=3)
    try:
        connection.putrequest(method, path)
        connection.putheader("Connection", "close")
        if length is not None:
            connection.putheader("Content-Length", length)
        connection.endheaders(body)
        response = connection.getresponse()
        return response.status, response.headers, json.load(response)
    finally:
        connection.close()


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind((HOST, 0))
        listener.listen()
        return listener.getsockname()[1]


@contextmanager
def running(command, **options):
    with subprocess.Popen(command, cwd=ROOT, **options) as process:
        try:
            yield process
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)


@contextmanager
def ready_server(command, port):
    with tempfile.TemporaryFile() as log:
        start = time.perf_counter()
        with running([*command, "--port", str(port)], stdout=log, stderr=subprocess.STDOUT) as process:
            while process.poll() is None and time.perf_counter() - start < TIMEOUT:
                try:
                    status, headers, health = request(port, "GET", "/api/health")
                except (ConnectionRefusedError, ConnectionResetError):
                    time.sleep(0.01)
                else:
                    if status != 200:
                        raise AssertionError(f"健康检查失败，HTTP {status}：{health}")
                    yield process, time.perf_counter() - start, headers, health
                    return
            log.seek(0)
            raise AssertionError(f"服务未启动（退出码 {process.poll()}）：{log.read().decode(errors='replace')}")


def rss_mib(pid):
    if shutil.which("ps") is None:
        return None
    try:
        output = subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)], stderr=subprocess.DEVNULL, timeout=3)
        return int(output.strip()) / 1024
    except (OSError, ValueError, subprocess.SubprocessError):
        return None


def report_profile(label, elapsed, rss):
    memory = "" if rss is None else f"，RSS {rss:.2f} MiB"
    print(f"{label} {elapsed * 1000:.1f} ms{memory}", flush=True)


def first_exact_difference(expected, actual, path="$"):
    """按 JSON 类型、字段和值精确比较，保留数组顺序。"""
    if type(expected) is not type(actual):
        return f"{path} 类型不同，期望 {type(expected).__name__}，实际 {type(actual).__name__}"
    if isinstance(expected, dict):
        if expected.keys() != actual.keys():
            return f"{path} 字段不同，缺少 {sorted(expected.keys() - actual.keys())}，多出 {sorted(actual.keys() - expected.keys())}"
        for key, value in expected.items():
            if diff := first_exact_difference(value, actual[key], f"{path}.{key}"):
                return diff
    elif isinstance(expected, list):
        if len(expected) != len(actual):
            return f"{path} 长度不同，期望 {len(expected)}，实际 {len(actual)}"
        for index, (left, right) in enumerate(zip(expected, actual)):
            if diff := first_exact_difference(left, right, f"{path}[{index}]"):
                return diff
    elif expected != actual:
        return f"{path} 值不同，期望 {expected!r}，实际 {actual!r}"
    return None


class Phase2aServerTests(unittest.TestCase):
    def test_architecture_has_no_python_dependency(self):
        manifest = (CRATE / "Cargo.toml").read_text(encoding="utf-8")
        self.assertNotIn("pyo3", manifest.lower())
        self.assertEqual(
            set(tomllib.loads(manifest)["dependencies"]),
            {"wfcore-lib", "indexmap", "serde", "serde_json", "tiny_http"},
        )
        link_tool = {"darwin": ["otool", "-L"], "linux": ["ldd"]}.get(sys.platform)
        if not SERVER.is_file() or link_tool is None:
            return
        result = subprocess.run([*link_tool, str(SERVER)], check=True, capture_output=True, text=True, timeout=TIMEOUT)
        self.assertNotRegex(result.stdout.lower(), r"libpython|python\.framework")

    def require_server(self):
        self.assertTrue(SERVER.is_file(), f"请先构建发行版独立服务：{SERVER}")

    def assert_health(self, health):
        self.assertEqual(set(health), {"ok", "buildLabel", "engineFingerprint", "schemaVersion"})
        self.assertIs(health["ok"], True)
        self.assertEqual(health["buildLabel"], BUILD)
        self.assertIs(type(health["schemaVersion"]), int)
        self.assertEqual(health["schemaVersion"], 3)
        self.assertRegex(health["engineFingerprint"], r"\A[0-9a-f]{16}\Z")

    def assert_headers(self, headers, cache):
        self.assertEqual(headers["Content-Type"], CONTENT_TYPE)
        self.assertEqual(headers["Cache-Control"], cache)

    def test_probe(self):
        self.require_server()
        # 从无数据的工作目录运行，验证可执行文件相对路径回退。
        with tempfile.TemporaryDirectory() as directory:
            probe = subprocess.check_output([str(SERVER), "--probe"], cwd=directory, timeout=TIMEOUT)
        self.assert_health(json.loads(probe))
        explicit = subprocess.check_output(
            [str(SERVER), "--data-dir", ".", "--probe"], cwd=ROOT / "data", timeout=TIMEOUT,
        )
        self.assertEqual(json.loads(probe), json.loads(explicit))

    def test_catalog_phase0_and_python_parity(self):
        self.require_server()
        try:
            port = free_port()
        except OSError as error:
            if error.errno not in (errno.EACCES, errno.EPERM, errno.EAFNOSUPPORT):
                raise
            print("沙箱禁止 TCP 监听，改用 --dump-catalog 和 Rust 路由单测。", flush=True)
            catalog = self.catalog_from_cli()
        else:
            with ready_server([str(SERVER)], port) as (process, elapsed, headers, health):
                self.assert_health(health)
                self.assert_headers(headers, "no-store")
                report_profile("Rust HTTP 冷启动", elapsed, rss_mib(process.pid))
                status, headers, catalog = request(port, "GET", "/api/catalog")
                self.assertEqual(status, 200)
                self.assert_headers(headers, "no-cache")
                self.check_http_errors(port)
            python_server = [sys.executable, "-B", str(ROOT / "app.py"), "--no-browser"]
            with ready_server(python_server, free_port()) as (process, elapsed, _, _):
                report_profile("Python HTTP 冷启动", elapsed, rss_mib(process.pid))

        with GOLDEN.open(encoding="utf-8") as stream:
            golden = next(case["expected"] for case in json.load(stream)["cases"] if case["id"] == "catalog")
        mismatches = list(islice(differences(golden["response"], catalog_summary(catalog)), 12))
        self.assertFalse(mismatches, "\n".join(mismatches))

        import app

        # 比较 Python 通过 HTTP 序列化后的形状，例如元组会转为 JSON 数组。
        self.assertIsNone(first_exact_difference(json.loads(json.dumps(app.CATALOG)), catalog))

    def catalog_from_cli(self):
        # 在管道首次可读时取 RSS，完整读完后再比对 JSON。
        start = time.perf_counter()
        with running([str(SERVER), "--dump-catalog"], stdout=subprocess.PIPE, bufsize=0) as process:
            first = process.stdout.read(1)
            report_profile("Rust 目录就绪（CLI）", time.perf_counter() - start, rss_mib(process.pid))
            rest, _ = process.communicate(timeout=TIMEOUT)
            self.assertEqual(process.returncode, 0)
        start = time.perf_counter()
        script = "import app, sys; print('ready', flush=True); sys.stdin.read()"
        with running([sys.executable, "-B", "-c", script], stdin=subprocess.PIPE, stdout=subprocess.PIPE, bufsize=0) as process:
            self.assertEqual(process.stdout.readline(), b"ready\n")
            report_profile("Python 目录就绪（CLI）", time.perf_counter() - start, rss_mib(process.pid))
            process.communicate(timeout=TIMEOUT)
            self.assertEqual(process.returncode, 0)
        return json.loads(first + rest)

    def check_http_errors(self, port):
        cases = [
            ("POST", "/api/simulate", None, None, 400, "invalid_length"),
            ("POST", "/api/simulate", "bad", None, 400, "invalid_length"),
            ("POST", "/api/simulate", "1", b"{", 400, "invalid_json"),
            ("POST", "/api/simulate", "1", b"\xff", 400, "invalid_json"),
            ("POST", "/api/simulate", "2", b"[]", 422, "invalid_request"),
            ("POST", "/api/simulate", "0", None, 413, "request_too_large"),
            ("POST", "/api/simulate", "131073", None, 413, "request_too_large"),
            ("POST", "/api/simulate", "18446744073709551615", None, 413, "request_too_large"),
            ("GET", "/api/missing", None, None, 404, "not_found"),
            ("POST", "/api/missing", None, None, 404, "not_found"),
        ]
        cases.extend(
            ("POST", f"/api/{endpoint}", "2", b"{}", 501, "not_implemented")
            for endpoint in ("analyze", "trait-matrix")
        )
        for method, path, length, body, expected_status, code in cases:
            with self.subTest(method=method, path=path, length=length, body=body):
                status, headers, error = request(port, method, path, length=length, body=body)
                self.assertEqual(status, expected_status)
                self.assert_headers(headers, "no-store")
                self.assertEqual(set(error), {"error"})
                self.assertEqual(set(error["error"]), {"code", "message"})
                self.assertEqual(error["error"]["code"], code)
                self.assertIsInstance(error["error"]["message"], str)
                self.assertTrue(error["error"]["message"])


if __name__ == "__main__":
    unittest.main()
