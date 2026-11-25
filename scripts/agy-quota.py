#!/usr/bin/env python3
"""Push the official agy CLI's own quota report to Zero-G.

Runs `agy --print /usage --output-format json` (no model call, no tokens) with the login
agy already has, then posts the result to POST /v1/agy/quota. Zero-G never contacts Google
for Antigravity quota. Schedule this every 10 to 15 minutes on the machine that runs agy.

Environment:
  ZERO_G_URL           Gateway root, without /v1.
  ZERO_G_VIRTUAL_KEY   Virtual key without a provider allowlist.
  ZERO_G_AGY_ACCOUNT   Name of the Google Antigravity credential in Zero-G for this login.
  AGY_BIN              Optional path to the agy executable (default: agy).
"""

import json
import os
import subprocess
import sys
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def fail(message):
    print(f"agy-quota: {message}", file=sys.stderr)
    sys.exit(1)


def main():
    base = os.environ.get("ZERO_G_URL", "").rstrip("/")
    key = os.environ.get("ZERO_G_VIRTUAL_KEY", "")
    account = os.environ.get("ZERO_G_AGY_ACCOUNT", "")
    if not base or not key or not account:
        fail("set ZERO_G_URL, ZERO_G_VIRTUAL_KEY and ZERO_G_AGY_ACCOUNT")
    try:
        done = subprocess.run(
            [os.environ.get("AGY_BIN", "agy"), "--print", "/usage", "--output-format", "json"],
            capture_output=True, text=True, timeout=60, check=True,
        )
        data = json.loads(done.stdout)["command"]
        report = data["data"]
        if data.get("name") != "usage" or not isinstance(report.get("groups"), list):
            raise ValueError("unexpected /usage output")
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
        fail(f"could not read quota from agy: {error}")
    request = urllib.request.Request(
        base + "/v1/agy/quota",
        data=json.dumps({"account": account, "report": report}).encode("utf-8"),
        headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.build_opener(NoRedirect).open(request, timeout=10):
            pass
    except OSError as error:
        fail(f"Zero-G did not accept the report: {error}")


main()
