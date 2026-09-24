#!/usr/bin/python3
"""Forward WorkBuddy lifecycle hooks to the local Astra office collector. Always exit 0."""
import json
import os
import sys
import urllib.request

def edition_from_path(path):
    normalized = os.path.abspath(path).replace("\\", "/").lower()
    return "international" if "/.workbuddy-ai/" in normalized else "domestic"

def main():
    sys.stdout.write("{}\n")
    sys.stdout.flush()
    url_file = os.path.join(os.path.dirname(os.path.abspath(__file__)), "astra-office-monitor.url")
    base = "http://127.0.0.1:8849"
    try:
        with open(url_file, encoding="utf-8") as handle:
            base = handle.read().strip() or base
    except OSError:
        pass
    raw = sys.stdin.buffer.read(65536) or b"{}"
    try:
        payload = json.loads(raw.decode("utf-8") or "{}")
    except Exception:
        payload = {}
    if not isinstance(payload, dict):
        payload = {}
    payload["agent_edition"] = edition_from_path(__file__)
    raw = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        base.rstrip("/") + "/api/workbuddy-hook",
        data=raw,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        urllib.request.urlopen(req, timeout=2).read()
    except Exception:
        pass


if __name__ == "__main__":
    try:
        main()
    except Exception:
        pass
    sys.exit(0)
