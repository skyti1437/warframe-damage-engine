"""打包并校验纯 Rust 独立运行包。"""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import stat
import subprocess
import tempfile
import zipfile


ROOT = Path(__file__).resolve().parents[1]
PLATFORMS = ("darwin-arm64", "linux-x86_64", "linux-arm64")
BINARY = "wfcore-server"
RUNTIME = "rust-standalone"
FINGERPRINT = "535ed49f5a51149d"
RUNTIME_DIGEST = "3ea749df5c3de728"
DATA_VERSION = "2026.08.13.13.24"
UPSTREAM = "d62e4ae56387e241d512dcf9176f3bfd3d426de7"
LEGACY_BUNDLE = ROOT / "downloads" / "portable-runtime-mac-arm64.zip"
INCARNON_DEFAULTS = f"data/export/{DATA_VERSION}/incarnon_defaults.json"
LOCAL_FILES = ("LICENSE", "THIRD_PARTY_NOTICES.md", INCARNON_DEFAULTS, BINARY)
MANIFEST = "manifest.json"
ZIP_TIMESTAMP = (2026, 8, 13, 0, 0, 0)
EXCLUDED_UPSTREAM = {
    "data/arena/score-identity-baseline.json.gz",
    "data/sources/wiki/stance_data_2026-08-23.lua",
}


def native_platform():
    machine = platform.machine().lower()
    arch = {"aarch64": "arm64", "amd64": "x86_64"}.get(machine, machine)
    return f"{platform.system().lower()}-{arch}"


def safe_relative(name):
    path = PurePosixPath(name)
    if (not path.parts or path.is_absolute() or ".." in path.parts or path.as_posix() != name
            or "\\" in name or ":" in name or any(ord(char) < 32 for char in name)):
        raise ValueError(f"无效的运行包路径：{name!r}")
    return path


def frozen_data_hashes():
    """从已发布 ZIP 取出 42 个上游输入及随包的灵化默认路线摘要。"""
    with zipfile.ZipFile(LEGACY_BUNDLE) as archive:
        legacy = json.loads(archive.read(MANIFEST))
    identity = (legacy["upstream"], legacy["dataVersion"], legacy["fingerprint"])
    if identity != (UPSTREAM, DATA_VERSION, FINGERPRINT):
        raise ValueError("旧运行包的冻结身份不符")

    names = legacy["upstreamFiles"]
    if len(names) != 44 or len(set(names)) != 44 or not EXCLUDED_UPSTREAM <= set(names):
        raise ValueError("旧运行包的上游清单不符")

    hashes = {}
    for name in sorted((set(names) - EXCLUDED_UPSTREAM) | {INCARNON_DEFAULTS}):
        safe_relative(name)
        digest = legacy["files"][name]
        if not name.startswith("data/") or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError(f"无效的冻结数据条目：{name!r}")
        hashes[name] = digest
    if len(hashes) != 43:
        raise ValueError("独立服务需要 42 个上游文件和随包的灵化默认路线")
    return hashes


def file_sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while block := stream.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def require_sha256(name, actual, expected):
    if actual != expected:
        raise ValueError(f"{name} 的 SHA-256 不符，期望 {expected}，实际 {actual}")


