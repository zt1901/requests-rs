from __future__ import annotations

import asyncio
import gzip
import os
import socketserver
import threading

import psutil


# 可右键运行；所有极端响应和并发等待均使用本机回环服务。
测试版本 = "chrome146"
解压后字节数 = 8 * 1024 * 1024
响应上限字节数 = 1024 * 1024
等待任务数 = 1000


class 极端处理器(socketserver.StreamRequestHandler):
    响应闸门 = threading.Event()

    def handle(self) -> None:
        request_line = self.rfile.readline().decode("latin1").strip()
        while self.rfile.readline() not in {b"\r\n", b"\n", b""}:
            pass
        path = request_line.split(" ", 2)[1]
        if path == "/gzip":
            body = gzip.compress(b"Z" * 解压后字节数)
            self.wfile.write(
                b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n"
                + f"Content-Length: {len(body)}\r\nConnection: close\r\n\r\n".encode()
                + body
            )
        elif path == "/hold":
            self.wfile.write(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n")
            self.wfile.flush()
            type(self).响应闸门.wait(timeout=10)
            self.wfile.write(b"X")
        elif path == "/short-body":
            self.wfile.write(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nBAD")
        elif path == "/bad-chunk":
            self.wfile.write(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nZ\r\nBAD\r\n0\r\n\r\n"
            )
        elif path == "/gzip-truncated":
            body = gzip.compress(b"truncated" * 1000)[:-5]
            self.wfile.write(
                b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n"
                + f"Content-Length: {len(body)}\r\nConnection: close\r\n\r\n".encode()
                + body
            )
        elif path.startswith("/body/"):
            size = int(path.rsplit("/", 1)[1])
            body = b"B" * size
            self.wfile.write(
                b"HTTP/1.1 200 OK\r\n"
                + f"Content-Length: {size}\r\nConnection: close\r\n\r\n".encode()
                + body
            )
        elif path == "/redirect-loop":
            self.wfile.write(
                b"HTTP/1.1 302 Found\r\nLocation: /redirect-loop\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
        else:
            self.wfile.write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
        self.wfile.flush()


class 线程服务(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
    request_queue_size = 2048


class CONNECT记录处理器(socketserver.StreamRequestHandler):
    请求行 = ""

    def handle(self) -> None:
        type(self).请求行 = self.rfile.readline().decode("latin1").strip()
        while self.rfile.readline() not in {b"\r\n", b"\n", b""}:
            pass
        self.wfile.write(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")


class 切换代理处理器(socketserver.StreamRequestHandler):
    标识 = b"?"

    def handle(self) -> None:
        while True:
            request_line = self.rfile.readline()
            if not request_line:
                return
            while self.rfile.readline() not in {b"\r\n", b"\n", b""}:
                pass
            body = type(self).标识
            self.wfile.write(
                b"HTTP/1.1 200 OK\r\n"
                + f"Content-Length: {len(body)}\r\nConnection: keep-alive\r\n\r\n".encode()
                + body
            )
            self.wfile.flush()


async def 验证极端(url: str, proxy_port: int) -> None:
    from requests_rs import requests
    AsyncSession = requests.AsyncSession

    try:
        AsyncSession(impersonate=测试版本, max_connections=2**63 - 1)
    except ValueError:
        pass
    else:
        raise AssertionError("极大max_connections没有被拒绝")

    async with AsyncSession(
        impersonate=测试版本,
        max_response_bytes=响应上限字节数,
    ) as session:
        try:
            await session.get(url + "/gzip")
        except RuntimeError as error:
            assert "max_response_bytes" in str(error)
        else:
            raise AssertionError("解压后超大响应没有被拒绝")

        exact = await session.get(url + f"/body/{响应上限字节数}")
        assert len(exact.content) == 响应上限字节数
        try:
            await session.get(url + f"/body/{响应上限字节数 + 1}")
        except RuntimeError as error:
            assert "max_response_bytes" in str(error)
        else:
            raise AssertionError("响应Body上限+1字节没有被拒绝")

        response = await session.get(url + "/gzip", stream=True)
        received = 0
        async for chunk in response.aiter_content(64 * 1024):
            received += len(chunk)
        assert received == 解压后字节数

        for path in ("/short-body", "/bad-chunk", "/gzip-truncated"):
            try:
                await session.get(url + path)
            except RuntimeError:
                pass
            else:
                raise AssertionError(f"畸形HTTP响应没有失败: {path}")
            sentinel = await session.get(url + "/ok")
            assert sentinel.content == b"OK"

        try:
            await session.get(url + "/redirect-loop", max_redirects=3)
        except RuntimeError:
            pass
        else:
            raise AssertionError("重定向环没有按max_redirects终止")

        for kwargs in (
            {"headers": {"Bad\nName": "value"}},
            {"headers": {"X-Test": "bad\r\nvalue"}},
            {"timeout": float("nan")},
            {"timeout": float("inf")},
        ):
            try:
                await session.get(url + "/ok", **kwargs)
            except (TypeError, ValueError, RuntimeError):
                pass
            else:
                raise AssertionError(f"非法请求参数没有被拒绝: {kwargs}")

    session = AsyncSession(
        impersonate=测试版本,
        max_connections=1,
        max_cached_origins=4,
    )
    response = await session.get(url + "/hold", stream=True)
    process = psutil.Process(os.getpid())
    initial_rss = process.memory_info().rss
    initial_threads = process.num_threads()
    tasks = [
        asyncio.create_task(
            session.get(
                f"http://extreme-{index}.test:{url.rsplit(':', 1)[1]}/ok",
                dns_servers=[f"127.0.0.1:{10000 + index}"],
                dns_timeout=0.1,
            )
        )
        for index in range(等待任务数)
    ]
    await asyncio.sleep(0.2)
    assert not any(task.done() for task in tasks)
    rss_delta = process.memory_info().rss - initial_rss
    thread_delta = process.num_threads() - initial_threads
    assert rss_delta < 32 * 1024 * 1024, rss_delta
    assert thread_delta <= 4, thread_delta
    await session.close()
    results = await asyncio.gather(*tasks, return_exceptions=True)
    assert all(isinstance(result, RuntimeError) for result in results)
    极端处理器.响应闸门.set()
    await response.aclose()

    极端处理器.响应闸门.clear()
    cancelling = AsyncSession(impersonate=测试版本, max_connections=1)
    held = await cancelling.get(url + "/hold", stream=True)
    cancel_initial_rss = process.memory_info().rss
    cancel_initial_threads = process.num_threads()
    cancel_tasks = [
        asyncio.create_task(
            cancelling.get(
                f"http://cancel-{index}.test:{url.rsplit(':', 1)[1]}/ok",
                dns_servers=[f"127.0.0.1:{30000 + index}"],
                dns_timeout=0.1,
            )
        )
        for index in range(等待任务数)
    ]
    await asyncio.sleep(0.1)
    for task in cancel_tasks:
        task.cancel()
    cancelled = await asyncio.gather(*cancel_tasks, return_exceptions=True)
    assert all(isinstance(result, asyncio.CancelledError) for result in cancelled)
    await asyncio.sleep(0.2)
    assert process.memory_info().rss - cancel_initial_rss < 32 * 1024 * 1024
    assert process.num_threads() - cancel_initial_threads <= 4
    极端处理器.响应闸门.set()
    await held.aclose()
    sentinel = await asyncio.wait_for(cancelling.get(url + "/ok"), 2)
    assert sentinel.content == b"OK"
    await cancelling.close()

    async with AsyncSession(impersonate=测试版本, verify=False) as ipv6_session:
        try:
            await ipv6_session.get(
                "https://[::1]:443/",
                proxy=f"http://127.0.0.1:{proxy_port}",
                transfer_stats=True,
                timeout=1,
            )
        except RuntimeError:
            pass
        else:
            raise AssertionError("无目标TLS服务时IPv6计量请求意外成功")
    assert CONNECT记录处理器.请求行 == "CONNECT [::1]:443 HTTP/1.1"
    print(
        f"极端参数、gzip解压上限和1000等待任务验证通过："
        f"RSS增量={rss_delta / 1024 / 1024:.2f}MB，线程增量={thread_delta}，"
        f"1000取消任务无后台构建膨胀"
    )


async def 验证代理切换竞态(proxy_a: int, proxy_b: int) -> None:
    from requests_rs import requests
    AsyncSession = requests.AsyncSession

    session = AsyncSession(
        impersonate=测试版本,
        proxy=f"http://127.0.0.1:{proxy_a}",
        max_connections=100,
        max_cached_origins=4,
    )

    async def toggler() -> None:
        for index in range(500):
            session.set_proxy(
                f"http://127.0.0.1:{proxy_a if index % 2 == 0 else proxy_b}"
            )
            await asyncio.sleep(0)

    async def request() -> bytes:
        response = await session.get("http://proxy-race.test/value", timeout=5)
        return response.content

    try:
        toggle_task = asyncio.create_task(toggler())
        results = await asyncio.gather(*(request() for _ in range(500)))
        await toggle_task
        assert set(results) <= {b"A", b"B"}
        assert len(results) == 500
    finally:
        await session.close()
    print("默认代理原子快照500请求竞态验证通过")

    closing = AsyncSession(impersonate=测试版本)
    await asyncio.gather(*(closing.close() for _ in range(100)))
    try:
        await closing.get("http://closed-session.test/")
    except RuntimeError as error:
        assert "关闭" in str(error)
    else:
        raise AssertionError("关闭100次后的Session仍可请求")
    print("Session并发关闭100次幂等验证通过")


def main() -> None:
    server = 线程服务(("127.0.0.1", 0), 极端处理器)
    proxy_server = 线程服务(("127.0.0.1", 0), CONNECT记录处理器)
    proxy_a_handler = type("代理A处理器", (切换代理处理器,), {"标识": b"A"})
    proxy_b_handler = type("代理B处理器", (切换代理处理器,), {"标识": b"B"})
    proxy_a_server = 线程服务(("127.0.0.1", 0), proxy_a_handler)
    proxy_b_server = 线程服务(("127.0.0.1", 0), proxy_b_handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    proxy_thread = threading.Thread(target=proxy_server.serve_forever, daemon=True)
    proxy_a_thread = threading.Thread(target=proxy_a_server.serve_forever, daemon=True)
    proxy_b_thread = threading.Thread(target=proxy_b_server.serve_forever, daemon=True)
    thread.start()
    proxy_thread.start()
    proxy_a_thread.start()
    proxy_b_thread.start()
    try:
        asyncio.run(
            验证极端(
                f"http://127.0.0.1:{server.server_address[1]}",
                proxy_server.server_address[1],
            )
        )
        asyncio.run(
            验证代理切换竞态(
                proxy_a_server.server_address[1],
                proxy_b_server.server_address[1],
            )
        )
    finally:
        极端处理器.响应闸门.set()
        server.shutdown()
        server.server_close()
        proxy_server.shutdown()
        proxy_server.server_close()
        proxy_a_server.shutdown()
        proxy_a_server.server_close()
        proxy_b_server.shutdown()
        proxy_b_server.server_close()
        thread.join(timeout=2)
        proxy_thread.join(timeout=2)
        proxy_a_thread.join(timeout=2)
        proxy_b_thread.join(timeout=2)


if __name__ == "__main__":
    main()
