#!/usr/bin/python3
"""Forward Codex lifecycle hooks to the local Astra office collector. Always exit 0."""
import os
import sys
import urllib.request

def main():
    url_file = os.path.join(os.path.expanduser("~"), ".codex", "hooks", "astra-office-monitor.url")
    base = "http://127.0.0.1:8849"
    try:
        with open(url_file, encoding="utf-8") as handle:
            base = handle.read().strip() or base
    except OSError:
        pass
    raw = sys.stdin.buffer.read(65536) or b"{}"
    req = urllib.request.Request(
        base.rstrip("/") + "/api/codex-hook",
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
