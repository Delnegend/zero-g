import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

SCRIPT = Path(__file__).with_name("agy-observe.py")


class CollectorTest(unittest.TestCase):
    def test_model_response_after_injected_system_step(self):
        received = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                received.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                self.send_response(200)
                self.end_headers()

            def log_message(self, *_args):
                pass

        server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as directory:
                transcript = Path(directory) / "transcript.jsonl"
                transcript.write_text("\n".join(json.dumps(row) for row in [
                    {"source": "MODEL", "status": "DONE", "step_index": 0, "input_tokens": 99},
                    {"source": "SYSTEM_SDK", "status": "DONE", "step_index": 1},
                    {"source": "MODEL", "status": "DONE", "step_index": 2, "input_tokens": 12, "output_tokens": 3},
                ]) + "\n")
                event = {"transcriptPath": str(transcript), "initialNumSteps": 1,
                         "conversationId": "test", "modelName": "test", "lastUserInput": "hello"}
                result = subprocess.run(["python3", str(SCRIPT)], input=json.dumps(event), text=True,
                                        capture_output=True, check=True, timeout=8,
                                        env={**os.environ, "ZERO_G_URL": f"http://127.0.0.1:{server.server_port}",
                                             "ZERO_G_VIRTUAL_KEY": "fixture"})
                self.assertEqual(json.loads(result.stdout), {})
                self.assertEqual(len(received), 1)
                self.assertEqual((received[0]["step_index"], received[0]["input_tokens"],
                                  received[0]["output_tokens"]), (2, 12, 3))
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == "__main__":
    unittest.main()
