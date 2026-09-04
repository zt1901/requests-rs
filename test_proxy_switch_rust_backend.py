from __future__ import annotations

import asyncio
import ctypes
import os
import select
import shutil
import socket
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


# ══════════════════════════════════════════════════
# 【可调参数】本测试自行启动本地 Rust 指纹后端和两个真实 CONNECT 转发代理，不访问外网。
项目目录 = Path(__file__).resolve().parent
捕获器目录 = 项目目录.parent
Rust后端目录 = 捕获器目录 / "rust_fingerprint_server"
测试指纹版本 = "chrome150"
并发请求数 = 20
后端同时捕获数 = 4
启动超时秒 = 20
# ══════════════════════════════════════════════════


def 获取空闲端口() -> int:
    with socket.socket() as server:
        server.bind(("127.0.0.1", 0))
        return server.getsockname()[1]


def 是管理员() -> bool:
    try:
        return bool(ctypes.windll.shell32.IsUserAnAdmin())
    except AttributeError:
        return False


def 自动管理员重启() -> None:
    """右键运行时自动请求 UAC，以启用 Rust 后端的完整 WinDivert TCP 采集。"""
    arguments = " ".join(f'"{argument}"' for argument in sys.argv[1:] if argument != "--elevated")
    command = (
        f"Start-Process -FilePath '{sys.executable}' "
        f"-ArgumentList '" + f'"{Path(__file__).resolve()}" --elevated {arguments}' + "' -Verb RunAs -Wait"
    )
    result = subprocess.run(["powershell", "-NoProfile", "-Command", command], check=False)
    if result.returncode:
        raise RuntimeError("管理员权限请求被取消或管理员子进程执行失败")


