from __future__ import annotations

import asyncio
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import time
from urllib.parse import urlsplit


# 【可调参数】本测试只访问本机QUIC服务，不使用代理或外网。
测试版本 = "chrome150"
项目目录 = Path(__file__).resolve().parent
捕获器目录 = 项目目录.parent
服务目录 = 项目目录 / "tests" / "http3_server"
启动超时秒 = 20


def 获取UDP端口() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def Cargo环境() -> tuple[dict[str, str], Path]:
    environment = os.environ.copy()
    cargo_home = Path(r"D:\Rust\cargo")
    rustup_home = Path(r"D:\Rust\rustup")
    target_dir = Path(r"D:\BuildCache\requests-rust-http3-test-target")
    if cargo_home.is_dir() and rustup_home.is_dir():
        environment["CARGO_HOME"] = str(cargo_home)
        environment["RUSTUP_HOME"] = str(rustup_home)
        environment["PATH"] = str(cargo_home / "bin") + os.pathsep + environment.get("PATH", "")
    else:
        target_dir = 服务目录 / "target"
    environment["CARGO_TARGET_DIR"] = str(target_dir)
    executable = target_dir / "release" / ("http3-test-server.exe" if os.name == "nt" else "http3-test-server")
    return environment, executable


def 启动服务(port: int) -> subprocess.Popen[str]:
    environment, executable = Cargo环境()
    cargo = shutil.which("cargo", path=environment.get("PATH"))
    if cargo is None:
        raise RuntimeError("没有找到cargo，无法构建本地HTTP/3测试服务")
    subprocess.run(
        [cargo, "build", "--release", "--manifest-path", str(服务目录 / "Cargo.toml")],
        cwd=项目目录,
        env=environment,
        check=True,
    )
    environment["HTTP3_TEST_PORT"] = str(port)
    process = subprocess.Popen(
        [str(executable)],
        cwd=项目目录,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    deadline = time.monotonic() + 启动超时秒
    while time.monotonic() < deadline:
        line = process.stdout.readline().strip() if process.stdout else ""
        if line.startswith("HTTP3_READY="):
            return process
        if process.poll() is not None:
            output = process.stdout.read() if process.stdout else ""
            raise RuntimeError(f"HTTP/3测试服务提前退出: {line}\n{output}")
    process.terminate()
    raise TimeoutError("HTTP/3测试服务启动超时")


def 验证同步(url: str) -> None:
    from requests_rust import Session

    with Session(
        impersonate=测试版本,
        verify=False,
        http_version="v3only",
        max_connections=20,
    ) as session:
        first = session.get(
            url + "/echo",
            params={"page": 1},
            headers={"X-Echo": "sync"},
            cookies={"explicit": "yes"},
        )
        assert first.http_version == "HTTP/3"
        first_data = first.json()
        assert first_data["method"] == "GET"
        assert first_data["query"] == "page=1"
        assert first_data["x_echo"] == "sync"
        assert "explicit=yes" in first_data["cookie"]
        connection_id = first.headers["x-http3-connection-id"]

        second = session.post(url + "/echo", data="sync-body")
        assert second.http_version == "HTTP/3"
        assert second.json()["body"] == "sync-body"
        assert second.headers["x-http3-connection-id"] == connection_id

        redirected = session.get(url + "/redirect")
        assert redirected.url.endswith("/final")
        assert len(redirected.history) == 1
        assert redirected.history[0].status_code == 302
        assert "redirected=1" in redirected.json()["cookie"]

        streamed = session.get(url + "/stream", stream=True)
        assert streamed.http_version == "HTTP/3"
        assert streamed.content
        streamed.close()

    with Session(impersonate=测试版本, verify=False) as session:
        overridden = session.get(url + "/echo", http_version="v3")
        assert overridden.http_version == "HTTP/3"

    port = urlsplit(url).port
    with Session(
        impersonate=测试版本,
        verify=False,
        http_version="http3",
        resolve={"http3.test": ["127.0.0.1"]},
    ) as session:
        resolved = session.get(f"https://http3.test:{port}/echo")
        assert resolved.http_version == "HTTP/3"
        assert resolved.json()["path"] == "/echo"


def 验证拒绝边界(url: str) -> None:
    from requests_rust import Session

    try:
        Session(impersonate=测试版本, http_version="invalid")
    except ValueError:
        pass
    else:
        raise AssertionError("无效http_version没有被拒绝")

    with Session(
        impersonate=测试版本,
        verify=False,
        http_version="http3",
        proxy="http://127.0.0.1:1",
    ) as session:
        try:
            session.get(url + "/echo")
        except ValueError as error:
            assert "QUIC需要UDP" in str(error)
        else:
            raise AssertionError("HTTP/3代理请求没有被拒绝")

    with Session(impersonate=测试版本, verify=False, http_version="http3") as session:
        try:
            session.get("http://127.0.0.1/")
        except RuntimeError as error:
            assert "只支持https" in str(error)
        else:
            raise AssertionError("HTTP/3明文URL没有被拒绝")
        try:
            session.get(url + "/echo", transfer_stats=True)
        except ValueError as error:
            assert "transfer_stats" in str(error)
        else:
            raise AssertionError("HTTP/3 transfer_stats没有被拒绝")
        try:
            session.post(url + "/echo", files={"document": Path(__file__)})
        except ValueError as error:
            assert "multipart" in str(error)
        else:
            raise AssertionError("HTTP/3 multipart没有被拒绝")

    with Session(
        impersonate=测试版本,
        verify=False,
        http_version="http3",
        max_response_bytes=32,
    ) as session:
        try:
            session.get(url + "/echo")
        except RuntimeError as error:
            assert "max_response_bytes" in str(error)
        else:
            raise AssertionError("HTTP/3响应Body上限没有生效")


async def 验证异步(url: str) -> None:
    from requests_rust import AsyncSession

    async with AsyncSession(
        impersonate=测试版本,
        verify=False,
        http_version="v3",
        max_connections=20,
    ) as session:
        responses = await asyncio.gather(
            *(session.post(url + "/echo", data=f"body-{index}") for index in range(10))
        )
        assert all(response.http_version == "HTTP/3" for response in responses)
        assert [response.json()["body"] for response in responses] == [
            f"body-{index}" for index in range(10)
        ]
        assert len({response.headers["x-http3-connection-id"] for response in responses}) == 1

        streamed = await session.get(url + "/stream", stream=True)
        assert streamed.http_version == "HTTP/3"
        assert await streamed.aread()
        await streamed.aclose()

        pending = asyncio.create_task(session.get(url + "/slow"))
        await asyncio.sleep(0.05)
        pending.cancel()
        try:
            await pending
        except asyncio.CancelledError:
            pass
        else:
            raise AssertionError("HTTP/3异步请求没有被取消")
        sentinel = await session.get(url + "/echo")
        assert sentinel.http_version == "HTTP/3"


def main() -> None:
    port = 获取UDP端口()
    process = 启动服务(port)
    url = f"https://127.0.0.1:{port}"
    try:
        验证同步(url)
        asyncio.run(验证异步(url))
        验证拒绝边界(url)
        print("HTTP/3同步、异步、连接复用、重定向、Cookie、流和拒绝边界验证通过")
    finally:
        process.terminate()
        process.wait(timeout=5)


if __name__ == "__main__":
    main()
