#!/usr/bin/env python3
"""Pulso reference server.

Receives health samples from the Pulso iOS app and appends them to NDJSON
files on disk — one file per sample type, inspectable with `cat`.

Standard library only. No dependencies.

    python3 server.py

Environment:
    PULSO_PORT   listen port                 (default 8787)
    PULSO_BIND   bind address                (default 0.0.0.0)
    PULSO_DATA   data directory              (default ./data)
    PULSO_TOKEN  bearer token; empty = open  (default empty)

Contract: docs/PROTOCOL.md in the Pulso repository.
"""
import json
import os
import re
import threading
import zlib
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("PULSO_PORT", "8787"))
BIND = os.environ.get("PULSO_BIND", "0.0.0.0")
DATA_DIR = os.environ.get("PULSO_DATA", "./data")
TOKEN = os.environ.get("PULSO_TOKEN", "")

MAX_BODY = 64 * 1024 * 1024  # decompressed bodies larger than this are rejected
UNSAFE = re.compile(r"[^A-Za-z0-9_-]")  # sample types become file names; allow nothing else
TOMBSTONE_FILE = "_deleted"


def _now():
    return datetime.now(timezone.utc).astimezone().isoformat(timespec="seconds")


class Store:
    """Append-only NDJSON store, deduplicated by sample uuid."""

    def __init__(self, root):
        self.root = root
        self.lock = threading.Lock()
        self.seen = set()      # sample uuids already stored
        self.deleted = set()   # uuids already recorded as deleted
        os.makedirs(root, exist_ok=True)
        for name in sorted(os.listdir(root)):
            if not name.endswith(".ndjson"):
                continue
            with open(os.path.join(root, name), encoding="utf-8") as f:
                for line in f:
                    try:
                        obj = json.loads(line)
                    except ValueError:
                        continue
                    if name == TOMBSTONE_FILE + ".ndjson":
                        self.deleted.update(u for u in obj.get("deleted", []) if isinstance(u, str))
                    elif isinstance(obj.get("uuid"), str):
                        self.seen.add(obj["uuid"])

    def _append(self, stem, obj):
        with open(os.path.join(self.root, stem + ".ndjson"), "a", encoding="utf-8") as f:
            f.write(json.dumps(obj, separators=(",", ":"), ensure_ascii=False) + "\n")

    def ingest(self, elements):
        """elements: list of sample dicts and/or {"deleted": [uuid, ...]} tombstones.
        Returns (received, new, deleted). Idempotent: duplicates are skipped."""
        received = len(elements)
        new = 0
        tombstoned = 0
        stamp = _now()
        with self.lock:
            for el in elements:
                if not isinstance(el, dict):
                    continue
                if "deleted" in el:
                    uuids = el["deleted"] if isinstance(el["deleted"], list) else []
                    fresh = [u for u in uuids if isinstance(u, str) and u not in self.deleted]
                    if fresh:
                        self._append(TOMBSTONE_FILE, {"deleted": fresh, "receivedAt": stamp})
                        self.deleted.update(fresh)
                        tombstoned += len(fresh)
                    continue
                uuid = el.get("uuid")
                if not isinstance(uuid, str) or uuid in self.seen:
                    continue
                stem = UNSAFE.sub("_", str(el.get("type") or "")) or "unknown"
                self._append(stem, {**el, "receivedAt": stamp})
                self.seen.add(uuid)
                new += 1
        return received, new, tombstoned


class Handler(BaseHTTPRequestHandler):
    store = None  # set in main()

    def _reply(self, code, payload):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _authorized(self):
        if not TOKEN:
            return True
        return self.headers.get("Authorization", "") == f"Bearer {TOKEN}"

    def do_GET(self):
        if self.path == "/health":
            self._reply(200, {"status": "ok"})
        else:
            self._reply(404, {"error": "not found"})

    def do_POST(self):
        if self.path != "/ingest":
            self._reply(404, {"error": "not found"})
            return
        if not self._authorized():
            self._reply(401, {"error": "unauthorized"})
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            length = 0
        if length <= 0 or length > MAX_BODY:
            self._reply(length and 413 or 400, {"error": "bad content length"})
            return
        body = self.rfile.read(length)
        if self.headers.get("Content-Encoding", "").lower() == "gzip":
            try:
                d = zlib.decompressobj(wbits=31)  # gzip container
                body = d.decompress(body, MAX_BODY)  # cap output: no gzip bombs
                if d.unconsumed_tail:
                    self._reply(413, {"error": "body too large"})
                    return
                if not d.eof:
                    raise zlib.error("truncated gzip body")
            except zlib.error:
                self._reply(400, {"error": "bad gzip body"})
                return
        try:
            elements = json.loads(body)
        except ValueError:
            self._reply(400, {"error": "bad json"})
            return
        if not isinstance(elements, list):
            self._reply(400, {"error": "body must be a json array"})
            return
        received, new, deleted = self.store.ingest(elements)
        self._reply(200, {"received": received, "new": new, "deleted": deleted})

    def log_message(self, fmt, *args):
        print(f"{_now()} {self.address_string()} {fmt % args}")


def main():
    Handler.store = Store(DATA_DIR)
    server = ThreadingHTTPServer((BIND, PORT), Handler)
    auth = "bearer token required" if TOKEN else "open (no auth)"
    print(f"pulso server on {BIND}:{PORT} — data: {os.path.abspath(DATA_DIR)} — {auth}")
    server.serve_forever()


if __name__ == "__main__":
    main()
