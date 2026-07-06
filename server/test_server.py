#!/usr/bin/env python3
"""Tests for the Pulso reference server. Standard library only.

    python3 test_server.py
"""
import gzip
import json
import os
import shutil
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer

import server


SAMPLES = [
    {
        "uuid": "AAAA-1111",
        "type": "sleepAnalysis",
        "start": "2026-07-06T01:12:00+01:00",
        "end": "2026-07-06T02:40:00+01:00",
        "value": "asleepREM",
        "source": "Apple Watch",
        "metadata": {"timeZone": "Europe/Lisbon"},
    },
    {
        "uuid": "BBBB-2222",
        "type": "heartRate",
        "start": "2026-07-06T08:00:00+01:00",
        "end": "2026-07-06T08:00:00+01:00",
        "value": 58,
        "unit": "count/min",
        "source": "Apple Watch",
    },
    {"deleted": ["CCCC-3333", "DDDD-4444"]},
]


class ServerTest(unittest.TestCase):
    def setUp(self):
        self.data_dir = tempfile.mkdtemp(prefix="pulso-test-")
        self.addCleanup(shutil.rmtree, self.data_dir)
        server.TOKEN = ""
        self._start()

    def _start(self):
        server.Handler.store = server.Store(self.data_dir)
        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        self.port = self.httpd.server_address[1]
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()
        self.addCleanup(self.httpd.shutdown)

    def _request(self, path, body=None, headers=None, method=None):
        req = urllib.request.Request(
            f"http://127.0.0.1:{self.port}{path}",
            data=body,
            headers=headers or {},
            method=method or ("POST" if body is not None else "GET"),
        )
        try:
            with urllib.request.urlopen(req) as resp:
                return resp.status, json.loads(resp.read())
        except urllib.error.HTTPError as e:
            with e:
                return e.code, json.loads(e.read())

    def _post(self, elements, gzipped=False, token=None):
        body = json.dumps(elements).encode()
        headers = {"Content-Type": "application/json"}
        if gzipped:
            body = gzip.compress(body)
            headers["Content-Encoding"] = "gzip"
        if token:
            headers["Authorization"] = f"Bearer {token}"
        return self._request("/ingest", body, headers)

    def _lines(self, stem):
        path = os.path.join(self.data_dir, stem + ".ndjson")
        if not os.path.exists(path):
            return []
        with open(path, encoding="utf-8") as f:
            return [json.loads(l) for l in f if l.strip()]

    def test_health(self):
        status, body = self._request("/health")
        self.assertEqual(status, 200)
        self.assertEqual(body["status"], "ok")

    def test_ingest_and_idempotency(self):
        status, body = self._post(SAMPLES)
        self.assertEqual(status, 200)
        self.assertEqual(body, {"received": 3, "new": 2, "deleted": 2})

        # Re-sending the identical batch must be a no-op.
        status, body = self._post(SAMPLES)
        self.assertEqual(status, 200)
        self.assertEqual(body, {"received": 3, "new": 0, "deleted": 0})

        sleep = self._lines("sleepAnalysis")
        self.assertEqual(len(sleep), 1)
        self.assertEqual(sleep[0]["uuid"], "AAAA-1111")
        self.assertEqual(sleep[0]["value"], "asleepREM")
        self.assertIn("receivedAt", sleep[0])
        self.assertEqual(len(self._lines("heartRate")), 1)
        tombs = self._lines("_deleted")
        self.assertEqual(len(tombs), 1)
        self.assertEqual(tombs[0]["deleted"], ["CCCC-3333", "DDDD-4444"])

    def test_gzip_body(self):
        status, body = self._post(SAMPLES, gzipped=True)
        self.assertEqual(status, 200)
        self.assertEqual(body["new"], 2)

    def test_dedup_survives_restart(self):
        self._post(SAMPLES)
        self.httpd.shutdown()
        self._start()  # rebuilds the seen-uuid index from disk
        status, body = self._post(SAMPLES)
        self.assertEqual(body, {"received": 3, "new": 0, "deleted": 0})

    def test_auth(self):
        server.TOKEN = "secret"
        self.addCleanup(setattr, server, "TOKEN", "")
        status, _ = self._post(SAMPLES)
        self.assertEqual(status, 401)
        status, _ = self._post(SAMPLES, token="wrong")
        self.assertEqual(status, 401)
        status, body = self._post(SAMPLES, token="secret")
        self.assertEqual(status, 200)
        self.assertEqual(body["new"], 2)

    def test_type_is_sanitized_for_file_name(self):
        evil = [{"uuid": "EE-55", "type": "../../etc/passwd", "start": "x", "end": "x"}]
        status, body = self._post(evil)
        self.assertEqual(status, 200)
        self.assertEqual(body["new"], 1)
        names = os.listdir(self.data_dir)
        self.assertEqual(len(names), 1)
        self.assertNotIn("/", names[0].replace(".ndjson", ""))
        self.assertNotIn("..", names[0])

    def test_bad_bodies(self):
        status, _ = self._request("/ingest", b"not json", {"Content-Type": "application/json"})
        self.assertEqual(status, 400)
        status, _ = self._request("/ingest", b'{"not": "array"}', {"Content-Type": "application/json"})
        self.assertEqual(status, 400)
        status, _ = self._request(
            "/ingest", b"\x1f\x8bgarbage", {"Content-Encoding": "gzip"}
        )
        self.assertEqual(status, 400)

    def test_unknown_paths(self):
        status, _ = self._request("/nope")
        self.assertEqual(status, 404)
        status, _ = self._request("/nope", b"[]")
        self.assertEqual(status, 404)


if __name__ == "__main__":
    unittest.main(verbosity=2)
