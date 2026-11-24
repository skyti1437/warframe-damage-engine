import os
import unittest
from collections import OrderedDict
from unittest.mock import patch

os.environ.setdefault("WF_ENGINE_WORKERS", "1")

import app
from wfarsenal import native_replay, search_combat
from wfarsenal.damage import Scenario


FLAG = "gaugeChargingHeadshots"
BURSTON = "/Lotus/Weapons/Tenno/LongGuns/PrimeBurston/PrimeBurston"
BOAR = "/Lotus/Weapons/Tenno/Shotgun/PrimeBoar"
BRATON = "/Lotus/Weapons/Tenno/Rifle/BratonPrime"
TORID = "/Lotus/Weapons/ClanTech/Bio/BioWeapon"
ARGON_SCOPE = "/Lotus/Upgrades/Mods/Rifle/Event/CritChanceWhileAimingRifleMod"


def request(enabled=None, *, weapon=BURSTON, fire_mode=1, cycle=True, aiming=False):
    scenario = {"incarnonCycle": cycle, "aiming": aiming}
    if enabled is not None:
        scenario["combatState"] = {FLAG: enabled}
    return {
        "weapon": weapon, "fireMode": fire_mode, "enemy": app._BENCHMARK_ENEMY_PATH,
        "level": 130, "windowSeconds": 30, "headshotChance": 0.0, "scenario": scenario,
    }


