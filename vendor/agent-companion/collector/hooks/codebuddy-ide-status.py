#!/usr/bin/env python3
"""Observation only. IDE and CLI share settings; forward IDE clients only."""
import json
import os
import pathlib
import subprocess
import sys
import urllib.request

def edition_from_text(text):
    lower = text.lower()
    if 'codebuddycn' in lower or 'CodeBuddy CN' in text:
        return 'domestic'
    if 'com.tencent.codebuddy' in lower or '/CodeBuddy.app' in text or 'Application Support/CodeBuddy/' in text:
        return 'international'
    return None

def edition_from_path(path):
    normalized = os.path.abspath(path).replace("\\", "/").lower()
    return "domestic" if "/.codebuddycn/" in normalized else None

def edition_from_host():
    blob = ' '.join(os.environ.get(key, '') for key in (
        '__CFBundleIdentifier', 'XPC_SERVICE_NAME', 'VSCODE_IPC_HOOK', 'VSCODE_CODE_CACHE_PATH', 'VSCODE_NLS_CONFIG',
    ))
    found = edition_from_text(blob)
    if found:
        return found
    pid = os.getpid()
    for _ in range(8):
        try:
            out = subprocess.check_output(['ps', '-p', str(pid), '-o', 'ppid=,command='], text=True, timeout=0.4)
        except Exception:
            break
        found = edition_from_text(out)
        if found:
            return found
        try:
            pid = int(out.split()[0])
        except Exception:
            break
        if pid <= 1:
            break
    return edition_from_path(__file__)

try:
    payload = json.loads(sys.stdin.read(1024 * 1024))
    if str(payload.get('client', '')).lower() in ('codebuddyide', 'codebuddy', 'vscode'):
        edition = edition_from_host()
        if edition:
            payload['agent_edition'] = edition
        url = pathlib.Path(__file__).with_name('astra-office-codebuddy-ide.url').read_text().strip()
        request = urllib.request.Request(url + '/api/codebuddy-ide-hook', data=json.dumps(payload).encode(), headers={'Content-Type': 'application/json'})
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=2):
            pass
except Exception:
    pass
print('{}')
