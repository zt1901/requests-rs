from __future__ import annotations

import argparse
import asyncio
from collections import Counter
from contextlib import suppress
import heapq
import json
import math
from pathlib import Path
import random
import select
import socket
import statistics
import subprocess
import threading
import time

import psutil

from test_http3 import 启动服务, 获取UDP端口


# 【可调参数】透明UDP中继只转发QUIC密文，用于模拟代理链路的网络条件。
测试版本 = "chrome150"
冷连接次数 = 5
顺序请求数 = 20
并发请求数 = 100
并发度 = 20
小响应字节 = 1024
大响应字节 = 256 * 1024
大响应请求数 = 20
上游处理延迟毫秒 = 5
结果文件 = Path(__file__).resolve().parent / "benchmark_http3_proxy_results.json"
测试场景 = (
    {"name": "loopback", "rtt_ms": 0, "jitter_ms": 0, "loss": 0.0, "bandwidth_mbps": 0},
    {"name": "metro", "rtt_ms": 20, "jitter_ms": 2, "loss": 0.0, "bandwidth_mbps": 100},
    {"name": "residential", "rtt_ms": 80, "jitter_ms": 10, "loss": 0.002, "bandwidth_mbps": 50},
    {"name": "mobile-stable", "rtt_ms": 150, "jitter_ms": 25, "loss": 0.002, "bandwidth_mbps": 20},
    {"name": "mobile-lossy", "rtt_ms": 150, "jitter_ms": 25, "loss": 0.01, "bandwidth_mbps": 20},
)