class GaugeChargingHeadshotsTests(unittest.TestCase):
    def test_burston_body_shots_charge_the_gauge(self):
        enabled = app.simulate_manual_payload(request(True))
        disabled = app.simulate_manual_payload(request(False))
        omitted = app.simulate_manual_payload(request())
        primary = app.simulate_manual_payload(request(True, fire_mode=0, cycle=False))
        incarnon = app.simulate_manual_payload(request(True, cycle=False))

        window = enabled["benchmarkTimeline"]
        self.assertGreater(window["transforms"], 0)
        self.assertEqual(enabled["targetDamage"]["hitLocation"], "body")
        self.assertEqual(enabled["targetDamage"], disabled["targetDamage"])
        self.assertEqual(disabled["benchmarkTimeline"]["transforms"], 0)
        self.assertEqual(disabled["benchmarkTimeline"], omitted["benchmarkTimeline"])
        self.assertLess(primary["benchmarkTimeline"]["windowDps"], window["windowDps"])
        self.assertLess(window["windowDps"], incarnon["benchmarkTimeline"]["windowDps"])

    def test_burston_prime_auto_incarnon_prefers_tier2_1_on_tie(self):
        for cycle in (True, False):
            with self.subTest(incarnonCycle=cycle):
                payload = {
                    **request(True, cycle=cycle), "profile": "fast", "minFill": 0, "maxFill": 0,
                    "autoIncarnon": True, "autoExilus": False, "arcanes": [],
                }
                result = app.build_weapon_route(payload)
                route = result["incarnonChoices"]
                alternate = app.build_weapon_route({
                    **payload, "autoIncarnon": False, "incarnonChoices": {**route, "tier2": "2-2"},
                })
                self.assertEqual(alternate["incarnonChoices"]["tier2"], "2-2")
                self.assertGreater(result["build"]["objectiveScore"], 0)
                self.assertEqual(result["build"]["objectiveScore"], alternate["build"]["objectiveScore"])
                self.assertEqual(result["autoLoadout"]["incarnon"]["defaultRoute"]["tier2"], "2-1")
                self.assertEqual(route["tier2"], "2-1")

    @unittest.skipUnless(native_replay.ENABLED, "the published identity requires the native extension")
    def test_engine_fingerprint_keeps_published_identity(self):
        self.assertEqual(app.ENGINE_FINGERPRINT, "535ed49f5a51149d")

    def test_headshot_trigger_still_uses_actual_hit_chance(self):
        body = request(True, aiming=True)
        scope = [{"path": ARGON_SCOPE, "rank": 5, "slot": 0}]

        def dps(payload):
            return app.simulate_manual_payload(payload)["benchmarkTimeline"]["windowDps"]

        self.assertEqual(dps(body), dps({**body, "mods": scope}))
        head = {**body, "headshotChance": 1.0}
        self.assertGreater(dps(head), dps(body))
        self.assertGreater(dps({**head, "mods": scope}), dps(head))

    def test_torid_charges_on_direct_hits_either_way(self):
        enabled = app.simulate_manual_payload(request(True, weapon=TORID))["benchmarkTimeline"]
        disabled = app.simulate_manual_payload(request(False, weapon=TORID))["benchmarkTimeline"]
        self.assertGreater(disabled["transforms"], 0)
        self.assertEqual(enabled, disabled)

    def test_non_boolean_values_are_rejected(self):
        for value in (0, 1, 1.0, "true", None, [], {}):
            with self.subTest(value=value), self.assertRaisesRegex(TypeError, rf"{FLAG} must be a boolean"):
                Scenario(combat_state={FLAG: value})

    def test_native_cycle_spec_copies_and_keeps_kill_charging(self):
        for charge_on, expected in ((None, "direct_hits"), ("direct_hits", "direct_hits"), ("kills", "kills")):
            with self.subTest(charge_on=charge_on):
                cycle = {"chargeOn": charge_on, FLAG: True}
                self.assertEqual(native_replay._cycle_spec(cycle)["chargeOn"], expected)
                self.assertEqual(cycle, {"chargeOn": charge_on, FLAG: True})

    @unittest.skipUnless(native_replay.ENABLED, "wfcore native extension is unavailable")
    def test_native_shadow_matches_exactly_without_fallback(self):
        with patch.object(native_replay, "SHADOW", True), patch.object(search_combat, "_REPLAYS", OrderedDict()):
            for weapon in (BURSTON, BOAR, BRATON, TORID):
                with self.subTest(weapon=weapon):
                    payload = {
                        **request(True, weapon=weapon), "profile": "fast", "minFill": 0,
                        "autoIncarnon": False, "autoExilus": False, "arcanes": [],
                    }
                    before = dict(native_replay.STATS)
                    _, ctx = app.build_weapon_payload(payload, return_engine_context=True, fixed_mods=[])
                    window = app.optimizer._event_window(ctx["evaluation"].stats, ctx["benchmarkContext"])
                    self.assertGreater(window["transforms"], 0)
                    self.assertGreater(native_replay.STATS["native"], before["native"])
                    self.assertEqual(native_replay.STATS["fallback"], before["fallback"])

    def test_build_trait_matrix_and_analysis_cache_isolation(self):
        payload = {
            **request(False), "profile": "fast", "windowSeconds": 12, "slotCount": 1,
            "autoIncarnon": False, "autoExilus": False, "arcanes": [],
        }
        meta = app.CATALOG_WEAPONS_BY_PATH[BURSTON]
        disposition = meta["disposition"]
        values = app.optimizer.RIVEN_BASE_VALUES[meta["rivenType"]]
        scale = app.optimizer.theoretical_structure_scale(2, False)
        analysis_payload = {
            **payload, "profile": "ultra", "disposition": disposition,
            "stats": [{"code": code, "value": values[code] * disposition * scale} for code in ("0", "1")],
            "benchmarkTarget": {
                key: payload[key] for key in ("enemy", "level", "windowSeconds", "headshotChance")
            },
        }
        cases = (
            (app.build_weapon_route, payload, "_BUILD_RESULTS", "build", "objectiveScore"),
            (app.trait_matrix_payload, payload, "_TRAIT_MATRIX_RESULTS", "matrix", "baseline_dps"),
            (app.analyze_payload, analysis_payload, "_ANALYSIS_RESULTS", "scoringValues", "baseline"),
        )
        for solve, payload, cache_name, group, metric in cases:
            with (
                self.subTest(endpoint=solve.__name__),
                patch.object(app, cache_name, OrderedDict()) as cache,
                patch.object(app, "_persist_analysis_statics_cache"),
            ):
                disabled = solve(payload)
                enabled = solve({**payload, "scenario": request(True)["scenario"]})
                cached_disabled = solve(payload)
                self.assertIs(disabled["evaluationScenario"]["combatState"][FLAG], False)
                self.assertIs(enabled["evaluationScenario"]["combatState"][FLAG], True)
                self.assertGreater(enabled[group][metric], disabled[group][metric])
                self.assertEqual(cached_disabled[group][metric], disabled[group][metric])
                self.assertEqual(len(cache), 2)
