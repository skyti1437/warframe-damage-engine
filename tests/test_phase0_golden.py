"""通过真实发布进程验证 Phase 0 黄金夹具。"""

import unittest

from tools.phase0_benchmark_and_record import verify_golden


class Phase0GoldenTests(unittest.TestCase):
    def test_phase0_golden_over_http(self):
        failures = verify_golden()
        self.assertFalse(failures, "\n".join(failures))


if __name__ == "__main__":
    unittest.main()
