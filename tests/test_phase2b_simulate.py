"""逐字段验收纯 Rust 模拟，以及 HTTP 与批处理共用的校验。"""

import errno
from itertools import islice
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

from tests.test_phase2a_server import (
    CONTENT_TYPE, GOLDEN, ROOT, SERVER, TIMEOUT, free_port, ready_server, request,
)
from tools.phase0_benchmark_and_record import differences


BRATON = "/Lotus/Weapons/Tenno/Rifle/BratonPrime"
ORTHOS = "/Lotus/Weapons/Tenno/Melee/Polearms/PrimePolearmWeapon"
FRAGOR = "/Lotus/Weapons/Tenno/Melee/PrimeFragor/PrimeFragor"
LEX = "/Lotus/Weapons/Tenno/Pistols/PrimeLex/PrimeLex"
BURSTON = "/Lotus/Weapons/Tenno/LongGuns/PrimeBurston/PrimeBurston"
ARCANES = "/Lotus/Upgrades/CosmeticEnhancers"
GLAIVE = "/Lotus/Weapons/Tenno/Melee/Glaives/LightGlaive/LightGlaiveWeapon"
MODS = "/Lotus/Upgrades/Mods"
STANCES = "/Lotus/Weapons/Tenno/Melee/MeleeTrees"
MERCILESS = f"{ARCANES}/Offensive/PrimaryDamageOnKill"
BOAR = "/Lotus/Weapons/Tenno/Shotgun/PrimeBoar"
TARGET = "/Lotus/Types/Enemies/Duviri/Avatars/DuviriMeleeAvatar"
SIMULATE = "/api/simulate"
GOLDEN_IDS = (
    "simulate-braton-standard",
    "simulate-burston-gauge-on",
    "simulate-burston-gauge-off",
    "simulate-torid-cycle",
    "simulate-zymos-head",
    "simulate-braton-overguard",
    "simulate-braton-shield",
    "simulate-braton-lv9999",
    "simulate-magistar-light",
    "simulate-magistar-heavy",
    "simulate-magistar-slam-bullet",
    "simulate-glaive-detonation",
    "simulate-burston-loadout",
)