def 百分位(values: list[float], quantile: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return 0.0
    return ordered[min(len(ordered) - 1, math.ceil(len(ordered) * quantile) - 1)]


class 透明QUIC中继:
    def __init__(
        self,
        backend: tuple[str, int],
        *,
        rtt_ms: float,
        jitter_ms: float,
        loss: float,
        bandwidth_mbps: float,
        seed: int,
    ) -> None:
        self.backend = backend
        self.one_way_seconds = rtt_ms / 2000
        self.jitter_seconds = jitter_ms / 1000
        self.loss = loss
        self.bytes_per_second = bandwidth_mbps * 1_000_000 / 8 if bandwidth_mbps else 0.0
        self.random = random.Random(seed)
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.setblocking(False)
        self.upstreams: dict[tuple[str, int], socket.socket] = {}
        self.clients_by_socket: dict[socket.socket, tuple[str, int]] = {}
        self.stop = threading.Event()
        self.queue: list[
            tuple[
                float,
                int,
                int,
                bytes,
                tuple[str, int] | None,
                socket.socket,
            ]
        ] = []
        self.sequence = 0
        self.next_available = {0: 0.0, 1: 0.0}
        self.thread = threading.Thread(target=self._run, name="http3-transparent-udp-relay", daemon=True)
        self.packets_up = 0
        self.packets_down = 0
        self.bytes_up = 0
        self.bytes_down = 0
        self.dropped = 0

    @property
    def port(self) -> int:
        return self.listener.getsockname()[1]

    def start(self) -> None:
        self.thread.start()

    def close(self) -> None:
        self.stop.set()
        self.thread.join(timeout=5)
        self.listener.close()
        for upstream in self.upstreams.values():
            upstream.close()

    def _upstream_for(self, client_address: tuple[str, int]) -> socket.socket:
        upstream = self.upstreams.get(client_address)
        if upstream is not None:
            return upstream
        upstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        upstream.bind(("127.0.0.1", 0))
        upstream.connect(self.backend)
        upstream.setblocking(False)
        self.upstreams[client_address] = upstream
        self.clients_by_socket[upstream] = client_address
        return upstream

    def _schedule(
        self,
        direction: int,
        payload: bytes,
        address: tuple[str, int] | None,
        target: socket.socket,
    ) -> None:
        if self.random.random() < self.loss:
            self.dropped += 1
            return
        jitter = self.random.uniform(-self.jitter_seconds, self.jitter_seconds)
        ready = time.monotonic() + max(0.0, self.one_way_seconds + jitter)
        ready = max(ready, self.next_available[direction])
        if self.bytes_per_second:
            self.next_available[direction] = ready + len(payload) / self.bytes_per_second
        self.sequence += 1
        heapq.heappush(self.queue, (ready, self.sequence, direction, payload, address, target))

    def _flush(self) -> None:
        now = time.monotonic()
        while self.queue and self.queue[0][0] <= now:
            _, _, direction, payload, address, target = heapq.heappop(self.queue)
            try:
                if direction == 0:
                    target.send(payload)
                elif address is not None:
                    target.sendto(payload, address)
            except (OSError, ConnectionResetError):
                if self.stop.is_set():
                    return

    def _run(self) -> None:
        while not self.stop.is_set():
            self._flush()
            timeout = 0.01
            if self.queue:
                timeout = max(0.0, min(timeout, self.queue[0][0] - time.monotonic()))
            sources = (self.listener, *self.clients_by_socket.keys())
            try:
                readable, _, _ = select.select(sources, (), (), timeout)
            except (OSError, ValueError):
                return
            for source in readable:
                try:
                    if source is self.listener:
                        payload, address = self.listener.recvfrom(65535)
                        upstream = self._upstream_for(address)
                        self.packets_up += 1
                        self.bytes_up += len(payload)
                        self._schedule(0, payload, None, upstream)
                    else:
                        payload = source.recv(65535)
                        address = self.clients_by_socket[source]
                        self.packets_down += 1
                        self.bytes_down += len(payload)
                        self._schedule(1, payload, address, self.listener)
                except (BlockingIOError, ConnectionResetError):
                    continue
        self._flush()

    def result(self) -> dict[str, int]:
        return {
            "client_mappings": len(self.upstreams),
            "packets_up": self.packets_up,
            "packets_down": self.packets_down,
            "bytes_up": self.bytes_up,
            "bytes_down": self.bytes_down,
            "dropped": self.dropped,
        }


async def 创建客户端(name: str, max_connections: int):
    if name == "requests_rust":
        from requests_rust import AsyncSession

        return AsyncSession(
            impersonate=测试版本,
            http_version="http3",
            verify=False,
            max_connections=max_connections,
        )
    from curl_cffi.const import CurlHttpVersion
    from curl_cffi.requests import AsyncSession

    return AsyncSession(
        impersonate="chrome",
        http_version=CurlHttpVersion.V3ONLY,
        verify=False,
        max_clients=max_connections,
    )


async def 关闭客户端(client) -> None:
    result = client.close()
    if asyncio.iscoroutine(result):
        await result


async def 单次请求(client, url: str, index: int, size: int) -> dict[str, object]:
    started = time.perf_counter()
    try:
        response = await client.get(
            f"{url}/benchmark?size={size}&delay_ms={上游处理延迟毫秒}&id={index}",
            timeout=60 if size >= 大响应字节 else 20,
        )
        elapsed = time.perf_counter() - started
        content = response.content
        if len(content) != size:
            raise AssertionError(f"响应长度错误: {len(content)} != {size}")
        version = str(getattr(response, "http_version", ""))
        if "3" not in version and version != "HTTP/3":
            raise AssertionError(f"不是HTTP/3响应: {version}")
        return {"elapsed": elapsed, "bytes": len(content), "error": None}
    except Exception as error:
        return {
            "elapsed": time.perf_counter() - started,
            "bytes": 0,
            "error": f"{type(error).__name__}: {str(error)[:200]}",
        }


def 成功延迟(results: list[dict[str, object]]) -> list[float]:
    return [float(item["elapsed"]) for item in results if item["error"] is None]


def 错误统计(results: list[dict[str, object]]) -> dict[str, int]:
    return dict(Counter(str(item["error"]) for item in results if item["error"] is not None))


async def 测量客户端(name: str, url: str) -> dict[str, object]:
    cold_results = []
    for index in range(冷连接次数):
        client = await 创建客户端(name, 1)
        try:
            cold_results.append(await 单次请求(client, url, index, 小响应字节))
        finally:
            await 关闭客户端(client)
        await asyncio.sleep(0.05)

    client = await 创建客户端(name, 并发度)
    process = psutil.Process()
    try:
        warmup = []
        for index in range(3):
            result = await 单次请求(client, url, 10_000 + index, 小响应字节)
            warmup.append(result)
            if result["error"] is None:
                break
        cpu_start = sum(process.cpu_times()[:2])

        sequential_results = []
        started = time.perf_counter()
        for index in range(顺序请求数):
            sequential_results.append(await 单次请求(client, url, 20_000 + index, 小响应字节))
        sequential_wall = time.perf_counter() - started

        started = time.perf_counter()
        concurrent_results = await asyncio.gather(
            *(单次请求(client, url, 30_000 + index, 小响应字节) for index in range(并发请求数))
        )
        concurrent_wall = time.perf_counter() - started

        started = time.perf_counter()
        bulk_results = await asyncio.gather(
            *(单次请求(client, url, 40_000 + index, 大响应字节) for index in range(大响应请求数))
        )
        bulk_wall = time.perf_counter() - started
        cpu_seconds = sum(process.cpu_times()[:2]) - cpu_start
    finally:
        await 关闭客户端(client)

    cold = 成功延迟(cold_results)
    sequential = 成功延迟(sequential_results)
    concurrent = 成功延迟(concurrent_results)
    bulk_bytes = sum(int(item["bytes"]) for item in bulk_results)
    total_requests = 顺序请求数 + 并发请求数 + 大响应请求数
    all_results = cold_results + warmup + sequential_results + concurrent_results + bulk_results
    return {
        "client": name,
        "cold_success_rate": len(cold) / 冷连接次数,
        "cold_p50_ms": statistics.median(cold) * 1000 if cold else 0.0,
        "cold_p95_ms": 百分位(cold, 0.95) * 1000,
        "sequential_success_rate": len(sequential) / 顺序请求数,
        "sequential_p50_ms": statistics.median(sequential) * 1000 if sequential else 0.0,
        "sequential_p95_ms": 百分位(sequential, 0.95) * 1000,
        "sequential_rps": len(sequential) / sequential_wall,
        "concurrent_success_rate": len(concurrent) / 并发请求数,
        "concurrent_p50_ms": statistics.median(concurrent) * 1000 if concurrent else 0.0,
        "concurrent_p95_ms": 百分位(concurrent, 0.95) * 1000,
        "concurrent_rps": len(concurrent) / concurrent_wall,
        "bulk_success_rate": sum(item["error"] is None for item in bulk_results) / 大响应请求数,
        "bulk_mbps": bulk_bytes * 8 / bulk_wall / 1_000_000,
        "cpu_ms_per_request": cpu_seconds * 1000 / total_requests,
        "errors": 错误统计(all_results),
    }


async def main(
    selected_scenarios: set[str] | None = None,
    selected_clients: set[str] | None = None,
) -> None:
    rows = []
    for scenario_index, scenario in enumerate(测试场景):
        if selected_scenarios and scenario["name"] not in selected_scenarios:
            continue
        for client_index, client_name in enumerate(("requests_rust", "curl_cffi")):
            if selected_clients and client_name not in selected_clients:
                continue
            backend_port = 获取UDP端口()
            server = 启动服务(backend_port)
            relay = 透明QUIC中继(
                ("127.0.0.1", backend_port),
                rtt_ms=scenario["rtt_ms"],
                jitter_ms=scenario["jitter_ms"],
                loss=scenario["loss"],
                bandwidth_mbps=scenario["bandwidth_mbps"],
                seed=10_000 + scenario_index * 10 + client_index,
            )
            relay.start()
            try:
                result = await 测量客户端(
                    client_name,
                    f"https://127.0.0.1:{relay.port}",
                )
            finally:
                relay.close()
                server.terminate()
                with suppress(subprocess.TimeoutExpired):
                    server.wait(timeout=5)
                if server.poll() is None:
                    server.kill()
                    server.wait(timeout=5)
            result["scenario"] = scenario["name"]
            result["network"] = scenario
            result["relay"] = relay.result()
            rows.append(result)
            print(json.dumps(result, ensure_ascii=False), flush=True)
    output = 结果文件
    if selected_scenarios or selected_clients:
        suffix = "-".join(sorted((selected_scenarios or set()) | (selected_clients or set())))
        output = 结果文件.with_name(f"{结果文件.stem}_{suffix}.json")
    output.write_text(json.dumps(rows, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"结果文件: {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--scenario", action="append", choices=[item["name"] for item in 测试场景])
    parser.add_argument("--client", action="append", choices=("requests_rust", "curl_cffi"))
    args = parser.parse_args()
    asyncio.run(
        main(
            None if args.scenario is None else set(args.scenario),
            None if args.client is None else set(args.client),
        )
    )