def 等待端口(port: int, process: subprocess.Popen[bytes]) -> None:
    deadline = time.monotonic() + 启动超时秒
    while time.monotonic() < deadline:
        if process.poll() is not None:
            output = process.stdout.read().decode("utf-8", errors="replace") if process.stdout else ""
            raise RuntimeError(f"Rust指纹后端异常退出: {output}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise TimeoutError("Rust指纹后端启动超时")


class 转发代理处理器(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    代理名称 = ""
    命中记录: list[str] = []
    记录锁 = threading.Lock()

    def log_message(self, *_: object) -> None:
        pass

    @classmethod
    def 清空记录(cls) -> None:
        with cls.记录锁:
            cls.命中记录.clear()

    @classmethod
    def 命中次数(cls) -> int:
        with cls.记录锁:
            return len(cls.命中记录)

    def do_CONNECT(self) -> None:
        host, separator, port_text = self.path.rpartition(":")
        if not separator:
            self.send_error(400, "CONNECT目标缺少端口")
            return
        try:
            upstream = socket.create_connection((host, int(port_text)), timeout=5)
        except OSError as error:
            self.send_error(502, str(error))
            return
        with type(self).记录锁:
            type(self).命中记录.append(type(self).代理名称)
        self.send_response(200, "Connection Established")
        self.end_headers()
        self.wfile.flush()
        self.close_connection = True
        self.connection.setblocking(False)
        upstream.setblocking(False)
        try:
            # CONNECT 成功后的 TLS/HTTP2 字节只做透明双向转发，不修改任何指纹字节。
            while True:
                readable, _, exceptional = select.select((self.connection, upstream), (), (self.connection, upstream), 10)
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


def 创建代理处理器(name: str):
    return type(f"转发代理_{name}", (转发代理处理器,), {"代理名称": name, "命中记录": [], "记录锁": threading.Lock()})


def 启动代理(handler: type[转发代理处理器]) -> ThreadingHTTPServer:
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


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


def 断言Rust后端响应(response) -> None:
    response.raise_for_status()
    body = response.json()
    assert body == {}


async def 测试异步全部代理入口(target_url: str, proxy_a: str, proxy_b: str, handler_a, handler_b) -> None:
    from requests_rs import requests
    AsyncSession = requests.AsyncSession

    async with AsyncSession(impersonate=测试指纹版本, proxy=proxy_a, verify=False) as session:
        断言Rust后端响应(await session.get(target_url + "?async=constructor"))
        session.set_proxy(proxy_b)
        断言Rust后端响应(await session.get(target_url + "?async=set_proxy"))
        断言Rust后端响应(await session.get(target_url + "?async=request_proxy", proxy=proxy_a))
        断言Rust后端响应(await session.get(target_url + "?async=request_proxies", proxies={"https": proxy_b}))
        断言Rust后端响应(await session.get(target_url + "?async=direct", proxy=None))

        handler_a.清空记录()
        handler_b.清空记录()
        semaphore = asyncio.Semaphore(后端同时捕获数)
        async def 请求级代理(index: int) -> None:
            proxy = proxy_a if index % 2 == 0 else proxy_b
            # Rust 对撞后端每条 H2 响应主动 GOAWAY，因此并发压力使用独立连接，
            # 避免把后端单连接限制误判为代理选择或 Session 切换失败。
            async with semaphore:
                async with AsyncSession(impersonate=测试指纹版本, proxy=proxy, verify=False) as concurrent_session:
                    response = await concurrent_session.get(target_url + f"?async=override-{index}")
                    断言Rust后端响应(response)

        await asyncio.gather(*(请求级代理(index) for index in range(并发请求数)))
        assert handler_a.命中次数() == 并发请求数 // 2
        assert handler_b.命中次数() == 并发请求数 // 2

        for name, proxy, handler in (("A", proxy_a, handler_a), ("B", proxy_b, handler_b)):
            handler_a.清空记录()
            handler_b.清空记录()
            session.set_proxy(proxy)
            # 同一 Session 的 set_proxy 波次只验证默认代理替换；后端 GOAWAY 后不复用该连接。
            断言Rust后端响应(await session.get(target_url + f"?async=wave-{name}"))
            assert handler.命中次数() == 1
            other = handler_b if handler is handler_a else handler_a
            assert other.命中次数() == 0


def main() -> None:
    from requests_rs import requests
    Session = requests.Session
    get = requests.get

    backend_port = 获取空闲端口()
    executable = 准备Rust后端()
    environment = os.environ.copy()
    environment["FINGERPRINT_PORT"] = str(backend_port)
    environment["FINGERPRINT_OUTPUT"] = str(Rust后端目录 / "target" / "proxy_switch_records.json")
    if not 是管理员():
        # 自动化预检环境无法接受 UAC 时仅跳过 SYN 附加抓包；右键正常运行会自提权并保留完整抓包。
        environment["FINGERPRINT_DISABLE_TCP_CAPTURE"] = "1"
    # 代理切换验证使用 Rust 后端 keep-alive 模式；TLS/HTTP2 对撞由 test_python_package.py 独立覆盖。
    environment["FINGERPRINT_BENCHMARK_MODE"] = "1"
    backend = subprocess.Popen([str(executable)], cwd=Rust后端目录, env=environment, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    handler_a = 创建代理处理器("A")
    handler_b = 创建代理处理器("B")
    server_a = 启动代理(handler_a)
    server_b = 启动代理(handler_b)
    proxy_a = f"http://127.0.0.1:{server_a.server_port}"
    proxy_b = f"http://127.0.0.1:{server_b.server_port}"
    target_url = f"https://127.0.0.1:{backend_port}/api/fingerprint"
    try:
        等待端口(backend_port, backend)

        with Session(impersonate=测试指纹版本, proxy=proxy_a, verify=False) as session:
            断言Rust后端响应(session.get(target_url + "?sync=constructor"))
            session.set_proxy(proxy_b)
            断言Rust后端响应(session.get(target_url + "?sync=set_proxy"))
            断言Rust后端响应(session.get(target_url + "?sync=request_proxy", proxy=proxy_a))
            断言Rust后端响应(session.get(target_url + "?sync=request_proxies", proxies={"https": proxy_b}))

        # proxy=None 需要覆盖 Session 默认代理；单独 Session 避免与前序 CONNECT 连接池复用混淆验证结果。
        with Session(impersonate=测试指纹版本, proxy=proxy_a, verify=False) as session:
            断言Rust后端响应(session.get(target_url + "?sync=direct", proxy=None))

        with Session(impersonate=测试指纹版本, proxies={"https": proxy_a}, verify=False) as session:
            handler_a.清空记录()
            handler_b.清空记录()
            断言Rust后端响应(session.get(target_url + "?sync=session_proxies"))
            assert handler_a.命中次数() == 1
            session.set_proxy(proxy_b)
            断言Rust后端响应(session.get(target_url + "?sync=session_proxies_set_proxy"))
            assert handler_b.命中次数() == 1
        断言Rust后端响应(get(target_url + "?sync=shortcut_proxy", impersonate=测试指纹版本, proxy=proxy_b, verify=False))
        断言Rust后端响应(get(target_url + "?sync=shortcut_proxies", impersonate=测试指纹版本, proxies={"https": proxy_a}, verify=False))

        asyncio.run(测试异步全部代理入口(target_url, proxy_a, proxy_b, handler_a, handler_b))
        print(f"代理API全覆盖与{并发请求数}任务、{后端同时捕获数}并发切换验证通过")
    finally:
        server_b.shutdown()
        server_b.server_close()
        server_a.shutdown()
        server_a.server_close()
        backend.terminate()
        backend.wait(timeout=5)


if __name__ == "__main__":
    if "--elevated" not in sys.argv and not 是管理员():
        自动管理员重启()
    else:
        main()
