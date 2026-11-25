"""录制、对拍六个公开 HTTP 端点，并测量独立进程中的性能基线。"""

import argparse
import cProfile
import http.client
import json
import math
import platform
import re
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from contextlib import closing, contextmanager
from pathlib import Path
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[1]
GOLDEN = ROOT / "tests" / "golden" / "phase0_golden.json"
FIXTURE_VERSION = 1
HTTP_TIMEOUT = 55
STARTUP_TIMEOUT = 20
WARM_SAMPLES = 3
TOLERANCE = 1e-9

BRATON = "/Lotus/Weapons/Tenno/Rifle/BratonPrime"
BURSTON = "/Lotus/Weapons/Tenno/LongGuns/PrimeBurston/PrimeBurston"
TORID = "/Lotus/Weapons/ClanTech/Bio/BioWeapon"
ZYMOS = "/Lotus/Weapons/Infested/Pistols/InfUzi/InfUziWeapon"
MAGISTAR = "/Lotus/Weapons/Tenno/Melee/Maces/PaladinMace/PaladinMaceWeapon"
GLAIVE = "/Lotus/Weapons/Tenno/Melee/Glaives/PrimeGlaive/PrimeGlaiveWeapon"
THRAX = "/Lotus/Types/Enemies/Duviri/Avatars/DuviriMeleeAvatar"
HEAVY_GUNNER = "/Lotus/Types/Enemies/Grineer/Desert/Avatars/HeavyFemaleGrineerAvatar"
MERCILESS = "/Lotus/Upgrades/CosmeticEnhancers/Offensive/PrimaryDamageOnKill"
DEADHEAD = "/Lotus/Upgrades/CosmeticEnhancers/Offensive/PrimaryDamageOnNoMelee"
EXILUS = "/Lotus/Upgrades/Mods/Sets/Vigilante/PrimaryVigilanteSuppliesMod"
RIFLE_MODS = "/Lotus/Upgrades/Mods/Rifle"
LOADOUT_MODS = (
    (f"{RIFLE_MODS}/WeaponDamageAmountMod", 10),
    (f"{RIFLE_MODS}/WeaponFireIterationsMod", 5),
    (f"{RIFLE_MODS}/WeaponCritChanceMod", 5),
    (f"{RIFLE_MODS}/WeaponCritDamageMod", 5),
    (f"{RIFLE_MODS}/DualStat/PoisonEventRifleMod", 3),
    (f"{RIFLE_MODS}/DualStat/IceEventRifleMod", 3),
    ("/Lotus/Upgrades/Mods/Sets/Hunter/PrimaryHunterMunitionsMod", 5),
)

# 左边是目录摘要的分组名，右边是原响应的数组名和代表条目的路径。
CATALOG_SELECTION = {
    "weapons": ("weapons", {BRATON, BURSTON, TORID, ZYMOS, MAGISTAR, GLAIVE}),
    "mods": ("manualMods", {path for path, _ in LOADOUT_MODS}),
    "arcanes": ("arcanes", {MERCILESS, DEADHEAD}),
    "enemies": ("enemies", {THRAX, HEAVY_GUNNER}),
}

# 只在列出的端点与路径上处理运行耗时、进程缓存诊断及宿主机路径。
VOLATILE_FIELDS = {
    "/api/simulate": (("exportDir",),),
    "/api/build": (
        ("autoLoadout", "seconds"),
        ("autoLoadout", "incarnon", "seconds"),
    ),
    "/api/analyze": (
        ("searchWork", "elapsedSeconds"),
        ("searchStatus", "work", "elapsedSeconds"),
        ("incarnonRouteScreen", "seconds"),
        ("responseCache",),
    ),
}
AUTO_ROUTE_NOTE = re.compile(r"(，\d+ 次配装搜索)，\d+(?:\.\d+)? 秒")


def get_case(name, endpoint):
    return {"id": name, "method": "GET", "path": f"/api/{endpoint}"}


def post_case(name, endpoint, body):
    return {"id": name, "method": "POST", "path": f"/api/{endpoint}", "body": body}


