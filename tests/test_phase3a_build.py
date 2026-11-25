"""逐字段验收独立 Rust 配卡，同时覆盖 HTTP 和离线批处理入口。"""

import errno
import http.client
from itertools import islice
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from tests.test_phase2a_server import (
    CONTENT_TYPE, GOLDEN, HOST, ROOT, SERVER, free_port, ready_server,
)
from tests.test_phase2b_simulate import ARCANES, BOAR, BRATON, BURSTON, GLAIVE, MODS, ORTHOS, STANCES, TARGET
from tools.phase0_benchmark_and_record import differences, stable_response


BUILD = "/api/build"
GOLDEN_IDS = (
    "build-burston-auto-tie",
    "build-burston-fixed-tie",
    "build-burston-riven",
    "build-magistar-slam-bullet",
)
TIMEOUT = 300


def live_cases():
    common = {"enemy": TARGET, "level": 57, "steelPath": True, "profile": "fast",
              "windowSeconds": 4.7, "enemyEnhancements": {"noOverguard": True}}
    return [
        {"id": "rifle-all-slots", "body": {**common, "weapon": BRATON,
            "autoExilus": True, "minFill": 8}},
        {"id": "shotgun-all-slots", "body": {**common, "weapon": BOAR, "level": 82,
            "autoExilus": True, "minFill": 8}},
        {"id": "rifle-riven-and-arcane", "body": {**common, "weapon": BRATON, "slotCount": 4,
            "riven": {"rank": 6, "polarity": "AP_DEFENSE", "stats": [
                {"code": "0", "value": 110}, {"code": "6", "value": 64},
            ]},
            "arcanes": [{"path": f"{ARCANES}/Offensive/PrimaryDamageOnKill",
                "rank": 4, "stacks": 5, "uptime": 0.6}]}},
        {"id": "required-and-excluded", "body": {**common, "weapon": BRATON, "slotCount": 4,
            "profile": "standard", "requiredMods": [f"{MODS}/Rifle/WeaponDamageAmountMod"],
            "forbiddenMods": [f"{MODS}/Rifle/Expert/WeaponFreezeDamageModExpert"],
            "minFill": 3, "maxFill": 3, "elementTarget": "viral"}},
        {"id": "polearm-combo", "body": {**common, "weapon": ORTHOS, "slotCount": 4,
            "stancePath": f"{STANCES}/PolearmCmbOneMeleeTree", "comboTier": 3,
            "combatState": {"meleeRoute": "combo", "meleeCombo": "CC_GROUND"}}},
        {"id": "glaive-throw", "body": {**common, "weapon": GLAIVE, "slotCount": 4,
            "scenario": {"heavyAttack": True,
                "combatState": {"meleeRoute": "glaive", "meleeTargetDistance": 17}}}},
        {"id": "auto-weapon-arcane", "body": {**common, "weapon": BRATON, "level": 93,
            "windowSeconds": 5.3, "headshotChance": 0.27, "enemyEnhancements": {},
            "slotCount": 3, "autoArcane": True}},
        {"id": "deep-shotgun", "body": {**common, "weapon": BOAR, "level": 93,
            "windowSeconds": 5.3, "headshotChance": 0.27, "enemyEnhancements": {},
            "profile": "deep", "slotCount": 3}},
        {"id": "panel-heavy", "body": {**common, "weapon": ORTHOS, "level": 93,
            "windowSeconds": 5.3, "headshotChance": 0.27, "enemyEnhancements": {},
            "metric": "heavyDamage", "slotCount": 4}},
        {"id": "fixed-exilus", "body": {**common, "weapon": BRATON, "level": 93,
            "windowSeconds": 5.3, "headshotChance": 0.27, "enemyEnhancements": {}, "slotCount": 5,
            "exilus": {"path": f"{MODS}/Sets/Vigilante/PrimaryVigilanteSuppliesMod", "rank": 2}}},
        {"id": "incarnon-tie-prefers-current", "body": {**common, "weapon": BURSTON, "fireMode": 1,
            "level": 155, "windowSeconds": 12, "minFill": 0, "maxFill": 0,
            "autoIncarnon": True, "autoExilus": False,
            "incarnonChoices": {"tier2": "2-2", "tier3": "3-1", "tier4": "4-1"},
            "scenario": {"incarnonCycle": True, "combatState": {"gaugeChargingHeadshots": True}}}},
        {"id": "latron-double-tap", "body": {**common, "slotCount": 5, "level": 93,
            "windowSeconds": 5.3, "headshotChance": 0.27, "enemyEnhancements": {},
            "weapon": "/Lotus/Weapons/Tenno/Rifle/LatronPrime"}},
        {"id": "incarnon-and-arcane", "body": {**common, "weapon": BURSTON, "fireMode": 1,
            "slotCount": 3, "level": 126, "windowSeconds": 4.2, "headshotChance": 0.25,
            "enemyEnhancements": {}, "minFill": 0, "maxFill": 0,
            "autoIncarnon": True, "autoArcane": True, "autoExilus": False,
            "scenario": {"incarnonCycle": True, "combatState": {"gaugeChargingHeadshots": True}}}},
        {"id": "kuva-innate-element", "body": {**common, "slotCount": 3, "level": 70,
            "weapon": "/Lotus/Weapons/Grineer/KuvaLich/LongGuns/Karak/KuvaKarak",
            "innateElement": "DT_POISON", "innateElementValue": 0.47}},
        {"id": "external-buffs", "body": {**common, "weapon": BRATON, "slotCount": 3,
            "externalBuffs": [{"id": "roar", "strength": 1.5}, {"id": "corrosive-projection", "count": 2},
                {"id": "coaction-drift"}, {"id": "fireball-frenzy", "value": 0.82}]}},
        {"id": "assembled-zaw-combo", "body": {**common, "slotCount": 3, "level": 70,
            "windowSeconds": 4.2, "headshotChance": 0.25, "enemyEnhancements": {},
            "weapon": "/Lotus/Weapons/Ostron/Melee/LotusModularWeapon",
            "kitgunParts": ["/Lotus/Weapons/Ostron/Melee/ModularMelee01/Tip/TipOne",
                "/Lotus/Weapons/Ostron/Melee/ModularMelee01/Handle/HandleFive",
                "/Lotus/Weapons/Ostron/Melee/ModularMelee01/Balance/BalanceSpeedI"]}},
        {"id": "projectile-preview", "body": {**common, "slotCount": 3,
            "weapon": "/Lotus/Weapons/Grineer/Bows/GrnBow/GrnBowWeapon", "metric": "shotDamage", "autoExilus": False}},
    ]


