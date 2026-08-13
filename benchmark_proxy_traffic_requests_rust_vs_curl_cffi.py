from __future__ import annotations

import asyncio
import json
import os
import statistics
import subprocess
import threading
import time
from http.server import ThreadingHTTPServer
from pathlib import Path

from curl_cffi import requests as curl_requests
from requests_rust import AsyncSession

from benchmark_proxy_rust_vs_curl_cffi import (
    IPIPGO本地代理,
    Rust后端目录,
    启动超时秒,
    代理密码,
    代理用户名,
    获取空闲端口,
    准备Rust后端,
    等待端口,
    是管理员,
    自动管理员重启,
)


# 【可调参数】右键运行时使用本地 Rust HTTPS 页面和本地 CONNECT 代理，不访问外网。
项目目录 = Path(__file__).resolve().parent
测试指纹版本 = "chrome142"
并发数 = 3
每轮请求数 = 12
有效载荷字节 = 2 * 1024 * 1024
curl客户端上限 = 1_000
# ══════════════════════════════════════════════════


async def 运行一轮(库名称: str, 模式: str, 目标地址: str, 代理地址: str) -> dict[str, object]:
    if 库名称 == "requests_rust":
        session = AsyncSession(
            impersonate=测试指纹版本,
            proxy=代理地址,
            verify=False,
        )
    elif 库名称 == "curl_cffi":
        session = curl_requests.AsyncSession(
            impersonate="chrome",
            max_clients=curl客户端上限,
            proxy=代理地址,
            verify=False,
        )
    else:
        raise ValueError(f"未知库: {库名称}")

    payload = b"U" * 有效载荷字节
    latencies: list[float] = []
    原生统计上行 = 0
    原生统计下行 = 0
    failures = 0
    next_index = 0
    counter_lock = asyncio.Lock()
    IPIPGO本地代理.重置流量()

    async def 请求一次(index: int) -> None:
        nonlocal failures, 原生统计上行, 原生统计下行
        started = time.perf_counter()
        try:
            if 模式 == "download":
                request_kwargs = {"transfer_stats": True} if 库名称 == "requests_rust" else {}
                response = await session.get(f"{目标地址}/benchmark/download?id={index}", **request_kwargs)
                if response.status_code != 200 or len(response.content) != 有效载荷字节:
                    raise RuntimeError("下载响应长度或状态异常")
            else:
                request_kwargs = {"transfer_stats": True} if 库名称 == "requests_rust" else {}
                response = await session.post(
                    f"{目标地址}/benchmark/upload?id={index}",
                    data=payload,
                    headers={"Content-Type": "application/octet-stream"},
                    **request_kwargs,
                )
                if response.status_code != 200 or response.json().get("received_bytes") != 有效载荷字节:
                    raise RuntimeError("上传响应长度或状态异常")
            stats = getattr(response, "transfer_stats", None)
            if 库名称 == "requests_rust":
                assert stats is not None
                assert stats.scope == "tcp_payload_through_metered_tunnel"
            if stats is not None:
                原生统计上行 += stats.upload_size
                原生统计下行 += stats.download_size
        except Exception:
            failures += 1
        finally:
            latencies.append(time.perf_counter() - started)

    async def worker() -> None:
        nonlocal next_index
        while True:
            async with counter_lock:
                if next_index >= 每轮请求数:
                    return
                index = next_index
                next_index += 1
            await 请求一次(index)

    started = time.perf_counter()
    try:
        await asyncio.gather(*(worker() for _ in range(并发数)))
    finally:
        await session.close()
    elapsed = time.perf_counter() - started
    tunnel_upload, tunnel_download = IPIPGO本地代理.流量快照()
    effective_bytes = 有效载荷字节 * (每轮请求数 - failures)
    latencies.sort()
    return {
        "library": 库名称,
        "mode": 模式,
        "requests": 每轮请求数,
        "concurrency": 并发数,
        "success": 每轮请求数 - failures,
        "failure": failures,
        "elapsed_s": elapsed,
        "effective_payload_bytes": effective_bytes,
        "effective_mib_per_s": effective_bytes / 1024 / 1024 / elapsed,
        "proxy_client_to_server_bytes": tunnel_upload,
        "proxy_server_to_client_bytes": tunnel_download,
        "proxy_total_bytes": tunnel_upload + tunnel_download,
        "native_transfer_upload_bytes": 原生统计上行 or None,
        "native_transfer_download_bytes": 原生统计下行 or None,
        "p50_ms": statistics.median(latencies) * 1000,
        "p95_ms": latencies[min(len(latencies) - 1, int(len(latencies) * 0.95))] * 1000,
    }


def 打印结果(结果: list[dict[str, object]]) -> None:
    print("| 库 | 模式 | 成功 | 有效吞吐 MiB/s | P50 ms | P95 ms | 代理上行 MiB | 代理下行 MiB | 代理总流量 MiB |")
    print("|---|---|---:|---:|---:|---:|---:|---:|---:|")
    for row in 结果:
        print(
            f"| {row['library']} | {row['mode']} | {row['success']}/{row['requests']} | "
            f"{row['effective_mib_per_s']:.2f} | {row['p50_ms']:.2f} | {row['p95_ms']:.2f} | "
            f"{row['proxy_client_to_server_bytes'] / 1024 / 1024:.2f} | "
            f"{row['proxy_server_to_client_bytes'] / 1024 / 1024:.2f} | "
            f"{row['proxy_total_bytes'] / 1024 / 1024:.2f} |"
        )


async def main() -> None:
    backend_port = 获取空闲端口()
    executable = 准备Rust后端()
    environment = os.environ.copy()
    environment["FINGERPRINT_PORT"] = str(backend_port)
    environment["FINGERPRINT_OUTPUT"] = str(Rust后端目录 / "target" / "traffic_benchmark_records.json")
    environment["FINGERPRINT_BENCHMARK_MODE"] = "1"
    if not 是管理员():
        environment["FINGERPRINT_DISABLE_TCP_CAPTURE"] = "1"
    backend = subprocess.Popen(
        [str(executable)],
        cwd=Rust后端目录,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    proxy = ThreadingHTTPServer(("127.0.0.1", 0), IPIPGO本地代理)
    threading.Thread(target=proxy.serve_forever, daemon=True).start()
    proxy_url = f"http://{代理用户名}:{代理密码}@127.0.0.1:{proxy.server_port}"
    target_url = f"https://127.0.0.1:{backend_port}"
    try:
        等待端口(backend_port, backend)
        results = []
        for mode in ("download", "upload"):
            for library in ("requests_rust", "curl_cffi"):
                result = await 运行一轮(library, mode, target_url, proxy_url)
                if library == "requests_rust":
                    assert result["native_transfer_upload_bytes"] == result["proxy_client_to_server_bytes"]
                    # 原生计量包含上游代理返回的CONNECT响应，外层代理统计从隧道建立后开始。
                    assert result["native_transfer_download_bytes"] >= result["proxy_server_to_client_bytes"]
                results.append(result)
                print(json.dumps(result, ensure_ascii=False))
        打印结果(results)
        (项目目录 / "benchmark_proxy_traffic_results.json").write_text(
            json.dumps(results, ensure_ascii=False, indent=2),
            encoding="utf-8",
        )
    finally:
        proxy.shutdown()
        proxy.server_close()
        backend.terminate()
        backend.wait(timeout=启动超时秒)


if __name__ == "__main__":
    if "--elevated" not in __import__("sys").argv and not 是管理员():
        自动管理员重启()
    else:
        asyncio.run(main())
