"""Offline regressions for native request/fingerprint audit findings."""
from __future__ import annotations

import copy
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import threading
from pathlib import Path
import subprocess
import sys
import unittest

from requests_rs import requests


class NativeInputAuditTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.record = json.loads(Path(__file__).with_name("fingerprints.json").read_text("utf-8"))[0]

    def test_native_cookie_pairs_cannot_inject_another_cookie(self):
        with requests.Session(impersonate="chrome152") as session:
            for name, value in (("safe", "ok; injected=yes"), ("bad;name", "ok"),
                                ("bad=name", "ok"), ("safe", "tab\tvalue")):
                with self.subTest(name=name, value=value):
                    with self.assertRaisesRegex(RuntimeError, "invalid request cookie"):
                        session._native.request("GET", "http://127.0.0.1:1/", [],
                                                cookies=[(name, value)])

    def test_cyclic_json_does_not_crash_native_stack(self):
        # Run out of process so this regression reports a failure, not a crashed test suite.
        code = """
from requests_rs import requests
with requests.Session(impersonate="chrome152") as session:
    value = []
    value.append(value)
    try:
        session.post('http://127.0.0.1:1/', json=value)
    except ValueError as error:
        assert 'Circular' in str(error), str(error)
    else:
        raise AssertionError('circular JSON must be rejected')
"""
        result = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_invalid_http2_settings_rejected_at_session_creation(self):
        for identifier, value in ((2, 2), (4, 0x80000000), (5, 16383), (5, 16777216)):
            with self.subTest(identifier=identifier, value=value):
                record = copy.deepcopy(self.record)
                record["http"]["settings"] = [{"id": identifier, "value": value}]
                with self.assertRaisesRegex(RuntimeError, "SETTINGS"):
                    requests.Session(impersonate=[record])

    def test_invalid_http2_priorities_rejected_at_session_creation(self):
        for stream, dependency, source in ((0, 0, "PRIORITY"), (0x80000000, 0, "PRIORITY"),
                                            (1, 0x80000000, "HEADERS"), (2, 0, "HEADERS"),
                                            (1, 1, "HEADERS")):
            with self.subTest(stream=stream, dependency=dependency, source=source):
                record = copy.deepcopy(self.record)
                record["http"]["priorities"] = [{"stream_id": stream, "dependency": dependency,
                    "source": source, "exclusive": 0, "weight": 16}]
                with self.assertRaisesRegex(RuntimeError, "priority"):
                    requests.Session(impersonate=[record])

    def test_invalid_connection_window_rejected(self):
        for increment in (0, 0x7fffffff - 65535 + 1):
            with self.subTest(increment=increment):
                record = copy.deepcopy(self.record)
                record["http"]["connection_window_update"] = increment
                with self.assertRaisesRegex(RuntimeError, "connection_window_update"):
                    requests.Session(impersonate=[record])


class AsyncReservationAuditTests(unittest.TestCase):
    def test_missing_event_loop_does_not_reserve_origins(self):
        with requests.Session(impersonate="chrome152", max_cached_origins=1) as session:
            native = session._native
            for invoke in (
                lambda: native.request_async("GET", "http://127.0.0.1:1/", []),
                lambda: native.request_stream_async("GET", "http://127.0.0.1:1/", []),
                lambda: native.request_multipart_async("POST", "http://127.0.0.1:1/", [], [], []),
                lambda: native.websocket_async("ws://127.0.0.1:1/", []),
            ):
                with self.assertRaisesRegex(RuntimeError, "running event loop"):
                    invoke()
                self.assertEqual(native.cached_origin_count, 0)


class StreamLimitAuditTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                body = b"x" * (256 if self.path == "/large" else 4)
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.url = f"http://127.0.0.1:{cls.server.server_port}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join(timeout=5)

    def test_stream_content_limit_and_connection_slot_release(self):
        with requests.Session(impersonate="chrome152", max_response_bytes=128, max_connections=1) as session:
            response = session.get(self.url + "/large", stream=True, timeout=2)
            with self.assertRaisesRegex(RuntimeError, "max_response_bytes"):
                _ = response.content
            # No explicit close: terminal read failure must release the only slot.
            self.assertEqual(session.get(self.url + "/small", timeout=2).content, b"xxxx")
            response.close()

    def test_connection_pool_wait_is_bounded_for_all_native_entries(self):
        # Child process bounds the test even against an old native implementation.
        code = r"""
import asyncio
from requests_rs import requests
with requests.Session(impersonate="chrome152", max_connections=1) as session:
    held = session.get(URL + '/small', stream=True, timeout=2)
    native = session._native
    calls = [
        ('request', ('GET', URL, [])),
        ('request_stream', ('GET', URL, [])),
        ('request_multipart', ('POST', URL, [], [], [])),
        ('websocket', (URL.replace('http:', 'ws:'), [])),
    ]
    def check_sync():
        for name, args in calls:
            try:
                getattr(native, name)(*args, timeout=0.03)
            except RuntimeError as error:
                assert 'connection slot' in str(error), (name, str(error))
            else:
                raise AssertionError(name + ' did not time out')
    async def check_async():
        for name, args in calls:
            try:
                await asyncio.wait_for(getattr(native, name + '_async')(*args, timeout=0.03), 2)
            except RuntimeError as error:
                assert 'connection slot' in str(error), (name, str(error))
            else:
                raise AssertionError(name + ' did not time out')
    try:
        check_sync()
        asyncio.run(check_async())
    finally:
        held.close()
    assert session.get(URL + '/small', timeout=2).content == b'xxxx'
"""
        result = subprocess.run([sys.executable, "-c", "URL = " + repr(self.url) + "\n" + code],
                                capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_stream_iteration_enforces_cumulative_limit(self):
        with requests.Session(impersonate="chrome152", max_response_bytes=128) as session:
            response = session.get(self.url + "/large", stream=True, timeout=2)
            with self.assertRaisesRegex(RuntimeError, "max_response_bytes"):
                list(response.iter_content(16))
            response.close()


if __name__ == "__main__":
    unittest.main()
