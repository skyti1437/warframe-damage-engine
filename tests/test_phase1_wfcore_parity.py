"""对拍冻结的发布扩展与独立 Rust 核心驱动的扩展。"""

import hashlib
import importlib.util
import inspect
import math
import os
import struct
import sys
import unittest
from collections import OrderedDict
from contextlib import ExitStack, contextmanager
from copy import deepcopy
from pathlib import Path
from unittest.mock import patch

from tools.phase0_benchmark_and_record import AUTO_ROUTE_NOTE, golden_cases


ROOT = Path(__file__).resolve().parents[1]
EXTENSION = "wfcore.pyd" if os.name == "nt" else "wfcore.so"
FROZEN = ROOT / "vendor" / "wfarsenal" / EXTENSION
CANDIDATE = ROOT / "rust" / "target" / "release" / EXTENSION
CASE_IDS = (
    "simulate-braton-standard",
    "simulate-burston-gauge-on",
    "simulate-burston-gauge-off",
    "simulate-burston-loadout",
    "simulate-torid-cycle",
    "simulate-zymos-head",
    "simulate-braton-overguard",
    "simulate-braton-shield",
    "simulate-magistar-slam-bullet",
    "simulate-glaive-detonation",
    "build-burston-auto-tie",
    "build-burston-riven",
    "build-magistar-slam-bullet",
    "analyze-burston-riven",
    "matrix-burston-31",
    "matrix-magistar-slam-bullet",
)
WALL_CLOCK_FIELDS = {
    "/api/build": (("autoLoadout", "seconds"), ("autoLoadout", "incarnon", "seconds")),
    "/api/analyze": (
        ("searchWork", "elapsedSeconds"),
        ("searchStatus", "work", "elapsedSeconds"),
        ("incarnonRouteScreen", "seconds"),
    ),
}


