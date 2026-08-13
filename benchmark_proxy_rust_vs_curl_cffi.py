from __future__ import annotations

import asyncio
import base64
import ctypes
import json
import os
import random
import select
import shutil
import socket
import statistics
import subprocess
import sys
import threading
import time
import warnings
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import psutil
import requests
import wreq
from urllib3.exceptions import InsecureRequestWarning
from curl_cffi import requests as curl_requests
from requests_rust import AsyncSession


# 基准后端使用本地自签名证书且请求明确关闭校验，关闭重复告警以免影响同步库测量。
warnings.filterwarnings("ignore", category=InsecureRequestWarning)


# ══════════════════════════════════════════════════
# 【可调参数】右键运行时自动申请管理员权限，启动本地 Rust HTTPS 后端和 IPIPGO 风格 CONNECT 代理，不访问外网。
项目目录 = Path(__file__).resolve().parent
捕获器目录 = 项目目录.parent
Rust后端目录 = 捕获器目录 / "rust_fingerprint_server"
测试指纹版本 = "chrome142"
并发Worker数 = 10
# curl_cffi 允许保留更多 Client，实际同时请求数仍由并发Worker数决定。
curl客户端上限 = 1_000
请求规模列表 = [10, 10_000, 1_000_000]
延迟采样上限 = 20_000
启动超时秒 = 20
代理用户名 = "customer-local-zone-residential-session-benchmark-time-5"
代理密码 = "local-ipipgo-password"
# ══════════════════════════════════════════════════


def 是管理员() -> bool:
    try:
        return bool(ctypes.windll.shell32.IsUserAnAdmin())
    except AttributeError:
        return False


def 自动管理员重启() -> bool:
    """从 IDE 右键启动时自动弹出 UAC，并等待管理员子进程完成。"""
    arguments = " ".join(f'"{argument}"' for argument in sys.argv[1:] if argument != "--elevated")
    command = (
        f"Start-Process -FilePath '{sys.executable}' "
        f"-ArgumentList '" + f'"{Path(__file__).resolve()}" --elevated {arguments}' + "' -Verb RunAs -Wait"
    )
    result = subprocess.run(["powershell", "-NoProfile", "-Command", command], check=False)
    if result.returncode:
        raise RuntimeError("管理员权限请求被取消或管理员子进程执行失败")
    return True


def 获取空闲端口() -> int:
    with socket.socket() as server:
        server.bind(("127.0.0.1", 0))
        return server.getsockname()[1]


