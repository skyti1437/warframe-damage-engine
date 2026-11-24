import io
import json
import math
import os
import unittest
from unittest.mock import Mock, patch

os.environ.setdefault("WF_ENGINE_WORKERS", "1")

import app
from wfarsenal import native_replay, search_session, trait_matrix


BURSTON = "/Lotus/Weapons/Tenno/LongGuns/PrimeBurston/PrimeBurston"
MAGISTAR = "/Lotus/Weapons/Tenno/Melee/Maces/PaladinMace/PaladinMaceWeapon"
GLAIVE = "/Lotus/Weapons/Tenno/Melee/Glaives/PrimeGlaive/PrimeGlaiveWeapon"
THRAX = "/Lotus/Types/Enemies/Duviri/Avatars/DuviriMeleeAvatar"


class TraitMatrixTests(unittest.TestCase):
    def assert_matrix(self, result, context):
        matrix = result["matrix"]
        build = result["build"]
        self.assertEqual(set(result["weapon"]), {"path", "nameEn", "nameZh", "weaponType", "fireModes"})
        self.assertEqual(context["metric"], "killsPerMinute")
        self.assertIsNone(context["riven"])
        self.assertTrue(build["capacity"]["valid"])
        self.assertTrue(build["mods"])
        self.assertEqual(len(build["mods"]), len({mod["slot"] for mod in build["mods"]}))
        self.assertEqual(matrix["baseline_dps"], build["full_dps"])
        self.assertEqual(matrix["baseline_kpm"], build["kpm"])
        self.assertGreater(build["full_dps"], 0)
        self.assertGreater(build["kpm"], 0)
        groups = trait_matrix.candidate_sets(
            context["weapon"], context["catalogWeapon"], app.EXPORTS["rivenTemplates"],
        )
        for index, sign in enumerate(("positive", "negative")):
            rows = matrix[sign]
            self.assertEqual([row["code"] for row in rows], sorted(groups[0][index]))
            for row in rows:
                self.assertTrue({
                    "code", "stat_id", "unit", "spliced", "modeled", "neutral", "weakpoint",
                    "reoptimized", "shown_31", "pct_31", "shown_21", "pct_21", "elem_build",
                }.issubset(row))
                for roll_index, roll in enumerate(("31", "21")):
                    line = groups[roll_index][index][row["code"]]
                    value = -line.value if line.negative else line.value
                    self.assertEqual(row[f"shown_{roll}"], round(value, 2))
                    if row["modeled"]:
                        self.assertTrue(math.isfinite(row[f"pct_{roll}"]))
                    else:
                        self.assertIsNone(row[f"pct_{roll}"])

    def solve(self, request):
        contexts = []
        build = app.build_weapon_payload

        def capture(payload, **kwargs):
            self.assertNotIn("_searchBudgetSeconds", payload)
            self.assertIsNone(search_session.SESSION.get())
            result, context = build(payload, **kwargs)
            contexts.append(context)
            return result, context

        with patch.object(app, "build_weapon_payload", side_effect=capture):
            result = app.trait_matrix_payload(request)
        self.assertEqual(len(contexts), 1)
        context = contexts[0]
        self.assert_matrix(result, context)
        window = app.optimizer._event_window(context["evaluation"].stats, context["benchmarkContext"])
        return result, window

    def test_combined_defenses_incarnon_and_weakpoint_contexts(self):
        request = {
            "weapon": BURSTON, "fireMode": 1, "scenario": {"incarnonCycle": True, "armourStrip": 0.15},
            "benchmarkTarget": {
                "path": THRAX, "level": 130, "windowSeconds": 12,
                "enemyEnhancements": {"noOverguard": False, "shieldOverride": 3600},
            },
            "profile": "fast", "autoIncarnon": False, "arcanes": [], "autoExilus": False,
            "_searchBudgetSeconds": 0.00001,
        }
        result, window = self.solve(request)
        self.assertEqual(result["fireMode"], 1)
        self.assertTrue(result["matrix"]["incarnon_cycle"])
        self.assertFalse(result["matrix"]["incarnon_form"])
        self.assertEqual(result["evaluationTarget"]["armourStrip"], 0.15)
        self.assertEqual(result["evaluationTarget"]["enemyEnhancements"]["shieldOverride"], 3600)
        self.assertIs(result["evaluationTarget"]["enemyEnhancements"]["noOverguard"], False)
        self.assertGreater(result["matrix"]["baseline_weakpoint_kpm"], 0)
        self.assertEqual({row["code"] for row in result["matrix"]["positive"] if row["weakpoint"]}, {"W", "Y"})
        self.assertEqual(window["transforms"], 0)
        self.assertEqual(result["build"]["arcanes"], [])
        self.assertIsNone(result["build"]["exilus"])

        # The same normalized context shares one result across wire shapes and roll views.
        target = request["benchmarkTarget"]
        equivalent = {
            **request, "benchmarkTarget": None, "weapon": f" {BURSTON} ", "incarnonCycle": True,
            "scenario": None, "enemy": THRAX, "level": target["level"], "headshotChance": 0.0,
            "windowSeconds": float(target["windowSeconds"]), "enemyEnhancements": target["enemyEnhancements"],
            "armourStrip": 0.15, "roll": "2+1-", "liveChannel": "test", "liveSeq": 1,
        }
        with patch.object(app, "build_weapon_payload", side_effect=AssertionError("cache miss")):
            cached = app.trait_matrix_payload(equivalent)
        self.assertEqual(cached["build"], result["build"])
        for row, original in zip(cached["matrix"]["positive"], result["matrix"]["positive"], strict=True):
            self.assertEqual(row["pct_21"], original["pct_21"])
            self.assertIsNone(row["shown_31"])
            self.assertIsNone(row["pct_31"])

        mixed, window = self.solve({**request, "headshotChance": 0.75})
        self.assertGreaterEqual(window["transforms"], 1)
        self.assertEqual(mixed["evaluationTarget"]["headshotChance"], 0.75)
        self.assertIsNone(mixed["matrix"]["baseline_weakpoint_kpm"])
        self.assertFalse(any(row["weakpoint"] for row in mixed["matrix"]["positive"]))

    def test_melee_routes(self):
        cases = (
            (MAGISTAR, "combo", False, "CC_GROUND"),
            (MAGISTAR, "combo", True, "heavy-attack"),
            (MAGISTAR, "slam", True, "heavy-slam"),
            (MAGISTAR, "slamBullet", True, "heavy-slam"),
            (GLAIVE, "glaive", True, "heavy-throw-detonate"),
        )
        scores = []
        for weapon, route, heavy, label in cases:
            with self.subTest(weapon=weapon, route=route, heavy=heavy):
                result, window = self.solve({
                    "weapon": weapon, "profile": "fast", "windowSeconds": 6, "level": 90,
                    "arcanes": [], "autoExilus": False, "autoIncarnon": False,
                    "scenario": {"heavyAttack": heavy, "combatState": {"meleeRoute": route}},
                })
                self.assertEqual(result["evaluationScenario"]["heavyAttack"], heavy)
                self.assertEqual(result["evaluationScenario"]["combatState"]["meleeRoute"], route)
                self.assertEqual(window["melee"]["route"], route)
                if label == "CC_GROUND":
                    self.assertTrue(window["melee"]["label"].startswith(label))
                else:
                    self.assertEqual(window["melee"]["label"], label)
                by_code = {row["code"]: row for row in result["matrix"]["positive"]}
                if heavy:
                    self.assertGreater(by_code["HD"]["pct_31"], 0)
                else:
                    self.assertEqual(by_code["HD"]["pct_31"], 0)
                if route.startswith("slam"):
                    self.assertGreater(by_code["SL"]["pct_31"], 0)
                scores.append(result["build"]["kpm"])
        self.assertEqual(len(set(scores[:4])), 4)

        result, window = self.solve({
            "weapon": GLAIVE, "fireMode": 1, "profile": "fast", "windowSeconds": 4, "level": 60,
            "arcanes": [], "autoExilus": False, "autoIncarnon": False,
            "scenario": {"heavyAttack": True, "combatState": {"meleeRoute": "glaive"}},
        })
        self.assertEqual(result["fireMode"], 1)
        self.assertEqual(window["melee"]["label"], "heavy-throw-detonate")

    def test_http_dispatch(self):
        body = json.dumps({"weapon": MAGISTAR, "roll": "3+1-"}).encode()
        handler = app.PortableHandler.__new__(app.PortableHandler)
        handler.path = "/api/trait-matrix"
        handler.headers = {"Content-Length": str(len(body))}
        handler.rfile = io.BytesIO(body)
        handler._send_json = Mock()
        with patch.object(app, "trait_matrix_payload", return_value={"matrix": {}}) as solve:
            handler.do_POST()
        solve.assert_called_once_with({"weapon": MAGISTAR, "roll": "3+1-"})
        handler._send_json.assert_called_once_with(200, {"matrix": {}}, cache="no-store")

    def test_analyze_without_budget(self):
        cases = (
            (BURSTON, 1, {"incarnonCycle": True}),
            (MAGISTAR, 0, {"heavyAttack": True, "combatState": {"meleeRoute": "combo"}}),
            (MAGISTAR, 0, {"heavyAttack": True, "combatState": {"meleeRoute": "slamBullet"}}),
            (GLAIVE, 0, {"heavyAttack": True, "combatState": {"meleeRoute": "glaive"}}),
        )
        with patch.object(app, "_persist_analysis_statics_cache"):
            for path, mode, scenario in cases:
                with self.subTest(path=path, scenario=scenario):
                    meta = app.CATALOG_WEAPONS_BY_PATH[path]
                    disposition = meta["disposition"]
                    family = meta["rivenType"]
                    scale = app.optimizer.theoretical_structure_scale(2, False)
                    stats = [
                        {"code": code, "value": app.optimizer.RIVEN_BASE_VALUES[family][code] * disposition * scale}
                        for code in ("0", "1")
                    ]
                    result = app.analyze_payload({
                        "weapon": path, "fireMode": mode, "scenario": scenario, "profile": "ultra",
                        "disposition": disposition, "stats": stats,
                        "benchmarkTarget": {
                            "enemy": THRAX, "level": 170, "steelPath": True, "windowSeconds": 8,
                            "headshotChance": 0.5, "enemyEnhancements": {"noOverguard": False, "shieldOverride": 3600},
                        },
                    })
                    self.assertEqual(result["searchWork"]["limited"], 0)
                    self.assertIsNone(result["searchWork"]["budgetSeconds"])
                    self.assertGreater(result["searchWork"]["evaluations"], 0)
                    self.assertEqual(result["evaluationTarget"]["enemyEnhancements"]["shieldOverride"], 3600)
                    self.assertGreater(result["scoringValues"]["baseline"], 0)
                    for field in ("incarnonCycle", "heavyAttack"):
                        if field in scenario:
                            self.assertEqual(result["evaluationScenario"][field], scenario[field])
                    if "combatState" in scenario:
                        self.assertEqual(result["evaluationScenario"]["combatState"]["meleeRoute"],
                                         scenario["combatState"]["meleeRoute"])

    def test_release_identity(self):
        if not native_replay.ENABLED:
            self.skipTest("the published identity requires the native extension")
        self.assertEqual(app.EXPORTS["buildLabel"], "2026.08.13.13.24")
        self.assertEqual(app.ANALYSIS_STATICS_RUNTIME_INPUTS, "535ed49f5a51149d")


