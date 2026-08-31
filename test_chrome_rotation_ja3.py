from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time


# 可右键运行；每请求使用唯一代理身份，验证Chromium系JA3乱序和Firefox固定JA3。
测试版本列表 = ("chrome150", "edge152", "firefox151")
每版本测试次数 = 50
项目目录 = Path(__file__).resolve().parent
捕获器目录 = 项目目录.parent


def 获取空闲端口() -> int:
    with socket.socket() as server:
        server.bind(("127.0.0.1", 0))
        return server.getsockname()[1]


def 等待端口(port: int, process: subprocess.Popen) -> None:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("服务提前退出")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError("服务启动超时")


def 去除PSK扩展(ja3: str) -> str:
    parts = ja3.split(",")
    extensions = [value for value in parts[2].split("-") if value != "41"]
    parts[2] = "-".join(extensions)
    return ",".join(parts)


async def main() -> None:
    from requests_rust import AsyncSession

    backend_port = 获取空闲端口()
    proxy_port = 获取空闲端口()
    output = 项目目录 / "test_chromium_rotation_ja3_records.json"
    environment = os.environ.copy()
    environment["FINGERPRINT_OUTPUT"] = str(output)
    environment["FINGERPRINT_PORT"] = str(backend_port)
    output.unlink(missing_ok=True)
    backend = subprocess.Popen(
        [sys.executable, "fingerprint_server.py"],
        cwd=捕获器目录,
        env=environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.STDOUT,
    )
    proxy_environment = os.environ.copy()
    proxy_environment["BENCHMARK_PROXY_PORT"] = str(proxy_port)
    proxy_executable = Path(r"D:\BuildCache\requests-rust-target\release\benchmark_connect_proxy.exe")
    proxy = subprocess.Popen(
        [str(proxy_executable)],
        env=proxy_environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.STDOUT,
    )
    try:
        等待端口(backend_port, backend)
        等待端口(proxy_port, proxy)
        for 测试版本 in 测试版本列表:
            target = (
                f"https://127.0.0.1:{backend_port}/api/fingerprint"
                f"?source={测试版本}-rotation-ja3"
            )
            ja3_values = []
            fingerprint_ids = []
            async with AsyncSession(
                impersonate=测试版本,
                fingerprint_rotation=True,
                fingerprint_pool=True,
                verify=False,
                max_connections=50,
                max_cached_origins=100,
            ) as session:
                proxy_url = (
                    f"http://customer-local-zone-residential-session-{测试版本}-same-route-time-5:"
                    f"local-ipipgo-password@127.0.0.1:{proxy_port}"
                )
                for _ in range(每版本测试次数):
                    response = await session.get(target, proxy=proxy_url, timeout=30)
                    tls = response.json()["tls"]
                    ja3_values.append(tls["ja3"])
                    fingerprint_ids.append(response.fingerprint_id)

            unique_ja3 = len(set(ja3_values))
            unique_fingerprints = len(set(fingerprint_ids))
            print(f"{测试版本}请求次数:", 每版本测试次数)
            print(f"{测试版本} JA3唯一数:", unique_ja3)
            print(f"{测试版本}指纹ID唯一数:", unique_fingerprints)
            if 测试版本 in {"chrome150", "edge152"}:
                assert unique_ja3 == 每版本测试次数, json.dumps(ja3_values, ensure_ascii=False, indent=2)
            else:
                assert len({去除PSK扩展(ja3) for ja3 in ja3_values}) == 1, ja3_values
                assert unique_ja3 <= 2, ja3_values
            assert unique_fingerprints == 1, fingerprint_ids
    finally:
        proxy.terminate()
        backend.terminate()
        proxy.wait(timeout=5)
        backend.wait(timeout=5)
        output.unlink(missing_ok=True)


if __name__ == "__main__":
    asyncio.run(main())
