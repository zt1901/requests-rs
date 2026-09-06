"""Running-worker cancellation regression; uses only a local UDP sink."""
from __future__ import annotations

import asyncio
import socket
import subprocess
import sys
import unittest
from pathlib import Path


async def _running_worker_probe():
    from requests_rs import requests

    sink = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sink.bind(("127.0.0.1", 0))
    sink.setblocking(False)
    loop = asyncio.get_running_loop()
    url = f"https://127.0.0.1:{sink.getsockname()[1]}/cancel"
    seen_peers = set()
    try:
        async with requests.AsyncSession(
            impersonate="chrome152", verify=False, http_version="http3",
            max_connections=1, connect_timeout=20,
        ) as session:
            for _ in range(3):
                pending = asyncio.create_task(session.post(url, data=b"do-not-replay", timeout=20))
                try:
                    # Seeing Initial proves spawn_blocking has started, not merely queued.
                    # A fresh peer proves the cancelled connection was discarded and the
                    # same-origin mutex plus the only connection permit became available.
                    async def fresh_initial():
                        while True:
                            packet, peer = await loop.sock_recvfrom(sink, 65535)
                            if peer not in seen_peers:
                                assert packet, "empty QUIC datagram"
                                seen_peers.add(peer)
                                return
                    await asyncio.wait_for(fresh_initial(), timeout=3)
                finally:
                    pending.cancel()
                    try:
                        await asyncio.wait_for(pending, timeout=1)
                    except asyncio.CancelledError:
                        pass
                    else:
                        raise AssertionError("request did not propagate cancellation")
            assert len(seen_peers) == 3
    finally:
        sink.close()


class RunningQuicCancellationTests(unittest.TestCase):
    def test_cancel_running_quic_releases_same_origin_and_only_permit(self):
        result = subprocess.run(
            [sys.executable, str(Path(__file__).resolve()), "--probe"],
            capture_output=True, text=True, timeout=15,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    if "--probe" in sys.argv:
        asyncio.run(_running_worker_probe())
    else:
        unittest.main()
