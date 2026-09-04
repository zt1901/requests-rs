from __future__ import annotations

import asyncio
import socket
import socketserver
import struct
import threading


# 可右键运行；所有DNS、HTTP和SOCKS服务仅监听本机回环地址。
测试版本 = "chrome146"


def 读取DNS名称(packet: bytes, offset: int = 12) -> tuple[str, int]:
    labels = []
    while packet[offset]:
        length = packet[offset]
        offset += 1
        labels.append(packet[offset : offset + length].decode("ascii"))
        offset += length
    return ".".join(labels).lower(), offset + 1


class DNS状态:
    查询名称: list[str] = []
    TCP查询名称: list[str] = []
    锁 = threading.Lock()


def 构造DNS响应(packet: bytes) -> bytes:
    name, offset = 读取DNS名称(packet)
    query_type, _query_class = struct.unpack("!HH", packet[offset : offset + 4])
    question = packet[12 : offset + 4]
    with DNS状态.锁:
        DNS状态.查询名称.append(name)
    if query_type == 1:
        answer = b"\xc0\x0c" + struct.pack("!HHIH", 1, 1, 30, 4) + socket.inet_aton("127.0.0.1")
        answer_count = 1
    else:
        answer = b""
        answer_count = 0
    header = packet[:2] + struct.pack("!HHHHH", 0x8180, 1, answer_count, 0, 0)
    return header + question + answer


