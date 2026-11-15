# SPDX-License-Identifier: AGPL-3.0-or-later
"""通过本机 HTTP 接口复跑冰枪元素和启动烟测。"""

import json
import os
import threading
import unittest
from http.server import HTTPServer
from pathlib import Path
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from server import Handler


class TestEngine(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        token_env = patch.dict(os.environ, {"WARFRAME_ENGINE_TOKEN": "test-only"})
        token_env.start()
        cls.addClassCleanup(token_env.stop)
        http = HTTPServer(("127.0.0.1", 0), Handler)
        cls.url = f"http://127.0.0.1:{http.server_port}"
        worker = threading.Thread(target=http.serve_forever, daemon=True)
        worker.start()
        cls.addClassCleanup(http.server_close)
        cls.addClassCleanup(worker.join)
        cls.addClassCleanup(http.shutdown)

    def request(self, path, body=None, authorized=True):
        headers = {"Content-Type": "application/json"}
        if authorized:
            headers["Authorization"] = "Bearer test-only"
        data = None if body is None else json.dumps(body).encode()
        with urlopen(Request(self.url + path, data=data, headers=headers), timeout=35) as reply:
            return json.load(reply)

    def test_health(self):
        result = self.request("/healthz")
        revision = Path(__file__).with_name("UPSTREAM").read_text().splitlines()[0]
        self.assertEqual(result["data_revision"], revision)
        self.assertTrue(result["ok"])

    def test_auth_and_simulation(self):
        body = {
            "weapon": "vectis_prime",
            "enemy": "corrupted_heavy_gunner",
            "duration": 1,
            "runs": 1,
            "seed": 1,
        }
        with self.assertRaises(HTTPError) as denied:
            self.request("/api/simulate", body, authorized=False)
        self.assertEqual(denied.exception.code, 401)
        denied.exception.close()
        result = self.request("/api/simulate", body)
        self.assertTrue(result["ok"])
        self.assertEqual(result["runs"], 1)

    def test_toxin_glaxion_violet_shard(self):
        cases = (
            (None, False, {"Viral": "141"}),
            (False, False, {"Magnetic": "146", "Toxin": "50.5"}),
            (True, False, {"Magnetic": "174", "Toxin": "50.5"}),
            (True, True, {"Blast": "174", "Corrosive": "135"}),
        )
        for tau, heat, expected in cases:
            with self.subTest(tau=tau, heat=heat):
                body = {
                    "weapon": "tenet_glaxion",
                    "valence_element": "toxin",
                    "valence_bonus": 0.56,
                    "mods": ["serration", "thermite_rounds"] if heat else ["serration"],
                }
                if tau is not None:
                    body["wielder"] = {
                        "frame": "wukong_prime",
                        "shards": [
                            {
                                "shard": "violet_archon_shard",
                                "effect": "primary_electricity_damage",
                                "tauforged": tau,
                            }
                        ],
                    }
                result = self.request("/api/panel", body)
                self.assertTrue(result["ok"], result)
                damage = result["forms"][0]["parts"][0]["damage"]
                self.assertEqual({row["type"]: row["amount"] for row in damage}, expected)
