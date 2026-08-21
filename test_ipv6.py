from __future__ import annotations

import asyncio
import socket
import socketserver
import threading


# 可右键运行；仅监听本机IPv6回环地址，不访问外网。
测试版本 = "chrome146"


class IPv6线程服务(socketserver.ThreadingTCPServer):
    address_family = socket.AF_INET6
    allow_reuse_address = True
    daemon_threads = True


class HTTP处理器(socketserver.StreamRequestHandler):
    def handle(self) -> None:
        request_line = self.rfile.readline().decode("latin1").strip()
        while self.rfile.readline() not in {b"\r\n", b"\n", b""}:
            pass
        body = f"ipv6-ok:{request_line}".encode()
        self.wfile.write(
            b"HTTP/1.1 200 OK\r\n"
            + f"Content-Length: {len(body)}\r\n".encode()
            + b"Connection: close\r\n\r\n"
            + body
        )


def 启动IPv6服务():
    try:
        server = IPv6线程服务(("::1", 0), HTTP处理器)
    except OSError as error:
        raise RuntimeError(f"当前系统没有可用的IPv6回环网络: {error}") from error
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, thread


async def 验证异步(url: str) -> None:
    from requests_rust import AsyncSession

    async with AsyncSession(
        impersonate=测试版本,
        max_connections=2,
        happy_eyeballs_timeout=0.3,
    ) as session:
        response = await session.get(url)
        assert response.status_code == 200
        assert response.content.startswith(b"ipv6-ok:GET")


def main() -> None:
    from requests_rust import Session

    server, thread = 启动IPv6服务()
    url = f"http://[::1]:{server.server_address[1]}/ipv6"
    try:
        with Session(
            impersonate=测试版本,
            happy_eyeballs_timeout=0.3,
        ) as session:
            response = session.get(url)
            assert response.status_code == 200
            assert response.content.startswith(b"ipv6-ok:GET")
        asyncio.run(验证异步(url))
        print("IPv6同步和异步HTTP直连验证通过")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


if __name__ == "__main__":
    main()
