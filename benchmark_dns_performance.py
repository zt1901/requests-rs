from __future__ import annotations

import asyncio
from contextlib import suppress
import gc
import os
import socket
import socketserver
import struct
import threading
import time

import psutil

from requests_rs import requests

AsyncSession = requests.AsyncSession


# 可右键运行；所有HTTP和DNS流量仅使用本机回环地址。
测试版本 = "chrome150"
任务总数 = 1000
采样间隔秒 = 0.005


def 读取DNS名称(packet: bytes, offset: int = 12) -> tuple[str, int]:
    labels = []
    while packet[offset]:
        length = packet[offset]
        offset += 1
        labels.append(packet[offset : offset + length].decode("ascii"))
        offset += length
    return ".".join(labels).lower(), offset + 1


def 构造DNS响应(packet: bytes) -> bytes:
    _name, offset = 读取DNS名称(packet)
    query_type, _query_class = struct.unpack("!HH", packet[offset : offset + 4])
    question = packet[12 : offset + 4]
    if query_type == 1:
        answer = b"\xc0\x0c" + struct.pack("!HHIH", 1, 1, 60, 4) + socket.inet_aton("127.0.0.1")
        answer_count = 1
    else:
        answer = b""
        answer_count = 0
    return packet[:2] + struct.pack("!HHHHH", 0x8180, 1, answer_count, 0, 0) + question + answer


class DNS处理器(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        data, sock = self.request
        sock.sendto(构造DNS响应(data), self.client_address)


class DNS服务(socketserver.ThreadingUDPServer):
    allow_reuse_address = True
    daemon_threads = True


async def 处理HTTP(reader, writer) -> None:
    try:
        while True:
            try:
                await reader.readuntil(b"\r\n\r\n")
            except asyncio.IncompleteReadError:
                break
            body = b"dns-performance"
            writer.write(
                b"HTTP/1.1 200 OK\r\n"
                + f"Content-Length: {len(body)}\r\n".encode()
                + b"Connection: keep-alive\r\n\r\n"
                + body
            )
            await writer.drain()
    except (ConnectionError, asyncio.LimitOverrunError):
        pass
    finally:
        writer.close()
        with suppress(ConnectionError):
            await writer.wait_closed()


class HTTP服务线程:
    def __init__(self) -> None:
        self.loop = (
            asyncio.ProactorEventLoop()
            if os.name == "nt"
            else asyncio.new_event_loop()
        )
        self.ready = threading.Event()
        self.port = 0
        self.server = None
        self.writers = set()
        self.thread = threading.Thread(target=self._run, daemon=True)

    def _run(self) -> None:
        asyncio.set_event_loop(self.loop)

        async def start() -> None:
            async def handle(reader, writer) -> None:
                self.writers.add(writer)
                try:
                    await 处理HTTP(reader, writer)
                finally:
                    self.writers.discard(writer)

            self.server = await asyncio.start_server(
                handle,
                "127.0.0.1",
                0,
                backlog=4096,
            )
            self.port = self.server.sockets[0].getsockname()[1]
            self.ready.set()

        self.loop.run_until_complete(start())
        self.loop.run_forever()
        self.loop.close()

    async def _shutdown(self) -> None:
        self.server.close()
        await self.server.wait_closed()
        writers = list(self.writers)
        for writer in writers:
            writer.close()
        await asyncio.gather(
            *(writer.wait_closed() for writer in writers),
            return_exceptions=True,
        )
        await asyncio.sleep(0)

    def start(self) -> None:
        self.thread.start()
        if not self.ready.wait(timeout=5):
            raise RuntimeError("HTTP性能服务启动超时")

    def close(self) -> None:
        future = asyncio.run_coroutine_threadsafe(self._shutdown(), self.loop)
        future.result(timeout=10)
        self.loop.call_soon_threadsafe(self.loop.stop)
        self.thread.join(timeout=5)


async def 采样资源(stop: asyncio.Event, result: dict[str, int]) -> None:
    process = psutil.Process(os.getpid())
    while not stop.is_set():
        result["peak_rss"] = max(result["peak_rss"], process.memory_info().rss)
        result["peak_threads"] = max(result["peak_threads"], process.num_threads())
        await asyncio.sleep(采样间隔秒)


async def 测量(
    name: str,
    session: AsyncSession,
    url: str,
    task_count: int,
    request_dns_servers: list[str] | None = None,
) -> dict[str, float | int | str]:
    gc.collect()
    process = psutil.Process(os.getpid())
    initial_rss = process.memory_info().rss
    result = {"peak_rss": initial_rss, "peak_threads": process.num_threads()}
    stop = asyncio.Event()
    sampler = asyncio.create_task(采样资源(stop, result))
    async def request(index: int) -> bool:
        kwargs = {}
        if request_dns_servers is not None:
            kwargs["dns_servers"] = request_dns_servers
            kwargs["dns_timeout"] = 2
        try:
            response = await session.get(
                url,
                headers={"X-Request-Id": str(index)},
                cookies={"request_cookie": str(index)},
                proxy=None,
                **kwargs,
            )
            response.raise_for_status()
            return True
        except RuntimeError:
            return False

    started = time.perf_counter()
    try:
        request_results = await asyncio.gather(*(request(index) for index in range(task_count)))
    finally:
        elapsed = time.perf_counter() - started
        stop.set()
        await sampler
    gc.collect()
    end_rss = process.memory_info().rss
    success_count = sum(request_results)
    failure_count = task_count - success_count
    row = {
        "模式": name,
        "任务总数": task_count,
        "并发": task_count,
        "成功数": success_count,
        "失败数": failure_count,
        "吞吐": success_count / elapsed,
        "峰值增量MB": (result["peak_rss"] - initial_rss) / 1024 / 1024,
        "结束增量MB": (end_rss - initial_rss) / 1024 / 1024,
        "峰值线程": result["peak_threads"],
    }
    print(
        f"{name}: 任务总数={task_count}，并发={task_count}，成功={success_count}，失败={failure_count}，"
        f"成功吞吐={row['吞吐']:.2f}请求/秒，"
        f"峰值增量={row['峰值增量MB']:.2f}MB，结束增量={row['结束增量MB']:.2f}MB，"
        f"峰值线程={row['峰值线程']}"
    )
    return row


async def main() -> None:
    dns_server = DNS服务(("127.0.0.1", 0), DNS处理器)
    dns_thread = threading.Thread(target=dns_server.serve_forever, daemon=True)
    dns_thread.start()
    http_server = HTTP服务线程()
    http_server.start()
    http_port = http_server.port
    dns_address = f"127.0.0.1:{dns_server.server_address[1]}"
    ip_url = f"http://127.0.0.1:{http_port}/"
    dns_url = f"http://performance-dns.test:{http_port}/"
    try:
        print("=== 1000个独立get任务同时起跑，不增加Worker并发层 ===")
        for name, session_kwargs, request_dns in (
            ("系统DNS/直接IP", {}, None),
            ("Session指定DNS", {"dns_servers": [dns_address]}, None),
            ("单get指定DNS", {}, [dns_address]),
        ):
            async with AsyncSession(
                impersonate=测试版本,
                max_connections=任务总数,
                **session_kwargs,
            ) as session:
                await 测量(
                    name,
                    session,
                    dns_url if name != "系统DNS/直接IP" else ip_url,
                    任务总数,
                    request_dns,
                )
    finally:
        http_server.close()
        dns_server.shutdown()
        dns_server.server_close()
        dns_thread.join(timeout=2)


if __name__ == "__main__":
    asyncio.run(main())