class Phase2bSimulateTests(unittest.TestCase):
    def simulate(self, cases):
        self.assertTrue(SERVER.is_file(), f"请先构建发行版独立服务：{SERVER}")
        try:
            port = free_port()
        except OSError as error:
            if error.errno not in (errno.EACCES, errno.EPERM, errno.EAFNOSUPPORT):
                raise
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory, "simulate.json")
                path.write_text(json.dumps(cases, ensure_ascii=False), encoding="utf-8")
                process = subprocess.run(
                    [str(SERVER), "--simulate-batch", str(path)], cwd=ROOT,
                    check=True, capture_output=True, timeout=TIMEOUT,
                )
            return json.loads(process.stdout)

        results = []
        with ready_server([str(SERVER)], port):
            for case in cases:
                body = json.dumps(case["body"]).encode("utf-8")
                status, headers, response = request(port, "POST", SIMULATE, body=body, length=str(len(body)))
                self.assertEqual(headers["Content-Type"], CONTENT_TYPE)
                self.assertEqual(headers["Cache-Control"], "no-store")
                results.append({"id": case["id"], "status": status, "response": response})
        return results

    def assert_same(self, expected, actual):
        # 黄金和实时响应只剥离与数据目录位置绑定的字段。
        expected = {key: value for key, value in expected.items() if key != "exportDir"}
        actual = {key: value for key, value in actual.items() if key != "exportDir"}
        mismatches = list(islice(differences(expected, actual), 12))
        self.assertFalse(mismatches, "\n".join(mismatches))

    def test_phase0_golden(self):
        by_id = {
            case["id"]: case
            for case in json.loads(GOLDEN.read_text(encoding="utf-8"))["cases"]
            if case["path"] == SIMULATE
        }
        self.assertEqual(set(GOLDEN_IDS), set(by_id))
        cases = [{"id": key, "body": by_id[key]["body"]} for key in GOLDEN_IDS]
        results = self.simulate(cases)
        self.assertEqual([row["id"] for row in results], list(GOLDEN_IDS))
        for row in results:
            with self.subTest(case=row["id"]):
                expected = by_id[row["id"]]["expected"]
                self.assertEqual(row["status"], expected["status"], row["response"].get("error"))
                self.assert_same(expected["response"], row["response"])

    def test_live_ranged_parity(self):
        import app

        cases = [
            {"id": "boar", "body": {
                "weapon": BOAR, "enemy": TARGET, "level": 170, "steelPath": True,
                "windowSeconds": 12, "enemyEnhancements": {"noOverguard": True},
            }},
            {"id": "strip-and-mixed-aim", "body": {
                "weapon": BRATON, "enemy": TARGET, "level": 72, "armourStrip": 0.47,
                "elementalArmourStrip": 0.15, "headshotChance": 0.35, "windowSeconds": 7.1,
                "combatState": {"channeling": True},
            }},
            {"id": "panel", "body": {
                "weapon": BRATON, "damageModel": "panel", "windowSeconds": 2,
            }},
            {"id": "custom", "body": {
                "weapon": BRATON, "damageModel": "custom", "level": 24,
                "headshotChance": 0.4, "windowSeconds": 8.1,
                "defenseSnapshot": {"health": 215, "shield": 300, "armour": 140,
                    "baseLevel": 4, "healthType": "RK_FLESH", "armourType": "RK_ARMOR",
                    "shieldType": "RK_SHIELD", "factionResistance": "RK_CORPUS_FACTION"},
            }},
        ]
        self.assert_live_parity(app, cases)

    def test_live_melee_and_loadout_parity(self):
        import app

        common = {"enemy": TARGET, "level": 44, "windowSeconds": 5.8}
        cases = [
            {"id": "orthos-stance-and-combo-tier", "body": {**common,
                "weapon": ORTHOS, "stancePath": f"{STANCES}/PolearmCmbOneMeleeTree",
                "comboTier": 5,
                "combatState": {"meleeRoute": "combo", "meleeCombo": "CC_GROUND"},
                "mods": [
                    {"path": f"{MODS}/Melee/Event/ComboCritChanceMod", "rank": 8, "slot": 0},
                    {"path": f"{MODS}/Melee/WeaponMeleeDamageMod", "rank": 4, "slot": 2},
                    {"path": f"{MODS}/Melee/WeaponFireRateMod", "rank": 3, "slot": 5},
                ],
            }},
            {"id": "fragor-regular-jump-slam", "body": {**common,
                "weapon": FRAGOR, "stancePath": f"{STANCES}/HammerCmbTwoMeleeTree",
                "scenario": {"heavyAttack": True, "combatState": {"meleeRoute": "slam"}},
                "mods": [
                    {"path": f"{MODS}/Melee/WeaponMeleeHeavyDamageMod", "rank": 3},
                    {"path": f"{MODS}/Melee/WeaponMeleeChargeRateMod", "rank": 4},
                    {"path": f"{MODS}/Melee/WeaponMeleeSlamDamageMod", "rank": 2},
                ],
            }},
            {"id": "glaive-flight-and-heavy-efficiency", "body": {**common,
                "weapon": GLAIVE,
                "scenario": {"heavyAttack": True, "combatState": {"meleeRoute": "glaive", "meleeTargetDistance": 17}},
                "exilus": {"path": f"{MODS}/Melee/WeaponGlaiveSpeedMod", "rank": 2},
                "mods": [{"path": f"{MODS}/Melee/WeaponGlaivePowerthrowMod", "rank": 3}],
                "riven": {"rank": 6, "stats": [{"code": "U", "value": 62}, {"code": "B", "value": 42}]},
            }},
            {"id": "braton-full-loadout", "body": {**common,
                "weapon": BRATON, "slotCount": 4, "slotPolarities": ["AP_ATTACK", "AP_DEFENSE", None, "AP_ANY"],
                "specialSlotPolarities": {"exilus": "AP_TACTIC"},
                "mods": [
                    {"path": f"{MODS}/Rifle/WeaponDamageAmountMod", "rank": 7, "slot": 0},
                    {"path": f"{MODS}/Rifle/DualStat/PoisonEventRifleMod", "rank": 2, "slot": 2},
                    {"path": f"{MODS}/Rifle/DualStat/IceEventRifleMod", "rank": 1, "slot": 3},
                ],
                "riven": {"rank": 6, "polarity": "AP_ATTACK", "slot": 1, "stats": [
                    {"code": "4", "value": 82}, {"code": "GA", "value": 38},
                    {"code": "0", "value": 72}, {"code": "F", "value": 18, "negative": True},
                ]},
                "arcanes": [{"path": MERCILESS, "rank": 3}],
                "exilus": {"path": f"{MODS}/Sets/Vigilante/PrimaryVigilanteSuppliesMod", "rank": 3},
            }},
            {"id": "braton-fixed-arcane-and-kill-stack-mod", "body": {**common,
                "weapon": BRATON, "level": 9, "windowSeconds": 9,
                "arcanes": [{"path": MERCILESS, "rank": 4, "stacks": 5, "uptime": 0.6}],
                "mods": [{"path": f"{MODS}/Rifle/WeaponStatusChanceSPMod", "rank": 8}],
            }},
            {"id": "burston-incarnon-burst-and-reload-triggers", "body": {**common,
                "weapon": BURSTON,
                "incarnonChoices": {"tier2": "2-2", "tier3": "3-2", "tier4": "4-1"},
            }},
            {"id": "fragor-sacrificial-set", "body": {**common,
                "weapon": FRAGOR,
                "mods": [
                    {"path": f"{MODS}/Sets/Sacrifice/MeleeSacrificeModA", "rank": 7},
                    {"path": f"{MODS}/Sets/Sacrifice/MeleeSacrificeModB", "rank": 8},
                ],
            }},
            {"id": "braton-deadhead-mixed-aim", "body": {**common,
                "weapon": BRATON, "level": 5, "windowSeconds": 9, "headshotChance": 0.55,
                "arcanes": [{"path": f"{ARCANES}/Offensive/PrimaryDamageOnNoMelee", "rank": 3}],
                "mods": [{"path": f"{MODS}/Rifle/WeaponDamageAmountMod", "rank": 10}],
            }},
            {"id": "lex-cryogenic-puncture", "body": {**common,
                "weapon": LEX,
                "arcanes": [{"path": f"{ARCANES}/Offensive/SecondaryColdOnPuncture", "rank": 3}],
            }},
            {"id": "lex-enervate-fixed-state", "body": {**common,
                "weapon": LEX,
                "arcanes": [{"path": f"{ARCANES}/Offensive/SecondaryCritOnHit", "rank": 4, "stacks": 7, "uptime": 0.6}],
            }},
        ]
        self.assert_live_parity(app, cases)

    def assert_live_parity(self, app, cases):
        results = self.simulate(cases)
        self.assertEqual([row["id"] for row in results], [case["id"] for case in cases])
        for case, row in zip(cases, results):
            with self.subTest(case=case["id"]):
                self.assertEqual(row["status"], 200, row["response"].get("error"))
                expected = json.loads(json.dumps(app.simulate_manual_payload(case["body"]), allow_nan=False))
                self.assert_same(expected, row["response"])

    def test_invalid_requests(self):
        valid = {"weapon": BRATON, "enemy": TARGET}
        invalid = {
            "weapon": {**valid, "weapon": "/Lotus/Unknown"},
            "missing-weapon": {"enemy": TARGET},
            "enemy": {**valid, "enemy": "/Lotus/Unknown"},
            "missing-enemy": {"weapon": BRATON},
            "gauge": {**valid, "scenario": {"combatState": {"gaugeChargingHeadshots": 1}}},
            "headshot": {**valid, "headshotChance": 1.5},
            "window": {**valid, "windowSeconds": 0},
            "level": {**valid, "level": 1.5},
            "negative-level": {**valid, "level": -1},
            "trigger-policy": {**valid, "scenario": {"triggerPolicy": "unknown"}},
            "mod-rank": {**valid, "mods": [{"path": f"{MODS}/Rifle/WeaponDamageAmountMod", "rank": 11}]},
            "mod-path": {**valid, "mods": [{"path": "/Lotus/Unknown"}]},
            "mod-slot": {**valid, "mods": [{"path": f"{MODS}/Rifle/WeaponDamageAmountMod", "slot": 8}]},
            "slot-polarities": {**valid, "slotPolarities": [None]},
            "arcane-uptime": {**valid, "arcanes": [{"path": MERCILESS, "uptime": 1.2}]},
            "arcane-rate": {**valid, "arcanes": [{"path": MERCILESS, "triggerRate": 5}]},
            "melee-route": {**valid, "weapon": ORTHOS, "combatState": {"meleeRoute": "unknown"}},
            "body": [],
        }
        cases = [{"id": key, "body": body} for key, body in invalid.items()]
        results = self.simulate(cases)
        self.assertEqual([row["id"] for row in results], list(invalid))
        for row in results:
            with self.subTest(case=row["id"]):
                self.assertEqual(row["status"], 422)
                self.assertEqual(set(row["response"]), {"error"})
                error = row["response"]["error"]
                self.assertEqual(set(error), {"code", "message"})
                self.assertEqual(error["code"], "invalid_request")
                self.assertIsInstance(error["message"], str)
                self.assertTrue(error["message"].strip())