class UDPDNS处理器(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        data, sock = self.request
        name, offset = 读取DNS名称(data)
        if name == "tcp-fallback.test":
            question = data[12 : offset + 4]
            response = data[:2] + struct.pack("!HHHHH", 0x8380, 1, 0, 0, 0) + question
        else:
            response = 构造DNS响应(data)
        sock.sendto(response, self.client_address)


class TCPDNS处理器(socketserver.StreamRequestHandler):
    def handle(self) -> None:
        length_data = self.rfile.read(2)
        if len(length_data) != 2:
            return
        packet = self.rfile.read(struct.unpack("!H", length_data)[0])
        name, _offset = 读取DNS名称(packet)
        with DNS状态.锁:
            DNS状态.TCP查询名称.append(name)
        response = 构造DNS响应(packet)
        self.wfile.write(struct.pack("!H", len(response)) + response)


class 线程UDP服务(socketserver.ThreadingUDPServer):
    allow_reuse_address = True
    daemon_threads = True


class 线程TCP服务(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


class HTTP处理器(socketserver.StreamRequestHandler):
    请求行: list[str] = []
    锁 = threading.Lock()

    def handle(self) -> None:
        request_line = self.rfile.readline().decode("latin1").strip()
        while self.rfile.readline() not in {b"\r\n", b"\n", b""}:
            pass
        with type(self).锁:
            type(self).请求行.append(request_line)
        body = b"dns-ok"
        self.wfile.write(
            b"HTTP/1.1 200 OK\r\n"
            + f"Content-Length: {len(body)}\r\n".encode()
            + b"Connection: close\r\n\r\n"
            + body
        )


class SOCKS5处理器(socketserver.StreamRequestHandler):
    目标记录: list[tuple[int, str, int]] = []
    锁 = threading.Lock()

    def handle(self) -> None:
        version, methods = struct.unpack("!BB", self.rfile.read(2))
        assert version == 5
        self.rfile.read(methods)
        self.wfile.write(b"\x05\x00")
        version, command, _reserved, address_type = struct.unpack("!BBBB", self.rfile.read(4))
        assert (version, command) == (5, 1)
        if address_type == 1:
            target = socket.inet_ntoa(self.rfile.read(4))
        elif address_type == 3:
            target = self.rfile.read(self.rfile.read(1)[0]).decode("ascii")
        elif address_type == 4:
            target = socket.inet_ntop(socket.AF_INET6, self.rfile.read(16))
        else:
            raise AssertionError(f"未知SOCKS地址类型: {address_type}")
        port = struct.unpack("!H", self.rfile.read(2))[0]
        with type(self).锁:
            type(self).目标记录.append((address_type, target, port))
        upstream = socket.create_connection(("127.0.0.1", port), timeout=5)
        self.wfile.write(b"\x05\x00\x00\x01\x7f\x00\x00\x01\x00\x00")
        self.wfile.flush()

        def 转发(source, target_socket) -> None:
            try:
                while data := source.recv(65536):
                    target_socket.sendall(data)
            except OSError:
                pass

        client_socket = self.connection
        one = threading.Thread(target=转发, args=(client_socket, upstream), daemon=True)
        two = threading.Thread(target=转发, args=(upstream, client_socket), daemon=True)
        one.start()
        two.start()
        one.join(timeout=5)
        two.join(timeout=5)
        upstream.close()


def 启动服务(server) -> threading.Thread:
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return thread


async def 验证DNS(dns_port: int, http_port: int, proxy_port: int, socks_port: int) -> None:
    from requests_rs import requests
    AsyncSession = requests.AsyncSession

    dns_server = f"127.0.0.1:{dns_port}"
    async with AsyncSession(
        impersonate=测试版本,
        dns_servers=[dns_server],
        dns_timeout=2,
    ) as session:
        direct = await session.get(f"http://direct-dns.test:{http_port}/")
        assert direct.content == b"dns-ok"
        proxied = await session.get(
            "http://proxy-target.test/resource",
            proxy=f"http://proxy-dns.test:{proxy_port}",
        )
        assert proxied.content == b"dns-ok"
        local_socks = await session.get(
            f"http://socks-target.test:{http_port}/",
            proxy=f"socks5://socks-proxy.test:{socks_port}",
        )
        assert local_socks.content == b"dns-ok"
        remote_socks = await session.get(
            f"http://remote-target.test:{http_port}/",
            proxy=f"socks5h://socks-proxy.test:{socks_port}",
        )
        assert remote_socks.content == b"dns-ok"
        fallback = await session.get(f"http://tcp-fallback.test:{http_port}/")
        assert fallback.content == b"dns-ok"

    async with AsyncSession(
        impersonate=测试版本,
        dns_servers=["127.0.0.1:1"],
        dns_timeout=0.1,
        max_connections=20,
    ) as session:
        responses = await asyncio.gather(
            *(
                session.get(
                    f"http://request-dns.test:{http_port}/request-{index}",
                    dns_servers=[dns_server],
                    dns_timeout=2,
                )
                for index in range(20)
            )
        )
        assert all(response.content == b"dns-ok" for response in responses)
        try:
            await session.get(f"http://session-dns.test:{http_port}/", timeout=1)
        except RuntimeError:
            pass
        else:
            raise AssertionError("请求级DNS覆盖污染了Session默认DNS")
        assert session.request_dns_client_count == 1

    session = AsyncSession(
        impersonate="firefox151",
        max_connections=20,
        max_cached_origins=20,
    )
    try:
        for index in range(9):
            response = await session.get(
                f"http://dns-lru-{index}.test:{http_port}/",
                dns_servers=[dns_server, f"127.0.0.1:{20000 + index}"],
                dns_timeout=2,
            )
            assert response.content == b"dns-ok"
        assert session.request_dns_client_count == 8
        responses = await asyncio.gather(
            *(
                session.get(
                    f"http://dns-lru-concurrent.test:{http_port}/{index}",
                    dns_servers=[dns_server, "127.0.0.1:29999"],
                    dns_timeout=2,
                )
                for index in range(100)
            )
        )
        assert all(response.content == b"dns-ok" for response in responses)
        assert session.request_dns_client_count == 8
    finally:
        await session.close()
    assert session.request_dns_client_count == 0

    equivalent = AsyncSession(impersonate="firefox151")
    try:
        for server in (["127.0.0.1"], ["127.0.0.1:53"]):
            try:
                await equivalent.get(
                    "http://equivalent-dns.test/",
                    dns_servers=server,
                    dns_timeout=0.1,
                    timeout=1,
                )
            except RuntimeError:
                pass
        assert equivalent.request_dns_client_count == 1
    finally:
        await equivalent.close()


def main() -> None:
    from requests_rs import requests
    Session = requests.Session

    tcp_dns = 线程TCP服务(("127.0.0.1", 0), TCPDNS处理器)
    dns_port = tcp_dns.server_address[1]
    udp_dns = 线程UDP服务(("127.0.0.1", dns_port), UDPDNS处理器)
    http_server = 线程TCP服务(("127.0.0.1", 0), HTTP处理器)
    proxy_handler = type("HTTP代理处理器", (HTTP处理器,), {"请求行": [], "锁": threading.Lock()})
    proxy_server = 线程TCP服务(("127.0.0.1", 0), proxy_handler)
    socks_handler = type("SOCKS代理处理器", (SOCKS5处理器,), {"目标记录": [], "锁": threading.Lock()})
    socks_server = 线程TCP服务(("127.0.0.1", 0), socks_handler)
    servers = [udp_dns, tcp_dns, http_server, proxy_server, socks_server]
    threads = [启动服务(server) for server in servers]
    try:
        with Session(
            impersonate=测试版本,
            resolve={"static-dns.test": ["127.0.0.1"]},
        ) as session:
            response = session.get(f"http://static-dns.test:{http_server.server_address[1]}/")
            assert response.content == b"dns-ok"
        asyncio.run(
            验证DNS(
                dns_port,
                http_server.server_address[1],
                proxy_server.server_address[1],
                socks_server.server_address[1],
            )
        )
        with DNS状态.锁:
            queried = set(DNS状态.查询名称)
        assert {
            "direct-dns.test",
            "proxy-dns.test",
            "socks-proxy.test",
            "socks-target.test",
            "request-dns.test",
        } <= queried
        assert "proxy-target.test" not in queried
        assert "remote-target.test" not in queried
        assert "tcp-fallback.test" in DNS状态.TCP查询名称
        assert proxy_handler.请求行 == ["GET http://proxy-target.test/resource HTTP/1.1"]
        assert (1, "127.0.0.1", http_server.server_address[1]) in socks_handler.目标记录
        assert (3, "remote-target.test", http_server.server_address[1]) in socks_handler.目标记录
        print("静态DNS、自定义DNS、HTTP代理、SOCKS5和SOCKS5H解析语义验证通过")
    finally:
        for server in servers:
            server.shutdown()
            server.server_close()
        for thread in threads:
            thread.join(timeout=2)


if __name__ == "__main__":
    main()