def 等待端口(port: int, process: subprocess.Popen[bytes]) -> None:
    deadline = time.monotonic() + 启动超时秒
    while time.monotonic() < deadline:
        if process.poll() is not None:
            output = process.stdout.read().decode("utf-8", errors="replace") if process.stdout else ""
            raise RuntimeError(f"Rust基准后端异常退出: {output}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise TimeoutError("Rust基准后端启动超时")


class IPIPGO本地代理(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    命中用户: Counter[str] = Counter()
    记录锁 = threading.Lock()

    def log_message(self, *_: object) -> None:
        pass

    def do_CONNECT(self) -> None:
        authorization = self.headers.get("Proxy-Authorization", "")
        if not authorization.startswith("Basic "):
            self.send_error(407, "需要代理认证")
            return
        try:
            user, password = base64.b64decode(authorization[6:]).decode("utf-8").split(":", 1)
        except ValueError:
            self.send_error(407, "代理认证格式无效")
            return
        if password != 代理密码 or not user.startswith("customer-local-zone-residential-session-"):
            self.send_error(407, "IPIPGO风格账号验证失败")
            return
        host, separator, port_text = self.path.rpartition(":")
        if not separator:
            self.send_error(400, "CONNECT目标缺少端口")
            return
        try:
            upstream = socket.create_connection((host, int(port_text)), timeout=5)
        except OSError:
            # HTTP 状态行只能使用 Latin-1，Windows 本地错误文本可能包含中文。
            self.send_error(502, "Upstream connection failed")
            return
        with type(self).记录锁:
            type(self).命中用户[user] += 1
        self.send_response(200, "Connection Established")
        self.end_headers()
        self.wfile.flush()
        self.close_connection = True
        self.connection.setblocking(False)
        upstream.setblocking(False)
        try:
            while True:
                readable, _, exceptional = select.select((self.connection, upstream), (), (self.connection, upstream), 30)
                if exceptional or not readable:
                    return
                for source in readable:
                    target = upstream if source is self.connection else self.connection
                    try:
                        data = source.recv(64 * 1024)
                    except (BlockingIOError, ConnectionError):
                        continue
                    if not data:
                        return
                    try:
                        target.sendall(data)
                    except ConnectionError:
                        return
        finally:
            upstream.close()


def 准备Rust后端() -> Path:
    executable = Rust后端目录 / "target" / "release" / "rust-fingerprint-server.exe"
    if not executable.exists():
        subprocess.run(["cargo", "build", "--release", "--bin", "rust-fingerprint-server"], cwd=Rust后端目录, check=True)
    win_divert = Rust后端目录 / "vendor" / "WinDivert-2.2.2-A" / "x64"
    for name in ("WinDivert.dll", "WinDivert64.sys"):
        source = win_divert / name
        target = executable.parent / name
        if source.exists() and (not target.exists() or source.read_bytes() != target.read_bytes()):
            shutil.copy2(source, target)
    return executable


async def 测量资源(stop: asyncio.Event, result: dict[str, float]) -> None:
    process = psutil.Process(os.getpid())
    process.cpu_percent(None)
    while not stop.is_set():
        result["peak_rss"] = max(result["peak_rss"], process.memory_info().rss)
        result["peak_threads"] = max(result["peak_threads"], process.num_threads())
        result["peak_cpu"] = max(result["peak_cpu"], process.cpu_percent(None))
        await asyncio.sleep(0.05)


async def 运行一轮(name: str, total: int, target_url: str, proxy_url: str) -> dict[str, object]:
    process = psutil.Process(os.getpid())
    initial_rss = process.memory_info().rss
    initial_cpu = process.cpu_times()
    metrics: dict[str, float] = {
        "peak_rss": initial_rss,
        "peak_threads": process.num_threads(),
        "peak_cpu": 0,
    }
    stop = asyncio.Event()
    sampler = asyncio.create_task(测量资源(stop, metrics))
    next_index = 0
    counter_lock = asyncio.Lock()
    successes = 0
    failures = 0
    latencies: list[float] = []

    session: object | None = None
    if name == "requests_rust":
        session = AsyncSession(
            impersonate=测试指纹版本,
            proxy=proxy_url,
            verify=False,
            headers=[
                ("Accept", "application/json"),
                ("Accept-Language", "zh-CN,zh;q=0.9"),
                ("Cache-Control", "no-cache"),
                ("X-Benchmark-Default", "cached"),
            ],
        )
        async def request_once(index: int) -> bool:
            response = await session.get(target_url + f"?id={index}")
            return response.status_code == 200 and response.content == b"{}"
    elif name == "curl_cffi":
        session = curl_requests.AsyncSession(
            impersonate="chrome",
            max_clients=curl客户端上限,
            proxy=proxy_url,
            verify=False,
            headers={
                "Accept": "application/json",
                "Accept-Language": "zh-CN,zh;q=0.9",
                "Cache-Control": "no-cache",
                "X-Benchmark-Default": "cached",
            },
        )
        async def request_once(index: int) -> bool:
            response = await session.get(target_url + f"?id={index}")
            return response.status_code == 200 and response.content == b"{}"
    def build_sync_session() -> requests.Session:
        result = requests.Session()
        result.headers.update({
            "Accept": "application/json",
            "Accept-Language": "zh-CN,zh;q=0.9",
            "Cache-Control": "no-cache",
            "X-Benchmark-Default": "cached",
        })
        result.proxies.update({"https": proxy_url})
        result.verify = False
        return result

    def build_rnet_client() -> wreq.Client:
        return wreq.Client(
            emulation=wreq.Emulation.Chrome142,
            headers={
                "Accept": "application/json",
                "Accept-Language": "zh-CN,zh;q=0.9",
                "Cache-Control": "no-cache",
                "X-Benchmark-Default": "cached",
            },
            proxies=[wreq.Proxy.all(proxy_url)],
            tls_verify=False,
        )

    if name == "rnet":
        session = build_rnet_client()

        async def request_once(index: int) -> bool:
            response = await session.get(target_url + f"?id={index}")
            return response.status.as_int() == 200 and await response.text() == "{}"

    async def worker() -> None:
        nonlocal next_index, successes, failures
        # requests 的网络请求最终在工作线程中执行，不能让多个 worker 共享同步 Session。
        sync_session = build_sync_session() if name == "requests" else None
        try:
            while True:
                async with counter_lock:
                    if next_index >= total:
                        return
                    index = next_index
                    next_index += 1
                started = time.perf_counter()
                try:
                    if sync_session is not None:
                        response = await asyncio.to_thread(sync_session.get, target_url + f"?id={index}")
                        ok = response.status_code == 200 and response.content == b"{}"
                    else:
                        ok = await request_once(index)
                except Exception:
                    ok = False
                elapsed = time.perf_counter() - started
                if len(latencies) < 延迟采样上限 or random.randrange(index + 1) < 延迟采样上限:
                    if len(latencies) < 延迟采样上限:
                        latencies.append(elapsed)
                    else:
                        latencies[random.randrange(延迟采样上限)] = elapsed
                if ok:
                    successes += 1
                else:
                    failures += 1
        finally:
            if sync_session is not None:
                sync_session.close()

    started = time.perf_counter()
    try:
        await asyncio.gather(*(worker() for _ in range(并发Worker数)))
    finally:
        if session is not None:
            if name == "rnet":
                session.close()
            else:
                await session.close()
        elapsed = time.perf_counter() - started
        stop.set()
        await sampler
    latencies.sort()
    rss_delta = process.memory_info().rss - initial_rss
    final_cpu = process.cpu_times()
    cpu_seconds = (final_cpu.user - initial_cpu.user) + (final_cpu.system - initial_cpu.system)
    return {
        "library": name,
        "requests": total,
        "workers": 并发Worker数,
        "elapsed_s": elapsed,
        "success": successes,
        "failure": failures,
        "success_rate": successes / total * 100,
        "throughput_rps": total / elapsed,
        "p50_ms": statistics.median(latencies) * 1000,
        "p95_ms": latencies[min(len(latencies) - 1, int(len(latencies) * 0.95))] * 1000,
        "peak_rss_delta_mb": (metrics["peak_rss"] - initial_rss) / 1024 / 1024,
        "end_rss_delta_mb": rss_delta / 1024 / 1024,
        "peak_threads": int(metrics["peak_threads"]),
        "peak_cpu_percent": metrics["peak_cpu"],
        # 这是当前 Python 进程的 CPU 时间，不等同于硬件电能；用于同机同场景的能效代理比较。
        "cpu_seconds": cpu_seconds,
        "cpu_ms_per_request": cpu_seconds / total * 1000,
    }


def 打印表格(rows: list[dict[str, object]]) -> None:
    print("| 库 | 请求量 | Worker | 成功率 | 吞吐 req/s | P50 ms | P95 ms | 峰值 RSS 增量 MB | 线程峰值 | CPU 峰值 | CPU ms/请求 |")
    print("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for row in rows:
        print(
            f"| {row['library']} | {row['requests']:,} | {row['workers']} | {row['success_rate']:.4f}% | "
            f"{row['throughput_rps']:.2f} | {row['p50_ms']:.3f} | {row['p95_ms']:.3f} | "
            f"{row['peak_rss_delta_mb']:.2f} | {row['peak_threads']} | {row['peak_cpu_percent']:.1f}% | "
            f"{row['cpu_ms_per_request']:.4f} |"
        )


async def main() -> None:
    backend_port = 获取空闲端口()
    executable = 准备Rust后端()
    environment = os.environ.copy()
    environment["FINGERPRINT_PORT"] = str(backend_port)
    environment["FINGERPRINT_OUTPUT"] = str(Rust后端目录 / "target" / "benchmark_records.json")
    environment["FINGERPRINT_BENCHMARK_MODE"] = "1"
    if not 是管理员():
        # 自动化预检环境无法接受 UAC 时仅跳过 SYN 附加抓包；右键正常运行会自提权并保留完整抓包。
        environment["FINGERPRINT_DISABLE_TCP_CAPTURE"] = "1"
    backend = subprocess.Popen([str(executable)], cwd=Rust后端目录, env=environment, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    proxy = ThreadingHTTPServer(("127.0.0.1", 0), IPIPGO本地代理)
    threading.Thread(target=proxy.serve_forever, daemon=True).start()
    proxy_url = f"http://{代理用户名}:{代理密码}@127.0.0.1:{proxy.server_port}"
    target_url = f"https://127.0.0.1:{backend_port}/benchmark"
    try:
        等待端口(backend_port, backend)
        rows = []
        for total in 请求规模列表:
            for name in ("requests_rust", "curl_cffi", "rnet", "requests"):
                result = await 运行一轮(name, total, target_url, proxy_url)
                rows.append(result)
                print(json.dumps(result, ensure_ascii=False))
        with IPIPGO本地代理.记录锁:
            assert IPIPGO本地代理.命中用户[代理用户名] >= len(rows) * 并发Worker数
        打印表格(rows)
        (项目目录 / "benchmark_proxy_rust_vs_curl_cffi_results.json").write_text(json.dumps(rows, ensure_ascii=False, indent=2), encoding="utf-8")
    finally:
        proxy.shutdown()
        proxy.server_close()
        backend.terminate()
        backend.wait(timeout=5)


if __name__ == "__main__":
    if "--elevated" not in sys.argv and not 是管理员():
        自动管理员重启()
    else:
        asyncio.run(main())
