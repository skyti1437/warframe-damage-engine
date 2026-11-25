"""逐字段验收纯 Rust 远程模拟，以及 HTTP 与批处理共用的校验。"""

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
        by_id = {case["id"]: case for case in json.loads(GOLDEN.read_text(encoding="utf-8"))["cases"]}
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
