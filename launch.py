import argparse
import json
import multiprocessing
import os
from http import HTTPStatus
from http.server import ThreadingHTTPServer
from pathlib import Path

os.environ["WF_ENGINE_WORKERS"] = "1"
os.environ["WF_ANALYSIS_STATICS_CACHE_PATH"] = str(Path(__file__).with_name("analysis-statics.json"))
import app
from wfarsenal import native_replay

FINGERPRINT = "85ec9cdb3461c01c"
BUILD = "2026.08.13.13.24"


def health():
    if app.ANALYSIS_STATICS_RUNTIME_INPUTS != FINGERPRINT or not native_replay.ENABLED:
        raise RuntimeError("三靶运行包输入指纹不符或缺少 Rust 原生模块")
    if app.CATALOG["buildLabel"] != BUILD:
        raise RuntimeError("三靶运行包数据版本不符")
    return {"ok": True, "buildLabel": BUILD, "engineFingerprint": FINGERPRINT, "schemaVersion": 3}


class PinnedHandler(app.PortableHandler):
    def do_GET(self):
        if self.path == "/api/health":
            self._send_json(HTTPStatus.OK, health())
        else:
            super().do_GET()


if __name__ == "__main__":
    multiprocessing.freeze_support()
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", action="store_true")
    parser.add_argument("--port", type=int, default=0)
    args = parser.parse_args()
    info = health()
    if args.probe:
        print(json.dumps(info))
    else:
        app._disable_analysis_statics_persistence()
        with ThreadingHTTPServer(("127.0.0.1", args.port), PinnedHandler) as server:
            server.serve_forever(poll_interval=0.2)
