from __future__ import annotations

import asyncio
import base64
import socketserver
import threading


# 可右键运行；本测试仅使用本机HTTP代理回显，不访问外网。
测试版本 = "firefox151"
代理密码 = "pool-password"


class 代理处理器(socketserver.StreamRequestHandler):
    连接记录: set[int] = set()
    认证记录: list[str] = []
    锁 = threading.Lock()

    def handle(self) -> None:
        client_port = self.client_address[1]
        with type(self).锁:
            type(self).连接记录.add(client_port)
        while True:
            request_line = self.rfile.readline()
            if not request_line:
                return
            headers = {}
            while line := self.rfile.readline():
                if line in {b"\r\n", b"\n"}:
                    break
                name, value = line.decode("latin1").split(":", 1)
                headers[name.lower()] = value.strip()
            authorization = headers.get("proxy-authorization", "")
            if authorization.startswith("Basic "):
                username = base64.b64decode(authorization[6:]).decode().split(":", 1)[0]
                with type(self).锁:
                    type(self).认证记录.append(username)
            body = b"pool-ok"
            self.wfile.write(
                b"HTTP/1.1 200 OK\r\n"
                + f"Content-Length: {len(body)}\r\n".encode()
                + b"Connection: keep-alive\r\n\r\n"
                + body
            )
            self.wfile.flush()


class 线程代理服务(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
    request_queue_size = 128


def 代理地址(port: int, session_id: str) -> str:
    return f"http://pool-{session_id}:{代理密码}@127.0.0.1:{port}"


async def 验证自然指纹池(port: int) -> None:
    from requests_rust import AsyncSession

    target = "http://pool-target.test/resource"
    proxy_a = 代理地址(port, "session-a")
    proxy_b = 代理地址(port, "session-b")
    async with AsyncSession(
        impersonate=测试版本,
        fingerprint_rotation=True,
        max_connections=50,
    ) as session:
        responses = []
        for _ in range(session.fingerprint_count):
            responses.append(await session.get(target, proxy=proxy_a))
        assert all(response.content == b"pool-ok" for response in responses)
        ids = [response.fingerprint_id for response in responses]
        assert len(set(ids)) == session.fingerprint_count, (ids, session.fingerprint_pool_count)
        assert session.fingerprint_pool_count == session.fingerprint_count
        with 代理处理器.锁:
            warmed_connections = len(代理处理器.连接记录)
        assert warmed_connections == session.fingerprint_count

        responses = []
        for _ in range(session.fingerprint_count):
            responses.append(await session.get(target, proxy=proxy_a))
        assert all(response.content == b"pool-ok" for response in responses)
        with 代理处理器.锁:
            reused_connections = len(代理处理器.连接记录)
        assert reused_connections == warmed_connections

        responses = []
        for _ in range(session.fingerprint_count):
            responses.append(await session.get(target, proxy=proxy_b))
        assert all(response.content == b"pool-ok" for response in responses)
        with 代理处理器.锁:
            new_proxy_connections = len(代理处理器.连接记录)
            usernames = set(代理处理器.认证记录)
        assert new_proxy_connections > warmed_connections
        assert new_proxy_connections <= warmed_connections * 2
        assert usernames == {"pool-session-a", "pool-session-b"}


    for chromium_profile in ("chrome150", "edge152"):
        with 代理处理器.锁:
            before_chromium = len(代理处理器.连接记录)
        async with AsyncSession(
            impersonate=chromium_profile,
            max_cached_origins=4,
            max_connections=20,
        ) as session:
            chromium_proxy = 代理地址(port, f"{chromium_profile}-reuse")
            first = await session.get(target, proxy=chromium_proxy)
            second = await session.get(target, proxy=chromium_proxy)
            assert first.content == second.content == b"pool-ok"
            assert session.cached_origin_count == 1
        with 代理处理器.锁:
            chromium_connections = len(代理处理器.连接记录) - before_chromium
        assert chromium_connections == 1

    before_origin_test = len(代理处理器.连接记录)
    async with AsyncSession(
        impersonate="firefox151",
        max_cached_origins=1,
        max_connections=20,
    ) as session:
        first_proxy = 代理地址(port, "origin-cache")
        first = await session.get(
            "http://origin-one.test/api",
            params={"page": 1},
            proxy=first_proxy,
        )
        second = await session.get(
            "http://origin-one.test/other",
            params={"page": 2},
            proxy=first_proxy,
        )
        assert first.content == second.content == b"pool-ok"
        assert session.cached_origin_count == 1
        with 代理处理器.锁:
            same_origin_connections = len(代理处理器.连接记录) - before_origin_test
        assert same_origin_connections == 1

        await session.get("http://origin-two.test/a", proxy=first_proxy)
        await session.get("http://origin-two.test/b", proxy=first_proxy)
        assert session.cached_origin_count == 1
        with 代理处理器.锁:
            all_origin_connections = len(代理处理器.连接记录) - before_origin_test
        assert all_origin_connections == 3

    async with AsyncSession(
        impersonate=测试版本,
        fingerprint_rotation=True,
        fingerprint_pool=False,
        max_connections=20,
    ) as session:
        for _ in range(10):
            response = await session.get(target, proxy=proxy_a)
            assert response.content == b"pool-ok"
        assert session.fingerprint_pool_count == 0

    async with AsyncSession(
        impersonate="firefox151",
        max_cached_origins=4,
    ) as session:
        for index in range(4):
            try:
                await session.get(
                    f"http://failed-origin-{index}.test/",
                    dns_servers=["127.0.0.1:1"],
                    dns_timeout=0.1,
                    timeout=1,
                )
            except RuntimeError:
                pass
            else:
                raise AssertionError("不可解析Origin意外请求成功")
        assert session.cached_origin_count == 0
        response = await session.get(
            f"http://127.0.0.1:{port}/valid-origin",
        )
        assert response.content == b"pool-ok"
        assert session.cached_origin_count == 1


def main() -> None:
    server = 线程代理服务(("127.0.0.1", 0), 代理处理器)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        asyncio.run(验证自然指纹池(server.server_address[1]))
        print("自然指纹池、Origin缓存上限和代理session连接隔离验证通过")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


if __name__ == "__main__":
    main()
