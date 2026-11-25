"""Phase 3b 的真实 HTTP、离线批处理和 Python 逐字段对拍。"""

import copy
import errno
import http.client
from itertools import islice
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from tests.test_phase2a_server import CONTENT_TYPE, GOLDEN, HOST, ROOT, SERVER, free_port, ready_server
from tests.test_phase2b_simulate import ARCANES, BRATON, GLAIVE, ORTHOS, STANCES, TARGET
from tools.phase0_benchmark_and_record import differences, stable_response


ANALYZE = "/api/analyze"
MATRIX = "/api/trait-matrix"
ANALYZE_IDS = {"analyze-burston-riven", "analyze-magistar-slam-bullet"}
MATRIX_IDS = {"matrix-burston-31", "matrix-burston-21", "matrix-magistar-slam-bullet"}
TIMEOUT = 300
KARAK = "/Lotus/Weapons/Grineer/LongGuns/GrineerM16Homage/GrineerM16Rifle"
BAZA_PRIME = "/Lotus/Weapons/Tenno/LongGuns/PrimeBaza/PrimeBazaGun"


def case(identifier, path, body):
    return {"id": identifier, "method": "POST", "path": path, "body": body}


def stat(code, value, negative=False):
    return {"code": code, "value": value, "negative": negative}


def live_analyze_cases():
    target = {"enemy": TARGET, "level": 78, "steelPath": True, "windowSeconds": 5.5,
              "headshotChance": 0.0, "enemyEnhancements": {"noOverguard": True, "shieldOverride": 0}}
    common = {"weapon": KARAK, "profile": "ultra", "disposition": 1.2, "benchmarkTarget": target}
    rifle = {**common, "weapon": BAZA_PRIME, "profile": "fast", "disposition": 1.9}
    positives = [stat("D", 294), stat("1", 214), stat("S", 160)]
    return [
        case("rifle-fatal-curse", ANALYZE, {**rifle, "stats": [*positives, stat("0", 213.7, True)]}),
        case("rifle-neutral-curse", ANALYZE, {**rifle, "stats": [*positives, stat("Z", 128.25, True)]}),
        case("project-source-disposition", ANALYZE, {
            **common, "sourceDisposition": 0.9,
            "rivenCard": {"rank": 5, "polarity": "AP_TACTIC"},
            "stats": [stat("0", 89.1), stat("1", 71.28)],
        }),
        case("legal-fusion", ANALYZE, {
            **common, "stats": [stat("W", 267.3), stat("1", 142.56)],
            "benchmarkTarget": {**target, "headshotChance": 0.35},
        }),
        case("melee-combo", ANALYZE, {
            **common, "weapon": ORTHOS, "stancePath": f"{STANCES}/PolearmCmbOneMeleeTree",
            "scenario": {"comboTier": 3, "combatState": {"meleeRoute": "combo", "meleeCombo": "CC_GROUND"}},
            "stats": [stat("0", 213.84), stat("1", 106.92)],
        }),
        case("kuva-innate-element", ANALYZE, {
            **common, "weapon": "/Lotus/Weapons/Grineer/KuvaLich/LongGuns/Karak/KuvaKarak",
            "innateElement": "DT_POISON", "innateElementValue": 0.47,
            "stats": [stat("0", 178), stat("1", 143)],
        }),
        case("standard-window-ranking", ANALYZE, {
            **common, "profile": "standard", "disposition": 1.3, "maxFill": 4,
            "stats": [stat("D", 201), stat("1", 146), stat("S", 109.6), stat("0", 146, True)],
        }),
        case("arcane-timeline", ANALYZE, {
            **common, "sourceDisposition": 0.9,
            "rivenCard": {"rank": 5, "polarity": "AP_TACTIC"},
            "stats": [stat("0", 89.1), stat("1", 71.28)],
            "arcanes": [{"path": f"{ARCANES}/Offensive/PrimaryDamageOnKill", "rank": 5, "triggerRate": 1.3}],
            "timelineSeconds": 4,
        }),
        case("incarnon-route-screen", ANALYZE, {
            **common, "weapon": BRATON, "profile": "fast", "maxFill": 4,
            "stats": [stat("0", 169), stat("1", 135), stat("S", 101), stat("D", 148.5, True)],
        }),
    ]


def live_matrix_cases():
    rifle = {"weapon": BRATON, "enemy": TARGET, "level": 64, "steelPath": True,
             "windowSeconds": 5.8, "enemyEnhancements": {"noOverguard": True},
             "profile": "fast", "autoIncarnon": False, "autoExilus": False, "arcanes": [], "slotCount": 4}
    automatic = {key: value for key, value in rifle.items() if key != "autoExilus"}
    return [
        *(case("rifle-" + roll, MATRIX, {**rifle, "roll": roll}) for roll in ("both", "3+1-", "2+1-")),
        case("glaive-throw", MATRIX, {
            **rifle, "weapon": GLAIVE, "scenario": {
                "heavyAttack": True, "combatState": {"meleeRoute": "glaive", "meleeTargetDistance": 17},
            },
        }),
        case("melee-combo", MATRIX, {
            **rifle, "weapon": ORTHOS, "stancePath": f"{STANCES}/PolearmCmbOneMeleeTree",
            "scenario": {"comboTier": 3, "combatState": {"meleeRoute": "combo", "meleeCombo": "CC_GROUND"}},
        }),
        case("null-exilus", MATRIX, {**automatic, "headshotChance": 1.0, "exilus": None}),
    ]