class Phase3aBuildTests(unittest.TestCase):
    def build(self, cases):
        self.assertTrue(SERVER.is_file(), f"请先构建发行版独立服务：{SERVER}")
        try:
            port = free_port()
        except OSError as error:
            if error.errno not in (errno.EACCES, errno.EPERM, errno.EAFNOSUPPORT):
                raise
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory, "build.json")
                path.write_text(json.dumps(cases, ensure_ascii=False), encoding="utf-8")
                process = subprocess.run(
                    [str(SERVER), "--build-batch", str(path)], cwd=ROOT, check=True,
                    capture_output=True, timeout=TIMEOUT,
                )
            return json.loads(process.stdout)

        results = []
        with ready_server([str(SERVER)], port):
            for case in cases:
                body = json.dumps(case["body"], ensure_ascii=False, separators=(",", ":")).encode("utf-8")
                connection = http.client.HTTPConnection(HOST, port, timeout=TIMEOUT)
                try:
                    connection.request("POST", BUILD, body=body, headers={"Connection": "close"})
                    response = connection.getresponse()
                    self.assertEqual(response.headers["Content-Type"], CONTENT_TYPE)
                    self.assertEqual(response.headers["Cache-Control"], "no-store")
                    results.append({"id": case["id"], "status": response.status, "response": json.load(response)})
                finally:
                    connection.close()
        return results

    def assert_same(self, expected, actual):
        mismatches = list(islice(differences(stable_response(BUILD, expected), stable_response(BUILD, actual)), 24))
        self.assertFalse(mismatches, "\n".join(mismatches))

    def test_phase0_golden(self):
        expected = {
            case["id"]: case for case in json.loads(GOLDEN.read_text(encoding="utf-8"))["cases"]
            if case["path"] == BUILD
        }
        self.assertEqual(set(expected), set(GOLDEN_IDS))
        results = self.build([{"id": key, "body": expected[key]["body"]} for key in GOLDEN_IDS])
        self.assertEqual([row["id"] for row in results], list(GOLDEN_IDS))
        for row in results:
            with self.subTest(case=row["id"]):
                reference = expected[row["id"]]["expected"]
                self.assertEqual(row["status"], reference["status"], row["response"])
                self.assert_same(reference["response"], row["response"])

    def test_live_build_parity(self):
        import app

        cases = live_cases()
        results = self.build(cases)
        self.assertEqual([row["id"] for row in results], [case["id"] for case in cases])
        for case, result in zip(cases, results):
            with self.subTest(case=case["id"]):
                self.assertEqual(result["status"], 200, result["response"])
                expected = json.loads(json.dumps(app.build_weapon_route(case["body"]), allow_nan=False))
                self.assert_same(expected, result["response"])
                if case["id"].endswith("all-slots"):
                    self.assertEqual(len(result["response"]["build"]["mods"]), 8)
                if case["id"] == "incarnon-tie-prefers-current":
                    self.assertEqual(result["response"]["incarnonChoices"], case["body"]["incarnonChoices"])

    def test_request_aliases(self):
        import app

        body = {"weapon": BRATON, "enemy": TARGET, "level": 67, "windowSeconds": 3.1,
                "slotCount": 3, "maxFill": 2, "profile": "full",
                "pinnedMods": [f"{MODS}/Rifle/WeaponDamageAmountMod"],
                "excludedMods": [f"{MODS}/Rifle/Expert/WeaponFreezeDamageModExpert"]}
        [result] = self.build([{"id": "aliases", "body": body}])
        self.assertEqual(result["status"], 200, result["response"])
        reference = {key: value for key, value in body.items() if key not in ("pinnedMods", "excludedMods")}
        reference.update(profile="deep", requiredMods=body["pinnedMods"], forbiddenMods=body["excludedMods"])
        expected = json.loads(json.dumps(app.build_weapon_route(reference), allow_nan=False))
        self.assert_same(expected, result["response"])

    def test_invalid_requests(self):
        valid = {"weapon": BRATON, "enemy": TARGET}
        invalid = {
            "weapon": {**valid, "weapon": "/Lotus/Unknown"},
            "missing-weapon": {"enemy": TARGET},
            "enemy": {**valid, "enemy": "/Lotus/Unknown"},
            "slot-count-low": {**valid, "slotCount": 0},
            "slot-count-high": {**valid, "slotCount": 9},
            "slot-count-bool": {**valid, "slotCount": True},
            "min-fill": {**valid, "minFill": -1},
            "max-fill": {**valid, "maxFill": 9},
            "fill-order": {**valid, "minFill": 4, "maxFill": 3},
            "profile": {**valid, "profile": "unknown"},
            "level": {**valid, "level": 1.5},
            "negative-level": {**valid, "level": -1},
            "riven-object": {**valid, "riven": []},
            "riven-rank": {**valid, "riven": {"rank": 9, "stats": [{"code": "0", "value": 10}]}},
            "riven-sign": {**valid, "riven": {"stats": [{"code": "0", "negative": 1, "value": 10}]}},
            "riven-value": {**valid, "riven": {"stats": [{"code": "0", "value": -10}]}},
            "required-unknown": {**valid, "requiredMods": ["/Lotus/Unknown"]},
            "duplicate-constraint": {**valid, "requiredMods": [f"{MODS}/Rifle/WeaponDamageAmountMod"],
                "excludedMods": [f"{MODS}/Rifle/WeaponDamageAmountMod"]},
            "body": [],
        }
        results = self.build([{"id": key, "body": body} for key, body in invalid.items()])
        self.assertEqual([row["id"] for row in results], list(invalid))
        for row in results:
            with self.subTest(case=row["id"]):
                self.assertEqual(row["status"], 422, row["response"])
                self.assertEqual(set(row["response"]), {"error"})
                error = row["response"]["error"]
                self.assertEqual(set(error), {"code", "message"})
                self.assertEqual(error["code"], "invalid_request")
                self.assertIsInstance(error["message"], str)
                self.assertTrue(error["message"].strip())
