#!/usr/bin/env python3
"""Best-effort PostInvocation collection for the interactive Antigravity CLI."""

import json
import os
import signal
import stat
import sys
import urllib.request

MAX_INPUT = 1024 * 1024
MAX_TRANSCRIPT = 2 * 1024 * 1024


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def collect():
    base = os.environ.get("ZERO_G_URL", "").rstrip("/")
    key = os.environ.get("ZERO_G_VIRTUAL_KEY", "")
    if not base or not key:
        return
    raw = sys.stdin.buffer.read(MAX_INPUT + 1)
    if len(raw) > MAX_INPUT:
        return
    event = json.loads(raw)
    path = event.get("transcriptPath")
    start = event.get("initialNumSteps")
    conversation = event.get("conversationId")
    model = event.get("modelName")
    if (not isinstance(path, str) or not isinstance(conversation, str) or not conversation
            or not isinstance(model, str) or not model
            or type(start) is not int or start < 0):
        return

    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0))
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            return
        offset = max(0, info.st_size - MAX_TRANSCRIPT)
        os.lseek(fd, offset, os.SEEK_SET)
        data = os.read(fd, MAX_TRANSCRIPT)
    finally:
        os.close(fd)
    if offset:
        _, sep, data = data.partition(b"\n")
        if not sep:
            return

    matches = []
    for line in data.splitlines():
        try:
            record = json.loads(line)
        except (ValueError, UnicodeDecodeError):
            continue
        if (isinstance(record, dict) and record.get("source") == "MODEL"
                and record.get("status") == "DONE"
                and type(record.get("step_index")) is int and record["step_index"] == start):
            matches.append(record)
    if not matches:
        return
    record = min(matches, key=lambda item: item["step_index"])

    def count(name):
        value = record.get(name)
        return value if type(value) is int and 0 <= value <= 9223372036854775807 else None

    payload = {
        "conversation_id": conversation,
        "step_index": record["step_index"],
        "model": model,
        "input_tokens": count("input_tokens"),
        "output_tokens": count("output_tokens"),
        "request": {"lastUserInput": event.get("lastUserInput")},
        "response": record,
    }
    request = urllib.request.Request(
        base + "/v1/agy/observations",
        data=json.dumps(payload).encode("utf-8"),
        headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.build_opener(NoRedirect).open(request, timeout=2):
        pass


def timeout(_signum, _frame):
    raise TimeoutError()


try:
    signal.signal(signal.SIGALRM, timeout)
    signal.alarm(4)
    collect()
except Exception:
    pass
finally:
    signal.alarm(0)
    print("{}")