def target(*, level=170, window=30, headshot=0.0, overguard=False, shield=0):
    return {
        "enemy": THRAX, "level": level, "steelPath": True,
        "headshotChance": headshot, "windowSeconds": window,
        "enemyEnhancements": {"noOverguard": not overguard, "shieldOverride": shield},
    }


def melee(route, heavy):
    return {"heavyAttack": heavy, "combatState": {"meleeRoute": route}}


def riven(crit, damage):
    # 固定的两正满级紫卡，数值取该版本词条区间内的实用整数值。
    return {
        "rank": 8, "disposition": 1.2,
        "stats": [{"code": "0", "value": crit}, {"code": "1", "value": damage}],
    }


def golden_cases():
    standard = target()
    burston = {
        "weapon": BURSTON, "fireMode": 1,
        "scenario": {"incarnonCycle": True, "combatState": {"gaugeChargingHeadshots": True}},
    }
    arcane = {"path": MERCILESS, "rank": 5, "stacks": 8}
    glaive = {"weapon": GLAIVE, "fireMode": 1, "scenario": melee("glaive", True)}
    magistar = {"weapon": MAGISTAR, "scenario": melee("slamBullet", True)}
    fast = {"profile": "fast", "autoIncarnon": False, "autoExilus": False, "arcanes": []}
    tie = {**burston, **target(level=130), **fast, "minFill": 0, "maxFill": 0}
    ranged_search = {**burston, **target(window=8), **fast, "slotCount": 4}
    melee_search = {**magistar, **target(level=90, window=6), **fast, "slotCount": 5}
    return [
        get_case("health", "health"),
        get_case("catalog", "catalog"),
        post_case("simulate-braton-standard", "simulate", {"weapon": BRATON, **standard}),
        post_case("simulate-burston-gauge-on", "simulate", {**burston, **standard}),
        post_case("simulate-burston-gauge-off", "simulate", {
            **burston, **standard,
            "scenario": {"incarnonCycle": True, "combatState": {"gaugeChargingHeadshots": False}},
        }),
        post_case("simulate-torid-cycle", "simulate", {
            "weapon": TORID, "fireMode": 1, **standard, "scenario": {"incarnonCycle": True},
        }),
        post_case("simulate-zymos-head", "simulate", {
            "weapon": ZYMOS, **target(headshot=1.0), "scenario": {"aiming": True},
        }),
        post_case("simulate-braton-overguard", "simulate", {"weapon": BRATON, **target(overguard=True)}),
        post_case("simulate-braton-shield", "simulate", {"weapon": BRATON, **target(shield=3600)}),
        post_case("simulate-braton-lv9999", "simulate", {
            "weapon": BRATON, **target(level=9999, window=180, headshot=1.0, overguard=True),
        }),
        post_case("simulate-magistar-light", "simulate", {
            "weapon": MAGISTAR, **standard, "scenario": melee("combo", False),
        }),
        post_case("simulate-magistar-heavy", "simulate", {
            "weapon": MAGISTAR, **standard, "scenario": melee("combo", True),
        }),
        post_case("simulate-magistar-slam-bullet", "simulate", {**magistar, **standard}),
        post_case("simulate-glaive-detonation", "simulate", {**glaive, **standard}),
        post_case("simulate-burston-loadout", "simulate", {
            **burston, **standard, "mods": [
                {"path": path, "rank": rank, "slot": slot}
                for slot, (path, rank) in enumerate(LOADOUT_MODS)
            ],
            "slotCount": 8,
            "slotPolarities": ["AP_ATTACK"] * 8,
            "riven": {**riven(178, 143), "slot": 7, "polarity": "AP_ATTACK"},
            "arcanes": [arcane], "exilus": {"path": EXILUS, "rank": 5},
            "incarnonChoices": {"tier2": "2-2", "tier3": "3-3", "tier4": "4-2"},
        }),
        post_case("build-burston-auto-tie", "build", {**tie, "autoIncarnon": True}),
        post_case("build-burston-fixed-tie", "build", {
            **tie, "incarnonChoices": {"tier2": "2-2", "tier3": "3-1", "tier4": "4-1"},
        }),
        post_case("build-burston-riven", "build", {
            **ranged_search, "riven": riven(178, 143), "autoExilus": True, "arcanes": [arcane],
        }),
        post_case("build-magistar-slam-bullet", "build", melee_search),
        post_case("analyze-burston-riven", "analyze", {
            **burston, "profile": "ultra", "slotCount": 4, **riven(178, 143),
            "benchmarkTarget": target(window=8),
        }),
        post_case("analyze-magistar-slam-bullet", "analyze", {
            **magistar, "profile": "ultra", "slotCount": 5, **riven(214, 107),
            "benchmarkTarget": target(level=90, window=6),
        }),
        post_case("matrix-burston-31", "trait-matrix", {**ranged_search, "roll": "3+1-"}),
        post_case("matrix-burston-21", "trait-matrix", {**ranged_search, "roll": "2+1-"}),
        post_case("matrix-magistar-slam-bullet", "trait-matrix", {**melee_search, "roll": "both"}),
    ]