def json_bytes(document):
    return (json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode("utf-8")


def file_mode(name):
    return 0o755 if name == BINARY else 0o644


def write_bundle(output, binary, target):
    frozen = frozen_data_hashes()
    upstream_files = sorted(frozen.keys() - {INCARNON_DEFAULTS})
    files = {}
    for name in upstream_files:
        require_sha256(name, file_sha256(ROOT / name), frozen[name])
        files[name] = frozen[name]

    payload = {name: (binary if name == BINARY else ROOT / name).read_bytes() for name in LOCAL_FILES}
    for name, raw in payload.items():
        safe_relative(name)
        files[name] = hashlib.sha256(raw).hexdigest()
    require_sha256(INCARNON_DEFAULTS, files[INCARNON_DEFAULTS], frozen[INCARNON_DEFAULTS])
    manifest = {
        "runtime": RUNTIME,
        "binary": BINARY,
        "platform": target,
        "fingerprint": FINGERPRINT,
        "runtimeDigest": RUNTIME_DIGEST,
        "dataVersion": DATA_VERSION,
        "upstream": UPSTREAM,
        "upstreamFiles": upstream_files,
        "files": files,
    }
    payload[MANIFEST] = json_bytes(manifest)
    with zipfile.ZipFile(output, "w") as archive:
        for name, raw in sorted(payload.items()):
            entry = zipfile.ZipInfo(name, ZIP_TIMESTAMP)
            entry.create_system = 3
            entry.external_attr = (stat.S_IFREG | file_mode(name)) << 16
            archive.writestr(entry, raw, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
    return manifest


def verify_bundle(output, expected_manifest):
    with tempfile.TemporaryDirectory(prefix="wfcore-verify-") as directory:
        root = Path(directory)
        with zipfile.ZipFile(output) as archive:
            members = archive.infolist()
            if [member.filename for member in members] != sorted((*LOCAL_FILES, MANIFEST)):
                raise ValueError("运行包的 ZIP 条目不符")
            manifest = json.loads(archive.read(MANIFEST))
            if manifest != expected_manifest:
                raise ValueError("运行包 manifest.json 不符")
            for member in members:
                name = member.filename
                dest = root / safe_relative(name)
                if member.external_attr >> 16 != (stat.S_IFREG | file_mode(name)):
                    raise ValueError(f"运行包的文件类型或权限不符：{name}")
                raw = archive.read(member)
                if name != MANIFEST:
                    require_sha256(name, hashlib.sha256(raw).hexdigest(), manifest["files"][name])
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(raw)
                dest.chmod(file_mode(name))

        for name in manifest["upstreamFiles"]:
            path = safe_relative(name)
            dest = root / path
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / path, dest)
            require_sha256(name, file_sha256(dest), manifest["files"][name])

        probe = subprocess.run([f"./{BINARY}", "--probe"], cwd=root, capture_output=True, text=True, timeout=60)
        if probe.returncode != 0:
            raise ValueError(f"wfcore-server --probe 退出码 {probe.returncode}：{probe.stderr.strip()}")
        health = json.loads(probe.stdout)
        expected = {"ok": True, "buildLabel": DATA_VERSION, "engineFingerprint": FINGERPRINT, "schemaVersion": 3}
        if health != expected or health["ok"] is not True or type(health["schemaVersion"]) is not int:
            raise ValueError(f"wfcore-server --probe 响应不符：{health!r}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=PLATFORMS, help="目标平台，默认按本机推断")
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/release" / BINARY)
    parser.add_argument("--output", type=Path, help="ZIP 输出路径，默认在临时目录创建")
    parser.add_argument("--metadata-out", type=Path, help="可选的产物元数据 JSON 路径")
    parser.add_argument("--verify", action="store_true", help="隔离解包、补齐并校验数据，运行本机 --probe")
    args = parser.parse_args()

    host = native_platform()
    target = args.platform or host
    if target not in PLATFORMS:
        parser.error(f"不支持的本机平台 {host}，请用 --platform 指定目标平台")
    if args.verify and target != host:
        parser.error(f"--verify 仅能在目标平台上执行（目标 {target}，本机 {host}）")

    try:
        output = args.output or Path(tempfile.mkdtemp(prefix="portable-rust-")) / f"portable-rust-{target}.zip"
        destinations = [output, args.metadata_out] if args.metadata_out is not None else [output]
        resolved = {path.resolve() for path in destinations}
        if len(resolved) != len(destinations) or args.binary.resolve() in resolved:
            raise ValueError("ZIP、元数据和输入二进制必须使用不同路径")

        output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".package-rust-", dir=output.parent) as staging:
            staged = Path(staging) / output.name
            manifest = write_bundle(staged, args.binary, target)
            if args.verify:
                verify_bundle(staged, manifest)
            metadata = {key: value for key, value in manifest.items() if key not in {"files", "upstreamFiles"}}
            metadata.update(name=output.name, sha256=file_sha256(staged), size=staged.stat().st_size)
            staged.replace(output)

        if args.metadata_out is not None:
            args.metadata_out.parent.mkdir(parents=True, exist_ok=True)
            args.metadata_out.write_bytes(json_bytes(metadata))
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError, zipfile.BadZipFile) as error:
        parser.exit(1, f"打包失败：{error}\n")

    print(f"{output.resolve()} ({metadata['size']} bytes, SHA-256 {metadata['sha256']})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