class SearchBudgetTests(unittest.TestCase):
    def test_default_search_has_no_wall_clock_limit(self):
        @search_session.session_request
        def solve(_exports, request):
            with patch.object(search_session.time, "monotonic", return_value=1e6):
                for allowance in (None, 0.001):
                    self.assertEqual(search_session.phase_deadline(allowance), math.inf)
                    self.assertFalse(search_session.expired(search_session.phase_deadline(allowance)))
            return {"searchStatus": {}, "skipped": []}

        with patch.object(search_session.time, "monotonic", return_value=0), patch.object(search_session, "UNBOUNDED", False):
            result = solve(None, {"profile": "ultra"})
        self.assertEqual(result["searchWork"]["limited"], 0)
        self.assertIsNone(result["searchWork"]["budgetSeconds"])
        json.dumps(result, allow_nan=False)

    def test_explicit_budget_limits_the_search(self):
        @search_session.session_request
        def solve(_exports, request):
            self.assertEqual(search_session.phase_deadline(), 0.25)
            with patch.object(search_session.time, "monotonic", return_value=1):
                self.assertTrue(search_session.expired(search_session.phase_deadline()))
            return {"searchStatus": {}, "skipped": []}

        with patch.object(search_session.time, "monotonic", return_value=0), patch.object(search_session, "UNBOUNDED", False):
            result = solve(None, {"_searchBudgetSeconds": 0.25})
        self.assertEqual(result["searchWork"]["limited"], 1)
        self.assertEqual(result["searchWork"]["budgetSeconds"], 0.25)


if __name__ == "__main__":
    unittest.main()