def catalog_summary(body):
    counts, entries = {}, {}
    for name, (field, paths) in CATALOG_SELECTION.items():
        rows = body[field]
        counts[name] = len(rows)
        selected = [row for row in rows if row["path"] in paths]
        missing = paths - {row["path"] for row in selected}
        if missing:
            raise ValueError(f"目录 {field} 缺少代表条目 {', '.join(sorted(missing))}")
        entries[name] = selected
    return {
        "buildLabel": body["buildLabel"], "schemaVersion": body["schemaVersion"],
        "counts": counts, "entries": entries,
    }


def stable_response(path, body):
    if path == "/api/catalog":
        return catalog_summary(body)
    if not isinstance(body, dict):
        return body
    for fields in VOLATILE_FIELDS.get(path, ()):
        parent = body
        for key in fields[:-1]:
            parent = parent.get(key) if isinstance(parent, dict) else None
        if isinstance(parent, dict):
            parent.pop(fields[-1], None)
    if path == "/api/build" and isinstance(body.get("skipped"), list):
        body["skipped"] = [
            AUTO_ROUTE_NOTE.sub(r"\1，<耗时>", note)
            if isinstance(note, str) and note.startswith("optimizer-auto-incarnon: ") else note
            for note in body["skipped"]
        ]
    return body


def describe(value):
    rendered = json.dumps(value, ensure_ascii=False)
    return rendered if len(rendered) <= 180 else rendered[:177] + "..."


def differences(expected, actual, path="$"):
    """比较 JSON 类型和值并产出字段路径。对象的键顺序不参与比较。"""
    if type(expected) is not type(actual):
        yield f"{path} 类型不同，期望 {type(expected).__name__} {describe(expected)}，实际 {type(actual).__name__} {describe(actual)}"
    elif isinstance(expected, dict):
        for key in sorted(expected.keys() - actual.keys()):
            yield f"{path}[{key!r}] 缺失，期望 {describe(expected[key])}"
        for key in sorted(actual.keys() - expected.keys()):
            yield f"{path}[{key!r}] 多出字段，实际 {describe(actual[key])}"
        for key in sorted(expected.keys() & actual.keys()):
            yield from differences(expected[key], actual[key], f"{path}[{key!r}]")
    elif isinstance(expected, list):
        if len(expected) != len(actual):
            yield f"{path} 列表长度不同，期望 {len(expected)}，实际 {len(actual)}"
        for index, (left, right) in enumerate(zip(expected, actual)):
            yield from differences(left, right, f"{path}[{index}]")
    elif isinstance(expected, float):
        limit = max(TOLERANCE, TOLERANCE * max(abs(expected), abs(actual)))
        if not (math.isfinite(expected) and math.isfinite(actual) and abs(expected - actual) <= limit):
            yield f"{path} 浮点值不同，期望 {expected!r}，实际 {actual!r}，容差 {limit:g}"
    elif expected != actual:
        yield f"{path} 值不同，期望 {describe(expected)}，实际 {describe(actual)}"


def reject_constant(value):
    raise ValueError(f"JSON 中存在非有限数值 {value}")