class Phase3bAnalyzeMatrixTests(unittest.TestCase):
    def requests(self, cases):
        self.assertTrue(SERVER.is_file(), f"请先构建发行版独立服务：{SERVER}")
        try:
            port = free_port()
        except OSError as error:
            if error.errno not in (errno.EACCES, errno.EPERM, errno.EAFNOSUPPORT):
                raise
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory, "requests.json")
                path.write_text(json.dumps(cases, ensure_ascii=False, allow_nan=False), encoding="utf-8")
                process = subprocess.run(
                    [str(SERVER), "--request-batch", str(path)], cwd=ROOT, check=True,
                    capture_output=True, timeout=TIMEOUT,
                )
            results = json.loads(process.stdout)
        else:
            results = []
            with ready_server([str(SERVER)], port):
                for request in cases:
                    body = (json.dumps(request["body"], ensure_ascii=False, separators=(",", ":")).encode("utf-8")
                            if "body" in request else None)
                    connection = http.client.HTTPConnection(HOST, port, timeout=TIMEOUT)
                    try:
                        connection.request(request["method"], request["path"], body=body, headers={"Connection": "close"})
                        response = connection.getresponse()
                        self.assertEqual(response.headers["Content-Type"], CONTENT_TYPE)
                        cache = "no-cache" if request["path"] == "/api/catalog" else "no-store"
                        self.assertEqual(response.headers["Cache-Control"], cache)
                        results.append({"id": request["id"], "status": response.status, "response": json.load(response)})
                    finally:
                        connection.close()
        self.assertEqual([row["id"] for row in results], [request["id"] for request in cases])
        return results

    def assert_same(self, path, expected, actual, fingerprint=None, *, recorded=False):
        expected = copy.deepcopy(expected)
        if not (recorded and path == "/api/catalog"):
            expected = stable_response(path, expected)
        actual = stable_response(path, copy.deepcopy(actual))
        if path == ANALYZE:
            self.assertEqual(actual.pop("engineFingerprint"), fingerprint)
            self.assertRegex(expected.pop("engineFingerprint"), r"\A[0-9a-f]{16}\Z")
        mismatches = list(islice(differences(expected, actual), 24))
        self.assertFalse(mismatches, "\n".join(mismatches))

    def check_golden(self, selected):
        self.assertTrue(selected)
        if any(entry["path"] == "/api/health" for entry in selected):
            batch = selected
        else:
            batch = [{"id": "health", "method": "GET", "path": "/api/health"}, *selected]
        results = self.requests([{key: value for key, value in entry.items() if key != "expected"} for entry in batch])
        health = next(row for row in results if row["id"] == "health")
        self.assertEqual(health["status"], 200)
        fingerprint = health["response"]["engineFingerprint"]
        self.assertRegex(fingerprint, r"\A[0-9a-f]{16}\Z")
        by_id = {row["id"]: row for row in results}
        for entry in selected:
            with self.subTest(case=entry["id"]):
                expected = entry["expected"]
                result = by_id[entry["id"]]
                self.assertEqual(result["status"], expected["status"], result["response"])
                if entry["path"] == "/api/health":
                    # Phase 2a 的健康指纹合同是本服务加载的数据与规则身份。
                    self.assertEqual(set(result["response"]), set(expected["response"]))
                    probe = json.loads(subprocess.check_output([str(SERVER), "--probe"], cwd=ROOT, timeout=TIMEOUT))
                    self.assertEqual(result["response"], probe)
                    self.assertEqual({key: value for key, value in result["response"].items() if key != "engineFingerprint"},
                                     {key: value for key, value in expected["response"].items() if key != "engineFingerprint"})
                else:
                    self.assert_same(entry["path"], expected["response"], result["response"], fingerprint, recorded=True)

    def test_phase0_golden_analyze(self):
        cases = [entry for entry in golden() if entry["path"] == ANALYZE]
        self.assertEqual({entry["id"] for entry in cases}, ANALYZE_IDS)
        self.check_golden(cases)

    def test_phase0_golden_trait_matrix(self):
        cases = [entry for entry in golden() if entry["path"] == MATRIX]
        self.assertEqual({entry["id"] for entry in cases}, MATRIX_IDS)
        self.check_golden(cases)

    def test_phase0_golden_all_endpoints(self):
        cases = golden()
        self.assertEqual(len(cases), 24)
        self.assertEqual({entry["path"] for entry in cases},
                         {"/api/health", "/api/catalog", "/api/simulate", "/api/build", ANALYZE, MATRIX})
        self.check_golden(cases)

    def test_live_analyze_parity(self):
        import app

        cases = live_analyze_cases()
        results = self.requests([{"id": "health", "path": "/api/health", "method": "GET"}, *cases])
        fingerprint = results[0]["response"]["engineFingerprint"]
        # Python 的工作计数按串行路径定义。进程池会改变账面计数与 memo 命中数。
        with patch.dict(os.environ, {"WF_ANALYZER_SERIAL": "1"}), app._disable_analysis_statics_persistence():
            for entry, result in zip(cases, results[1:], strict=True):
                with self.subTest(case=entry["id"]):
                    self.assertEqual(result["status"], 200, result["response"])
                    expected = json.loads(json.dumps(app.analyze_payload(entry["body"]), allow_nan=False))
                    self.assert_same(ANALYZE, expected, result["response"], fingerprint)
                    if entry["id"] == "rifle-fatal-curse":
                        assessment = result["response"]["negativeAssessment"]
                        self.assertTrue(any(row["neutral"] for row in assessment))
                        self.assertTrue(result["response"]["submittedFatalNegative"])
                    if entry["id"] == "rifle-neutral-curse":
                        self.assertFalse(result["response"]["submittedFatalNegative"])
                        self.assertIn("武器后坐力", result["response"]["recommendedNegative"])
                    if entry["id"] == "incarnon-route-screen":
                        self.assertTrue(result["response"]["incarnonRouteScreen"]["adopted"])

    def test_live_trait_matrix_parity(self):
        import app

        cases = live_matrix_cases()
        results = self.requests(cases)
        for entry, result in zip(cases, results, strict=True):
            with self.subTest(case=entry["id"]):
                self.assertEqual(result["status"], 200, result["response"])
                expected = app.trait_matrix_payload(entry["body"])
                self.assert_same(MATRIX, expected, result["response"])
        all_rolls = results[0]["response"]
        self.assertTrue(any(row["weakpoint"] for row in all_rolls["matrix"]["positive"]))
        self.assertTrue(any(row["reoptimized"] for row in all_rolls["matrix"]["positive"]))
        for result, kept, omitted in zip(results[1:3], (31, 21), (21, 31), strict=True):
            filtered = copy.deepcopy(all_rolls)
            for sign in ("positive", "negative"):
                for row in filtered["matrix"][sign]:
                    row[f"shown_{omitted}"] = row[f"pct_{omitted}"] = None
                    if row.pop("elem_build_roll", kept) != kept:
                        row["elem_build"] = None
            self.assert_same(MATRIX, filtered, result["response"])

    def test_invalid_requests(self):
        analysis = live_analyze_cases()[0]["body"]
        matrix = live_matrix_cases()[0]["body"]
        invalid_analyze = {
            "unknown-weapon": {**analysis, "weapon": "/Lotus/Unknown"},
            "bad-disposition": {**analysis, "disposition": 2.01},
            "zero-source-disposition": {**analysis, "sourceDisposition": 0},
            "bad-level": {**analysis, "benchmarkTarget": {**analysis["benchmarkTarget"], "level": -1}},
            "bad-rank": {**analysis, "rivenCard": {"rank": 9, "polarity": "AP_ATTACK"}},
            "too-few-stats": {**analysis, "stats": [stat("0", 169)]},
            "too-many-stats": {**analysis, "stats": analysis["stats"] + [stat("2", 100)]},
            "above-roll-range": {**analysis, "stats": [stat("0", 999), stat("1", 143)]},
            "negative-fusion": {**analysis, "stats": [stat("0", 178), stat("1", 143), stat("W", 100, True)]},
            "multiple-fusion": {**analysis, "stats": [stat("W", 267), stat("Y", 294)]},
            "bad-sign": {**analysis, "stats": [stat("0", 178), {"code": "1", "value": 143, "negative": 1}]},
            "non-object": [],
        }
        invalid_matrix = {
            "unknown-weapon": {**matrix, "weapon": "/Lotus/Unknown"},
            "unknown-enemy": {**matrix, "enemy": "/Lotus/Unknown"},
            "bad-level": {**matrix, "level": 1.5},
            "bad-roll": {**matrix, "roll": "3+0-"},
            "null-roll": {**matrix, "roll": None},
            "bad-slot-count": {**matrix, "slotCount": 9},
            "non-object": "body",
        }
        requests = [case(path.rsplit("/", 1)[-1] + "-" + name, path, body)
                    for path, entries in ((ANALYZE, invalid_analyze), (MATRIX, invalid_matrix))
                    for name, body in entries.items()]
        for row in self.requests(requests):
            with self.subTest(case=row["id"]):
                self.assertEqual(row["status"], 422, row["response"])
                self.assertEqual(row["response"]["error"]["code"], "invalid_request")
                self.assertTrue(row["response"]["error"]["message"])


def golden():
    return json.loads(GOLDEN.read_text(encoding="utf-8"))["cases"]
