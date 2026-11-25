import http.server
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import threading
import unittest

SCRIPT = Path(__file__).with_name("agy-quota.py")
USAGE = {"command": {"name": "usage", "data": {"groups": [
    {"name": "Gemini Models", "buckets": [
        {"id": "gemini-5h", "window": "5h", "remaining_fraction": 0.5,
         "reset_time": "2026-11-25T10:30:13Z"}]}]}}}


FAKE_AGY = """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["FAKE_AGY_LOG"], "a") as log:
    log.write(" ".join(sys.argv[1:]) + "\\n")
if sys.argv[1:2] == ["acp"]:
    if os.environ.get("FAKE_ACP") == "broken":
        sys.exit(1)
    for line in sys.stdin:
        request = json.loads(line)
        result = {}
        if request["method"] == "session/new":
            result = {"sessionId": "s1", "_meta": {"antigravity": {"tier": {
                "name": "Google AI Pro", "id": "g1-pro-tier"}}}}
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
else:
    print(os.environ["FAKE_USAGE"])
"""


class QuotaPushTest(unittest.TestCase):
    def run_script(self, agy_output, status=200, acp="ok", runs=1, cache=None):
        received = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                received.append((self.path, self.headers["Authorization"],
                                 json.loads(self.rfile.read(int(self.headers["Content-Length"])))))
                self.send_response(status)
                self.end_headers()

            def log_message(self, *_args):
                pass

        server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            with tempfile.TemporaryDirectory() as directory:
                agy = Path(directory) / "agy"
                agy.write_text(FAKE_AGY)
                agy.chmod(agy.stat().st_mode | stat.S_IXUSR)
                log = Path(directory) / "agy.log"
                env = {**os.environ, "AGY_BIN": str(agy), "FAKE_AGY_LOG": str(log),
                       "FAKE_USAGE": agy_output, "FAKE_ACP": acp,
                       "ZERO_G_CACHE_DIR": cache or str(Path(directory) / "cache"),
                       "ZERO_G_URL": f"http://127.0.0.1:{server.server_port}",
                       "ZERO_G_VIRTUAL_KEY": "zg-test", "ZERO_G_AGY_ACCOUNT": "Kien"}
                for _ in range(runs):
                    result = subprocess.run(["python3", str(SCRIPT)], capture_output=True,
                                            text=True, timeout=30, env=env)
                self.agy_calls = log.read_text().splitlines() if log.exists() else []
        finally:
            server.shutdown()
            server.server_close()
        return result, received

    def test_pushes_the_usage_report_for_the_named_account(self):
        result, received = self.run_script(json.dumps(USAGE))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(received), 1)
        path, auth, body = received[0]
        self.assertEqual(path, "/v1/agy/quota")
        self.assertEqual(auth, "Bearer zg-test")
        self.assertEqual(body["account"], "Kien")
        self.assertEqual(body["report"]["groups"][0]["name"], "Gemini Models")

    def test_unexpected_agy_output_sends_nothing_and_fails(self):
        result, received = self.run_script('{"response": "hello"}')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(received, [])

    def test_rejected_report_fails_loudly(self):
        result, _ = self.run_script(json.dumps(USAGE), status=404)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not accept", result.stderr)

    def test_sends_the_subscription_tier_and_reads_it_once_a_day(self):
        result, received = self.run_script(json.dumps(USAGE),
                                           runs=2)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(received), 2)
        for _, _, body in received:
            self.assertEqual(body["tier"], {"name": "Google AI Pro", "id": "g1-pro-tier"})
        # Two pushes, but the throwaway ACP session is opened only once.
        self.assertEqual(self.agy_calls.count("acp"), 1)
        self.assertEqual(sum(call.startswith("--print") for call in self.agy_calls), 2)

    def test_unreadable_tier_still_pushes_the_quota(self):
        result, received = self.run_script(json.dumps(USAGE), acp="broken")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(received), 1)
        self.assertNotIn("tier", received[0][2])

    def test_missing_settings_fail(self):
        env = {k: v for k, v in os.environ.items() if not k.startswith("ZERO_G_")}
        result = subprocess.run(["python3", str(SCRIPT)], capture_output=True, text=True,
                                env=env, timeout=10)
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