def send_http(url, case, timeout=HTTP_TIMEOUT):
    origin = urlsplit(url)
    connection = http.client.HTTPSConnection if origin.scheme == "https" else http.client.HTTPConnection
    body = json.dumps(case["body"], ensure_ascii=False, allow_nan=False).encode("utf-8") if "body" in case else None
    headers = {"Content-Type": "application/json"} if body is not None else {}
    start = time.perf_counter()
    with closing(connection(origin.hostname, origin.port, timeout=timeout)) as conn:
        conn.request(case["method"], case["path"], body=body, headers=headers)
        reply = conn.getresponse()
        raw = reply.read()
        elapsed = time.perf_counter() - start
        status = reply.status
        content_type = reply.headers.get_content_type()
    if content_type != "application/json":
        raise ValueError(f"HTTP {status} 返回 {content_type}，响应开头 {raw[:180]!r}")
    return status, raw, elapsed


def request_json(url, case):
    status, raw, _ = send_http(url, case)
    return status, json.loads(raw, parse_constant=reject_constant)


@contextmanager
def launch_server():
    """启动真实发布入口，等待首个健康响应，并回收进程。"""
    try:
        with socket.socket() as port_socket:
            port_socket.bind(("127.0.0.1", 0))
            port = port_socket.getsockname()[1]
    except OSError as exc:
        raise RuntimeError(f"无法为 launch.py 分配 127.0.0.1 监听端口，{exc}") from exc
    url = f"http://127.0.0.1:{port}"
    with tempfile.TemporaryFile() as log:
        started = time.perf_counter()
        process = subprocess.Popen(
            [sys.executable, str(ROOT / "launch.py"), "--port", str(port)],
            cwd=ROOT, stdout=log, stderr=subprocess.STDOUT,
        )
        try:
            last_error = ""
            while process.poll() is None and time.perf_counter() - started < STARTUP_TIMEOUT:
                try:
                    status, raw, health_elapsed = send_http(url, get_case("health", "health"), timeout=0.5)
                    if status == 200 and process.poll() is None:
                        startup = time.perf_counter() - started
                        yield url, process.pid, startup, (health_elapsed, len(raw))
                        return
                    last_error = f"HTTP {status}，响应开头 {raw[:180]!r}"
                except (OSError, http.client.HTTPException, ValueError) as exc:
                    last_error = str(exc)
                time.sleep(0.02)
            log.seek(0)
            tail = log.read().decode("utf-8", errors="replace")[-3000:]
            raise RuntimeError(f"launch.py 未通过健康检查（退出码 {process.poll()}）{last_error}\n{tail}")
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def write_json(path, value):
    path = Path(path)
    payload = json.dumps(value, ensure_ascii=False, allow_nan=False, indent=2) + "\n"
    stream = tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent, delete=False)
    temp = Path(stream.name)
    try:
        with stream:
            stream.write(payload)
        temp.replace(path)
    finally:
        temp.unlink(missing_ok=True)


def record_golden():
    cases = golden_cases()
    with launch_server() as (url, _, _, _):
        for case in cases:
            status, body = request_json(url, case)
            if status != 200:
                raise ValueError(f"{case['id']} 返回 HTTP {status} {describe(body)}")
            case["expected"] = {"status": status, "response": stable_response(case["path"], body)}
            print(f"已录制 {case['id']}", flush=True)
    responses = {case["id"]: case["expected"]["response"] for case in cases}
    auto = responses["build-burston-auto-tie"]
    fixed = responses["build-burston-fixed-tie"]
    auto_route = auto["incarnonChoices"]
    if (auto_route["tier2"] != "2-1"
            or auto["autoLoadout"]["incarnon"]["defaultRoute"]["tier2"] != "2-1"
            or fixed["incarnonChoices"] != {**auto_route, "tier2": "2-2"}
            or auto["build"]["objectiveScore"] != fixed["build"]["objectiveScore"]):
        raise ValueError("伯斯顿自动灵化用例未复现同分时的 2-1 优先规则")
    GOLDEN.parent.mkdir(parents=True, exist_ok=True)
    write_json(GOLDEN, {"fixtureVersion": FIXTURE_VERSION, "cases": cases})
    print(f"已写入 {len(cases)} 个用例到 {GOLDEN.relative_to(ROOT)}")


