#!/usr/bin/env python3
"""Push the official agy CLI's own quota report to Zero-G.

Runs `agy --print /usage --output-format json` (no model call, no tokens) with the login
agy already has, then posts the result to POST /v1/agy/quota. Zero-G never contacts Google
for Antigravity quota. Schedule this every 10 to 15 minutes on the machine that runs agy.

The account's subscription tier (for example "Google AI Pro") comes from the agy ACP server's
session/new reply. That leaves a small throwaway conversation file in agy's own storage, so the
tier is read at most once a day and cached; the cached value is sent with every push.

Environment:
  ZERO_G_URL           Gateway root, without /v1.
  ZERO_G_VIRTUAL_KEY   Virtual key without a provider allowlist.
  ZERO_G_AGY_ACCOUNT   Name of the Google Antigravity credential in Zero-G for this login.
  AGY_BIN              Optional path to the agy executable (default: agy).
  AGY_TIER_REFRESH_HOURS  How often to re-read the subscription tier (default: 24).
  ZERO_G_CACHE_DIR     Where the tier cache lives (default: ~/.cache/zero-g).
"""

import json
import os
import re
import select
import subprocess
import sys
import tempfile
import time
import urllib.request
from typing import NoReturn


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def fail(message: str) -> NoReturn:
    print(f"agy-quota: {message}", file=sys.stderr)
    sys.exit(1)


def read_tier(agy):
    """Ask the agy ACP server for the account tier; None when it cannot be read."""
    process = subprocess.Popen([agy, "acp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, text=True)

    def call(request_id, method, params):
        process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method,
                                        "params": params}) + "\n")
        process.stdin.flush()
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if not select.select([process.stdout], [], [], 0.5)[0]:
                continue
            line = process.stdout.readline()
            if not line:
                return None
            try:
                message = json.loads(line)
            except ValueError:
                continue
            if message.get("id") == request_id:
                return message.get("result")
        return None

    try:
        if call(1, "initialize", {"protocolVersion": 1, "clientCapabilities": {}}) is None:
            return None
        session = call(2, "session/new", {"cwd": tempfile.gettempdir(), "mcpServers": []})
        if not isinstance(session, dict):
            return None
        call(3, "session/close", {"sessionId": session.get("sessionId")})
        tier = session["_meta"]["antigravity"]["tier"]
        name, tier_id = tier["name"], tier["id"]
        if isinstance(name, str) and isinstance(tier_id, str) and name and tier_id:
            return {"name": name, "id": tier_id}
    except (OSError, KeyError, TypeError):
        pass
    finally:
        try:
            process.stdin.close()
            process.wait(timeout=10)
        except (OSError, subprocess.TimeoutExpired):
            process.kill()
    return None


def cached_tier(agy, account):
    """The account tier, re-read from agy at most every AGY_TIER_REFRESH_HOURS."""
    folder = os.environ.get("ZERO_G_CACHE_DIR") or os.path.join(os.path.expanduser("~"), ".cache", "zero-g")
    path = os.path.join(folder, "agy-tier-" + re.sub(r"[^A-Za-z0-9_.-]", "_", account) + ".json")
    try:
        hours = float(os.environ.get("AGY_TIER_REFRESH_HOURS", "24"))
    except ValueError:
        hours = 24.0
    cached = None
    try:
        with open(path, encoding="utf-8") as handle:
            cached = json.load(handle)
        if time.time() - float(cached["checked_at"]) < hours * 3600 and cached.get("tier"):
            return cached["tier"]
    except (OSError, ValueError, KeyError, TypeError):
        cached = None
    tier = read_tier(agy)
    if tier is None:
        return cached.get("tier") if isinstance(cached, dict) else None
    try:
        os.makedirs(folder, exist_ok=True)
        with open(path, "w", encoding="utf-8") as handle:
            json.dump({"checked_at": time.time(), "tier": tier}, handle)
    except OSError:
        pass
    return tier


def main():
    base = os.environ.get("ZERO_G_URL", "").rstrip("/")
    key = os.environ.get("ZERO_G_VIRTUAL_KEY", "")
    account = os.environ.get("ZERO_G_AGY_ACCOUNT", "")
    if not base or not key or not account:
        fail("set ZERO_G_URL, ZERO_G_VIRTUAL_KEY and ZERO_G_AGY_ACCOUNT")
    try:
        agy = os.environ.get("AGY_BIN", "agy")
        done = subprocess.run(
            [agy, "--print", "/usage", "--output-format", "json"],
            capture_output=True, text=True, timeout=60, check=True,
        )
        data = json.loads(done.stdout)["command"]
        report = data["data"]
        if data.get("name") != "usage" or not isinstance(report.get("groups"), list):
            raise ValueError("unexpected /usage output")
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
        fail(f"could not read quota from agy: {error}")
    push = {"account": account, "report": report}
    tier = cached_tier(agy, account)
    if tier:
        push["tier"] = tier
    request = urllib.request.Request(
        base + "/v1/agy/quota",
        data=json.dumps(push).encode("utf-8"),
        headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.build_opener(NoRedirect).open(request, timeout=10):
            pass
    except OSError as error:
        fail(f"Zero-G did not accept the report: {error}")


main()
