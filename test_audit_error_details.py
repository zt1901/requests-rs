"""Public API diagnostic regressions; loopback only, no upstream requests."""
import asyncio
import contextlib
import socketserver
import threading
import time
import unittest
from requests_rs import requests

@contextlib.contextmanager
def proxy(reply, delay=0):
    class Handler(socketserver.StreamRequestHandler):
        def handle(self):
            self.connection.settimeout(3)
            header = bytearray()
            while not header.endswith(b"\r\n\r\n"):
                data = self.rfile.read(1)
                if not data: return
                header.extend(data)
                if len(header) > 8192: return
            if delay: time.sleep(delay)
            if reply:
                try: self.wfile.write(reply)
                except OSError: pass
    class Server(socketserver.ThreadingTCPServer):
        allow_reuse_address = True
        daemon_threads = True
    with Server(("127.0.0.1", 0), Handler) as server:
        thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": .01}, daemon=True)
        thread.start()
        try: yield f"http://127.0.0.1:{server.server_address[1]}"
        finally: server.shutdown(); thread.join(2)

class ErrorDetails(unittest.TestCase):
    def test_connect_status_sync_and_async(self):
        for code in (403, 407, 429, 502, 503, 504, 599, 631, 999):
            with self.subTest(code=code), proxy(f"HTTP/1.1 {code} Rejected\r\nContent-Length: 0\r\n\r\n".encode()) as address:
                with requests.Session(impersonate="chrome152") as client:
                    with self.assertRaisesRegex(requests.ProxyError, str(code)):
                        client.get("https://example.invalid/", proxy=address, timeout=2)
                async def check():
                    async with requests.AsyncSession(impersonate="chrome152") as client:
                        with self.assertRaisesRegex(requests.ProxyError, str(code)):
                            await client.get("https://example.invalid/", proxy=address, timeout=2)
                asyncio.run(check())

    def test_proxy_eof(self):
        with proxy(b"") as address, requests.Session(impersonate="chrome152") as client:
            with self.assertRaisesRegex(requests.ProxyError, "unexpected end of file"):
                client.get("https://example.invalid/", proxy=address, timeout=2)

    def test_proxy_malformed_response(self):
        with proxy(b"NOT-HTTP\r\n\r\n") as address, requests.Session(impersonate="chrome152") as client:
            with self.assertRaisesRegex(requests.ProxyError, "invalid proxy response"):
                client.get("https://example.invalid/", proxy=address, timeout=2)

    def test_proxy_timeout(self):
        with proxy(b"", .2) as address, requests.Session(impersonate="chrome152") as client:
            with self.assertRaisesRegex(requests.ProxyError, "(?i)timeout|timed out|超时"):
                client.get("https://example.invalid/", proxy=address, timeout=.05)

if __name__ == "__main__": unittest.main()