def verify_golden(url=None):
    """重放磁盘夹具，返回失败说明列表。提供 URL 时只执行 HTTP 请求。"""
    try:
        with GOLDEN.open(encoding="utf-8") as stream:
            fixture = json.load(stream, parse_constant=reject_constant)
    except FileNotFoundError as exc:
        raise RuntimeError(f"缺少 {GOLDEN.relative_to(ROOT)}，请先执行 --record") from exc
    if fixture["fixtureVersion"] != FIXTURE_VERSION or not fixture["cases"]:
        raise ValueError("黄金夹具版本不受支持或用例为空")
    if url is None:
        with launch_server() as (local_url, _, _, _):
            return verify_cases(local_url, fixture["cases"])
    return verify_cases(url, fixture["cases"])


def verify_cases(url, cases):
    failures = []
    for case in cases:
        label = f"{case['id']}（{case['method']} {case['path']}）"
        expected = case["expected"]
        try:
            status, body = request_json(url, case)
            if status != expected["status"]:
                failures.append(f"{label} 期望 HTTP {expected['status']}，实际 {status} {describe(body)}")
                continue
            diffs = differences(expected["response"], stable_response(case["path"], body))
            for index, diff in enumerate(diffs):
                if index == 8:
                    failures.append(f"{label} 还有其他差异")
                    break
                failures.append(f"{label} {diff}")
        except (OSError, http.client.HTTPException, TypeError, ValueError, KeyError) as exc:
            failures.append(f"{label} 请求或响应无效，{exc}")
    return failures


def profile_cases():
    cases = {case["id"]: case for case in golden_cases()}
    ranged = cases["build-burston-riven"]
    return {
        "health": cases["health"],
        "catalog": cases["catalog"],
        "simulate": cases["simulate-burston-loadout"],
        "build(fast)": post_case("build-burston-fast", "build", {
            key: value for key, value in ranged["body"].items() if key != "riven"
        }),
        "build(riven)": ranged,
        "analyze": cases["analyze-burston-riven"],
        "trait-matrix": cases["matrix-magistar-slam-bullet"],
    }


def measure_http():
    cases = profile_cases()
    wire = {}
    with launch_server() as (url, pid, startup, (health_elapsed, health_bytes)):
        startup_rss = rss_mib(pid)
        wire["health"] = {"coldMs": health_elapsed * 1000, "coldBytes": health_bytes}
        for name, case in cases.items():
            if name == "health":
                continue
            elapsed, size = timed_reply(url, case)
            wire[name] = {"coldMs": elapsed * 1000, "coldBytes": size}
        loaded_rss = rss_mib(pid)
        for name, case in cases.items():
            samples = [timed_reply(url, case)[0] * 1000 for _ in range(WARM_SAMPLES)]
            wire[name].update(warmMs=samples, warmP50Ms=statistics.median(samples), warmMaxMs=max(samples))
    return {"startupSeconds": startup, "startupRssMiB": startup_rss, "loadedRssMiB": loaded_rss, "requests": wire}


def rss_mib(pid):
    output = subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)], text=True)
    return int(output.strip()) / 1024


def timed_reply(url, case):
    status, body, elapsed = send_http(url, case)
    if status != 200:
        raise ValueError(f"{case['id']} 返回 HTTP {status}，响应开头 {body[:180]!r}")
    return elapsed, len(body)


