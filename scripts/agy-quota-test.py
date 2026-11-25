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


class QuotaPushTest(unittest.TestCase):
    def run_script(self, agy_output, status=200):
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
                agy.write_text("#!/bin/sh\ncat <<'EOF'\n" + agy_output + "\nEOF\n")
                agy.chmod(agy.stat().st_mode | stat.S_IXUSR)
                result = subprocess.run(
                    ["python3", str(SCRIPT)], capture_output=True, text=True, timeout=20,
                    env={**os.environ, "AGY_BIN": str(agy),
                         "ZERO_G_URL": f"http://127.0.0.1:{server.server_port}",
                         "ZERO_G_VIRTUAL_KEY": "zg-test", "ZERO_G_AGY_ACCOUNT": "Kien"},
                )
        finally:
            server.shutdown()
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

    def test_missing_settings_fail(self):
        env = {k: v for k, v in os.environ.items() if not k.startswith("ZERO_G_")}
        result = subprocess.run(["python3", str(SCRIPT)], capture_output=True, text=True,
                                env=env, timeout=10)
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