def load_candidate():
    # 扩展入口仍为 PyInit_wfcore，单独加载可避免替换磁盘上的发布二进制。
    spec = importlib.util.spec_from_file_location("wfcore", CANDIDATE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@contextmanager
def using_core(core, *, manual_replays=None):
    import app
    import wfarsenal
    from wfarsenal import (
        combat_replay, evaluation_cache, mods_native, native_replay, optimizer,
        search_combat, search_session, status_native,
    )

    with ExitStack() as stack:
        module_key = "wfarsenal.wfcore"
        stack.callback(sys.modules.__setitem__, module_key, sys.modules[module_key])
        sys.modules[module_key] = core
        stack.enter_context(patch.dict(os.environ, {"WF_ANALYZER_SERIAL": "1"}))
        for module in (wfarsenal, native_replay, mods_native, status_native):
            stack.enter_context(patch.object(module, "wfcore", core))
        for module in (native_replay, mods_native, status_native):
            stack.enter_context(patch.object(module, "ENABLED", True))
            stack.enter_context(patch.object(module, "SHADOW", False))
        for module, fields in (
            (native_replay, ("_TARGET_SETUPS", "STATS", "HEALTH_BATCH_STATS")),
            (mods_native, ("_PREPS",)),
            (status_native, ("_TARGETS",)),
            (search_combat, ("_REPLAYS",)),
            (optimizer, ("_BASELINE_EVALUATIONS",)),
            (evaluation_cache, ("_results", "_cards", "STATS")),
            (app, ("ANALYSIS_STATICS_CACHE", "_ANALYSIS_RESULTS", "_BUILD_RESULTS", "_TRAIT_MATRIX_RESULTS")),
        ):
            for name in fields:
                current = getattr(module, name)
                empty = OrderedDict() if isinstance(current, OrderedDict) else {}
                if name.endswith("STATS"):
                    empty.update(dict.fromkeys(current, 0))
                stack.enter_context(patch.object(module, name, empty))
        stack.enter_context(patch.object(evaluation_cache, "_bytes", 0))
        stack.enter_context(patch.object(evaluation_cache, "ENABLED", True))
        stack.enter_context(patch.object(search_session, "UNBOUNDED", True))
        stack.enter_context(patch.object(native_replay, "HEALTH_BATCH", True))
        stack.enter_context(patch.object(app.ANALYSIS_STATICS_PERSISTENCE, "enabled", False))
        stack.enter_context(patch.object(app, "_refresh_analysis_statics_cache_if_newer", lambda: None))
        if manual_replays is not None:
            original = combat_replay.replay

            def replay_with_probe(stats, target, **options):
                result = original(stats, target, **options)
                if options.get("collect_timeline", True) and options.get("combat") is not None:
                    # 手动模拟的展示时间线沿用原路径，同时验证同一战斗的无时间线路径。
                    before = native_replay.STATS["native"]
                    probe = original(stats, target, **{**options, "collect_timeline": False})
                    if native_replay.STATS["native"] > before:
                        manual_replays.append(probe)
                return result

            stack.enter_context(patch.object(combat_replay, "replay", replay_with_probe))
        yield native_replay


def without_wall_clock(result, endpoint):
    for path in WALL_CLOCK_FIELDS.get(endpoint, ()):
        parent = result
        for key in path[:-1]:
            parent = parent.get(key, {}) if isinstance(parent, dict) else {}
        if isinstance(parent, dict):
            parent.pop(path[-1], None)
    if endpoint == "/api/build" and isinstance(result.get("skipped"), list):
        result["skipped"] = [
            AUTO_ROUTE_NOTE.sub(r"\1", note) if note.startswith("optimizer-auto-incarnon: ") else note
            for note in result["skipped"]
        ]
    return result


class Phase1ParityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import app
        from wfarsenal import wfcore

        cls.app = app
        cls.frozen = wfcore
        cls.candidate = load_candidate()

    def assert_bits(self, expected, actual, path="$"):
        if type(expected) is not type(actual):
            self.fail(f"{path} 类型不同：{type(expected).__name__} / {type(actual).__name__}")
        if isinstance(expected, float):
            if struct.pack(">d", expected) != struct.pack(">d", actual):
                self.fail(f"{path} 浮点位不同：{expected.hex()} / {actual.hex()}")
        elif isinstance(expected, dict):
            self.assertEqual(expected.keys(), actual.keys(), path)
            for key, value in expected.items():
                self.assert_bits(value, actual[key], f"{path}[{key!r}]")
        elif isinstance(expected, (list, tuple)):
            self.assertEqual(len(expected), len(actual), path)
            for i, (left, right) in enumerate(zip(expected, actual)):
                self.assert_bits(left, right, f"{path}[{i}]")
        else:
            self.assertEqual(expected, actual, path)

    def test_architecture_and_exports(self):
        source = ROOT / "rust" / "wfcore-lib"
        self.assertNotIn("pyo3", (source / "Cargo.toml").read_text(encoding="utf-8").lower())
        for file in (source / "src").rglob("*.rs"):
            with self.subTest(source=file.name):
                self.assertNotIn("pyo3", file.read_text(encoding="utf-8").lower())
        self.assertEqual(FROZEN.resolve(), Path(self.frozen.__file__).resolve())
        self.assertEqual(CANDIDATE.resolve(), Path(self.candidate.__file__).resolve())
        public = {name for name in dir(self.frozen) if not name.startswith("_")}
        self.assertEqual(public, {name for name in dir(self.candidate) if not name.startswith("_")})
        self.assertEqual(44, sum(name.isupper() for name in public))
        self.assertNotEqual(self.frozen.SOURCE_SHA256, self.candidate.SOURCE_SHA256)

        rust = ROOT / "rust"
        files = [rust / "Cargo.toml", rust / "Cargo.lock",
                 rust / "wfcore-lib" / "Cargo.toml", rust / "wfcore-py" / "Cargo.toml",
                 rust / "wfcore-py" / "build.rs"]
        for package in ("wfcore-lib", "wfcore-py"):
            files.extend((rust / package / "src").rglob("*.rs"))
        base = rust / "wfcore-py"
        digest = hashlib.sha256()
        paths = {Path(os.path.relpath(path, base)).as_posix(): path for path in files}
        for name, path in sorted(paths.items()):
            digest.update(name.encode())
            digest.update(b"\0")
            digest.update(path.read_bytes().replace(b"\r\n", b"\n"))
        self.assertEqual(digest.hexdigest(), self.candidate.SOURCE_SHA256)

        for name in sorted(public - {"SOURCE_SHA256"}):
            with self.subTest(symbol=name):
                original = getattr(self.frozen, name)
                candidate = getattr(self.candidate, name)
                if name.isupper():
                    self.assert_bits(original, candidate, name)
                elif isinstance(original, type):
                    self.assertEqual(original.__module__, candidate.__module__)
                    self.assertEqual(original.__bases__, candidate.__bases__)
                    methods = {key for key in vars(original) if not key.startswith("_")}
                    self.assertEqual(methods, {key for key in vars(candidate) if not key.startswith("_")})
                    self.assertEqual(original.__text_signature__, candidate.__text_signature__)
                    for key in methods:
                        self.assertEqual(
                            getattr(getattr(original, key), "__text_signature__", None),
                            getattr(getattr(candidate, key), "__text_signature__", None), key,
                        )
                else:
                    self.assertEqual(inspect.signature(original), inspect.signature(candidate))

    def test_native_primitives(self):
        def exercise(core):
            sums = [core.py_sum(values) for values in (
                [], [-0.0], [1e16, 1.0, -1e16], [0.1] * 10,
                [math.inf, 1.0], [math.inf, -math.inf],
            )]
            math_results = [core.c_math(*args) for args in (
                ("log", 0.0, 0.0), ("log", 2.0, 0.0), ("expm1", -1e-12, 0.0),
                ("erfc", 1.5, 0.0), ("pow", 0.9, 1.1), ("pow", math.nan, 0.0),
                ("unknown", 0.0, 0.0),
            )]

            heat = core.HeatChains()
            heat_results = []
            for time, absence, count, seed, duration, scale in (
                (0.0, 0.25, 0.75, 14.0, 6.0, 1.0),
                (0.5, 0.0, 1.0, 17.0, 3.0, 1.0),
                (2.5, 0.5, 1.25, 31.0, 5.0, 0.8),
            ):
                heat.expire(time)
                heat.add(time, absence, count, seed, duration, scale)
                heat_results.append((heat.active(), heat.alive(), heat.count(), heat.seed(), heat.strip(time)))
            for time in (3.5, 6.0, 7.5, 9.5):
                heat.expire(time)
                heat_results.append((heat.active(), heat.alive(), heat.count(), heat.seed(), heat.strip(time)))
            heat.reset()
            heat_results.append((heat.active(), heat.seed()))

            pool = core.CappedPool(3.0)
            pool_results = []
            for end, absence, count in ((7.0, 0.0, 2.0), (5.0, 0.5, 1.0), (9.0, 0.25, 2.0)):
                pool_results.append((pool.add(end, absence, count), bool(pool), pool.version,
                                     pool.distribution(), pool.stats()))
            pool.expire(5.0)
            pool_results.append((pool.distribution(), pool.stats(), pool.version))
            pool.reset()
            pool_results.append((bool(pool), pool.distribution(), pool.version))

            presence = core.StatusPresence()
            presence.add(1.5, 0.25)
            presence.add(2.5, 0.5)
            probabilities = [presence.probability(time) for time in (0.0, 1.5, 2.5)]
            attribution = core.KillAttribution(1.0, interval=0.5)
            attribution.add(0.0, 0.0, 15.0, 0.25, 0.0, direct=True)
            attribution.add(0.5, 12.0, 15.0, 0.25, 1.0, direct=True)
            kill = [attribution.crossing(0.5, 20.0), attribution.crossing(0.5, 20.0, resume=0.25)]
            attribution.reset()
            kill.append(attribution.crossing(0.5, 20.0))

            dists = [core.capped_stack_add_dists([[0.1, 0.9], [0.25, 0.5, 0.25]], 0.4, 1.2, cap)
                     for cap in (1, 3, 10)]
            vertices = [(0.0, 0.0, z) for z in (1.0, 2.0)]
            vertices += [(1.0, 0.0, z) for z in (1.0, 2.0)] + [(0.0, 1.0, z) for z in (1.0, 2.0)]
            spatial = core.SpatialIndex(vertices, [(0, 2, 4), (1, 3, 5), (0, 2, 4)])
            origin, direction = (0.2, 0.2, 0.0), (0.0, 0.0, 2.0)
            rays = (spatial.raycast(origin, direction),
                    spatial.all_hits(origin, direction, max_hits=2),
                    spatial.raycast_batch([(origin, direction, 1.5), (origin, direction, 0.5)]))
            with self.assertRaisesRegex(ValueError, "invalid ray"):
                spatial.raycast(origin, (0.0, 0.0, 0.0))

            graph = core.PathGraph(5, [(0, 1, 0.125, 7), (1, 2, 0.25, 5),
                                       (0, 2, 0.4, 9), (2, 3, 0.5, 8)])
            paths = (graph.search(0, False, []), graph.search(3, True, [5]))
            with self.assertRaisesRegex(ValueError, "navigation node out of range"):
                graph.search(5, False, [])
            core.bake_pool_stop()
            try:
                core.bake_pool_capture(4)
                capture = core.CappedPool(10.0)
                capture.add(6.0, 0.5, 0.5)
                capture.add(7.0, 0.5, 0.5)
                keys = core.bake_pool_pending()
                self.assertEqual([(0.5, 0.5)], keys)
                baked = core.bake_pool_reference(keys, 3)
                core.bake_pool_install(keys, 3, baked)
                pool = core.CappedPool(10.0)
                pool.add(6.0, 0.5, 0.5)
                bake = (baked, pool.distribution(), core.bake_pool_stats())
                self.assertGreater(bake[2]["hits"], 0)
            finally:
                core.bake_pool_stop()

            with self.assertRaisesRegex(ValueError, "config.full missing"):
                core.trajectory({}, None)
            with self.assertRaisesRegex(ValueError, "health batch requires 1..64 targets"):
                core.trajectory_health_batch({}, None, [])
            return (sums, math_results, heat_results, pool_results, probabilities, kill, dists, rays, paths, bake)

        self.assert_bits(exercise(self.frozen), exercise(self.candidate))

    def test_payloads(self):
        routes = {
            "/api/simulate": self.app.simulate_manual_payload,
            "/api/build": self.app.build_weapon_route,
            "/api/analyze": self.app.analyze_payload,
            "/api/trait-matrix": self.app.trait_matrix_payload,
        }
        phase0 = {case["id"]: case for case in golden_cases()}
        cases = [phase0[name] for name in CASE_IDS]
        cycle_off = deepcopy(phase0["simulate-burston-gauge-on"])
        cycle_off["id"] = "simulate-burston-cycle-off"
        cycle_off["body"]["scenario"]["incarnonCycle"] = False
        cases.insert(3, cycle_off)

        for case in cases:
            with self.subTest(case=case["id"]):
                endpoint = case["path"]
                outputs = []
                native_manual = []
                for core in (self.frozen, self.candidate):
                    probes = [] if endpoint == "/api/simulate" else None
                    with using_core(core, manual_replays=probes) as native:
                        result = routes[endpoint](deepcopy(case["body"]))
                        self.assertGreater(native.STATS["native"], 0, (case["id"], core.__file__, native.STATS))
                        self.assertEqual(0, native.STATS["fallback"], (case["id"], core.__file__, native.STATS))
                        outputs.append(without_wall_clock(result, endpoint))
                        if probes is not None:
                            self.assertTrue(probes, case["id"])
                            native_manual.append(probes)
                self.assert_bits(*outputs)
                if native_manual:
                    self.assert_bits(*native_manual, path="原生回放")
                self.assertTrue(outputs[0] == outputs[1], case["id"])
                if case["id"] == "build-burston-auto-tie":
                    self.assertEqual("2-1", outputs[1]["incarnonChoices"]["tier2"])


if __name__ == "__main__":
    unittest.main()