def profile_python_case(name):
    # 只有剖析子进程会加载 Python 业务代码，使用发布入口的工作进程和缓存配置。
    sys.path.insert(0, str(ROOT))
    from launch import app, health
    from wfarsenal import native_replay

    health()
    app._disable_analysis_statics_persistence()
    handlers = {
        "simulate": app.simulate_manual_payload,
        "build(riven)": app.build_weapon_route,
        "analyze": app.analyze_payload,
        "trait-matrix": app.trait_matrix_payload,
    }
    before = dict(native_replay.STATS)
    profiler = cProfile.Profile()
    body = profile_cases()[name]["body"]
    start = time.perf_counter()
    profiler.runcall(handlers[name], body)
    wall_seconds = time.perf_counter() - start
    rows = []
    rust_seconds = 0.0
    for entry in profiler.getstats():
        code = entry.code
        if isinstance(code, str):
            function = code
            if "wfarsenal.wfcore." in code:
                rust_seconds += entry.inlinetime
        else:
            filename = code.co_filename
            if filename.startswith(str(ROOT) + "/"):
                filename = filename[len(str(ROOT)) + 1:]
            function = f"{filename}:{code.co_firstlineno}({code.co_name})"
        rows.append({
            "function": function, "calls": entry.callcount,
            "tottime": entry.inlinetime, "cumtime": entry.totaltime,
        })
    total = sum(row["tottime"] for row in rows)
    return {
        "wallSeconds": wall_seconds,
        "profiledSelfSeconds": total,
        "pythonAndOtherSeconds": total - rust_seconds,
        "wfcoreSeconds": rust_seconds,
        "pythonAndOtherPercent": 100 * (total - rust_seconds) / total,
        "wfcorePercent": 100 * rust_seconds / total,
        "nativeReplayStats": {key: value - before[key] for key, value in native_replay.STATS.items()},
        "topTottime": sorted(rows, key=lambda row: (-row["tottime"], row["function"]))[:15],
        "topCumtime": sorted(rows, key=lambda row: (-row["cumtime"], row["function"]))[:15],
    }


PROFILE_NAMES = ("build(riven)", "analyze", "trait-matrix", "simulate")


def profile_baseline():
    wire = measure_http()
    profiles = {}
    for name in PROFILE_NAMES:
        child = subprocess.run(
            [sys.executable, str(Path(__file__).resolve()), "--_profile-case", name],
            cwd=ROOT, capture_output=True, text=True, timeout=60,
        )
        if child.returncode:
            raise RuntimeError(f"{name} 剖析子进程退出码 {child.returncode}\n{child.stderr}{child.stdout}")
        profiles[name] = json.loads(child.stdout, parse_constant=reject_constant)
    return {"python": platform.python_version(), "platform": platform.platform(), "http": wire, "cProfile": profiles}


def http_origin(value):
    try:
        origin = urlsplit(value)
        if (origin.scheme in {"http", "https"} and origin.hostname and origin.port != 0
                and not origin.username and not origin.password
                and origin.path in {"", "/"} and not origin.query and not origin.fragment):
            return value
    except ValueError:
        pass
    raise argparse.ArgumentTypeError("URL 必须是 HTTP 服务地址，例如 http://127.0.0.1:8765")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--record", action="store_true", help="从发布入口录制黄金夹具")
    mode.add_argument("--verify", action="store_true", help="对拍黄金夹具")
    mode.add_argument("--profile", action="store_true", help="输出 HTTP、内存和函数剖析基线")
    mode.add_argument("--_profile-case", choices=PROFILE_NAMES, help=argparse.SUPPRESS)
    parser.add_argument("--url", type=http_origin, help="对拍已运行的 HTTP 服务")
    parser.add_argument("--out", type=Path, help="把性能基线写入指定 JSON 文件")
    args = parser.parse_args()
    if args.url and not args.verify:
        parser.error("--url 仅用于 --verify")
    if args.out and not args.profile:
        parser.error("--out 仅用于 --profile")
    try:
        if args.record:
            record_golden()
        elif args.verify:
            start = time.perf_counter()
            failures = verify_golden(args.url)
            if failures:
                print("\n".join(failures), file=sys.stderr)
                return 1
            print(f"全部黄金用例通过，总耗时 {time.perf_counter() - start:.2f} 秒")
        elif args.profile:
            report = profile_baseline()
            if args.out:
                write_json(args.out, report)
            else:
                print(json.dumps(report, ensure_ascii=False, allow_nan=False, indent=2))
        else:
            print(json.dumps(profile_python_case(args._profile_case), ensure_ascii=False, allow_nan=False))
    except (OSError, http.client.HTTPException, KeyError, TypeError, ValueError, RuntimeError, subprocess.SubprocessError) as exc:
        print(f"Phase 0 执行失败，{exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
