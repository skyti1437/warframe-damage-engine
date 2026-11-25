"""将独立 Rust 工作区的 Python 扩展构建到 rust/target。"""

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path


WORKSPACE = Path(__file__).resolve().parents[1] / "rust"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    profile = parser.add_mutually_exclusive_group()
    profile.add_argument("--debug", action="store_true", help="构建调试版")
    profile.add_argument("--release", action="store_true", help="构建发行版（默认）")
    parser.add_argument("--offline", action="store_true", help="仅使用本机 Cargo 缓存")
    args = parser.parse_args()

    cargo = shutil.which("cargo")
    if cargo is None:
        fallback = Path.home() / ".cargo" / "bin" / ("cargo.exe" if os.name == "nt" else "cargo")
        if fallback.is_file() and os.access(fallback, os.X_OK):
            cargo = str(fallback)
    if cargo is None:
        parser.exit(1, "找不到 cargo，请检查 PATH 和 ~/.cargo/bin。\n")

    target = WORKSPACE / "target"
    target.mkdir(exist_ok=True)
    # 构建产物停留在工作区的 target 目录，保持发布目录原样。
    (target / ".gitignore").write_text("*\n", encoding="utf-8")
    command = [cargo, "rustc", "-p", "wfcore-py", "--lib", "--locked", "--target-dir", str(target)]
    if not args.debug:
        command.append("--release")
    if args.offline:
        command.append("--offline")
    if sys.platform == "darwin":
        command.extend(["--", "-C", "link-arg=-undefined", "-C", "link-arg=dynamic_lookup"])
    env = dict(os.environ, PYO3_PYTHON=sys.executable)
    env.setdefault("CARGO_BUILD_JOBS", "4")
    build = subprocess.run(command, cwd=WORKSPACE, env=env, stderr=subprocess.STDOUT)
    if build.returncode:
        return build.returncode

    profile_dir = target / ("debug" if args.debug else "release")
    library = {"win32": "wfcore.dll", "darwin": "libwfcore.dylib"}.get(sys.platform, "libwfcore.so")
    output = profile_dir / ("wfcore.pyd" if os.name == "nt" else "wfcore.so")
    temporary = output.with_name(f".{output.name}.{os.getpid()}")
    try:
        shutil.copyfile(profile_dir / library, temporary)
        try:
            temporary.replace(output)
        except PermissionError:
            if os.name != "nt" or not output.exists():
                raise
            # Windows 中，仍处于加载状态的扩展可以先改名。
            output.rename(output.with_name(f"{output.name}.old-{os.getpid()}"))
            temporary.replace(output)
    finally:
        temporary.unlink(missing_ok=True)
    print(f"已构建 {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
