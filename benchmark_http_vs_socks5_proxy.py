from __future__ import annotations

import asyncio
import os
import statistics
import time
from urllib.parse import quote

import psutil

from requests_rs import requests

AsyncSession = requests.AsyncSession


# 【可调参数】同一IPIPGO账号、端口和sticky session，仅切换HTTP/SOCKS5协议。
代理主机端口 = "proxy.ipipgo.com:31212"
代理用户名 = "customer-liuxianqun"
代理密码 = "xiaoxi"
固定代理会话ID = "httpvssocks5perf"
指定DNS服务器 = ["1.1.1.1", "8.8.8.8"]
测试目标 = "https://www.facebook.com/robots.txt"
出口IP目标 = "https://api.ipify.org"
测试版本 = "chrome146"
顺序请求数 = 20
并发请求数 = 100
并发度 = 20
重复轮数 = 3


def 代理地址(scheme: str) -> str:
    username = f"{代理用户名}-session-{固定代理会话ID}-time-5"
    return f"{scheme}://{quote(username)}:{quote(代理密码)}@{代理主机端口}"


async def 请求一次(session: AsyncSession, proxy: str, url: str) -> float:
    started = time.perf_counter()
    response = await session.get(url, proxy=proxy, timeout=30)
    response.raise_for_status()
    if not response.content:
        raise RuntimeError("响应正文为空")
    return time.perf_counter() - started


async def 顺序测试(session: AsyncSession, proxy: str) -> dict[str, float | int]:
    latencies = []
    failures = 0
    for _ in range(顺序请求数):
        try:
            latencies.append(await 请求一次(session, proxy, 测试目标))
        except RuntimeError:
            failures += 1
    latencies.sort()
    return {
        "success": len(latencies),
        "failure": failures,
        "p50_ms": statistics.median(latencies) * 1000,
        "p95_ms": latencies[min(len(latencies) - 1, int(len(latencies) * 0.95))] * 1000,
        "average_ms": statistics.fmean(latencies) * 1000,
    }


async def 并发测试(session: AsyncSession, proxy: str) -> dict[str, float | int]:
    process = psutil.Process(os.getpid())
    initial_rss = process.memory_info().rss
    peak_rss = initial_rss
    peak_threads = process.num_threads()
    latencies = []
    failures = 0
    next_index = 0
    counter_lock = asyncio.Lock()
    stop = asyncio.Event()

    async def sampler() -> None:
        nonlocal peak_rss, peak_threads
        while not stop.is_set():
            peak_rss = max(peak_rss, process.memory_info().rss)
            peak_threads = max(peak_threads, process.num_threads())
            await asyncio.sleep(0.01)

    async def worker() -> None:
        nonlocal next_index, failures
        while True:
            async with counter_lock:
                if next_index >= 并发请求数:
                    return
                next_index += 1
            try:
                latencies.append(await 请求一次(session, proxy, 测试目标))
            except RuntimeError:
                failures += 1

    sampler_task = asyncio.create_task(sampler())
    started = time.perf_counter()
    try:
        await asyncio.gather(*(worker() for _ in range(并发度)))
    finally:
        elapsed = time.perf_counter() - started
        stop.set()
        await sampler_task
    latencies.sort()
    return {
        "success": len(latencies),
        "failure": failures,
        "throughput_rps": len(latencies) / elapsed,
        "p50_ms": statistics.median(latencies) * 1000,
        "p95_ms": latencies[min(len(latencies) - 1, int(len(latencies) * 0.95))] * 1000,
        "peak_rss_delta_mb": (peak_rss - initial_rss) / 1024 / 1024,
        "peak_threads": peak_threads,
    }


async def main() -> None:
    proxies = {scheme: 代理地址(scheme) for scheme in ("http", "socks5")}
    rows: dict[str, list[dict[str, object]]] = {"http": [], "socks5": []}
    async with AsyncSession(
        impersonate=测试版本,
        fingerprint_rotation=False,
        fingerprint_pool=True,
        max_connections=并发度,
        dns_servers=指定DNS服务器,
        dns_timeout=5,
    ) as session:
        for scheme, proxy in proxies.items():
            ip_response = await session.get(出口IP目标, proxy=proxy, timeout=30)
            ip_response.raise_for_status()
            print(f"{scheme}出口IP: {ip_response.text.strip()}")

        for round_index in range(重复轮数):
            order = ("http", "socks5") if round_index % 2 == 0 else ("socks5", "http")
            for scheme in order:
                proxy = proxies[scheme]
                await 请求一次(session, proxy, 测试目标)
                sequential = await 顺序测试(session, proxy)
                concurrent = await 并发测试(session, proxy)
                row = {"round": round_index + 1, "sequential": sequential, "concurrent": concurrent}
                rows[scheme].append(row)
                print(scheme, row)

    print("=== 三轮中位汇总 ===")
    for scheme in ("http", "socks5"):
        values = rows[scheme]
        print(
            f"{scheme}: 顺序平均延迟={statistics.median(row['sequential']['average_ms'] for row in values):.2f}ms，"
            f"顺序P50={statistics.median(row['sequential']['p50_ms'] for row in values):.2f}ms，"
            f"顺序P95={statistics.median(row['sequential']['p95_ms'] for row in values):.2f}ms，"
            f"并发吞吐={statistics.median(row['concurrent']['throughput_rps'] for row in values):.2f}请求/秒，"
            f"并发P95={statistics.median(row['concurrent']['p95_ms'] for row in values):.2f}ms，"
            f"RSS峰值增量={statistics.median(row['concurrent']['peak_rss_delta_mb'] for row in values):.2f}MB，"
            f"线程峰值={statistics.median(row['concurrent']['peak_threads'] for row in values):.0f}"
        )


if __name__ == "__main__":
    asyncio.run(main())
