from __future__ import annotations

import asyncio
import base64
import gc
import hashlib
import select
import socket
import socketserver
import ssl
import struct
import threading
from pathlib import Path
from urllib.parse import quote


# 可右键运行；全部 WebSocket 服务和代理均在本机，不访问外网。
项目目录 = Path(__file__).resolve().parent
证书文件 = 项目目录.parent / "fingerprint_cert.pem"
私钥文件 = 项目目录.parent / "fingerprint_key.pem"
测试版本 = "chrome146"
代理用户名 = "ws-session"
代理密码 = "proxy:password"
动态代理连接数 = 50
预期代理认证 = "Basic " + base64.b64encode(
    f"{代理用户名}:{代理密码}".encode()
).decode()


def 读取定长(stream, length: int) -> bytes:
    data = bytearray()
    while len(data) < length:
        chunk = stream.read(length - len(data))
        if not chunk:
            raise ConnectionError("WebSocket连接提前关闭")
        data.extend(chunk)
    return bytes(data)


def 读取帧(stream) -> tuple[int, bytes]:
    first, second = 读取定长(stream, 2)
    opcode = first & 0x0F
    length = second & 0x7F
    if length == 126:
        length = struct.unpack("!H", 读取定长(stream, 2))[0]
    elif length == 127:
        length = struct.unpack("!Q", 读取定长(stream, 8))[0]
    mask = 读取定长(stream, 4) if second & 0x80 else b""
    payload = 读取定长(stream, length)
    if mask:
        payload = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
    return opcode, payload


def 发送帧(stream, opcode: int, payload: bytes = b"") -> None:
    header = bytearray([0x80 | opcode])
    if len(payload) < 126:
        header.append(len(payload))
    elif len(payload) <= 0xFFFF:
        header.append(126)
        header.extend(struct.pack("!H", len(payload)))
    else:
        header.append(127)
        header.extend(struct.pack("!Q", len(payload)))
    stream.write(bytes(header) + payload)
    stream.flush()


def 双向转发(client: socket.socket, upstream: socket.socket) -> None:
    client.setblocking(False)
    upstream.setblocking(False)
    while True:
        readable, _, exceptional = select.select(
            (client, upstream), (), (client, upstream), 10
        )
        if exceptional or not readable:
            return
        for source in readable:
            target = upstream if source is client else client
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


class WebSocket处理器(socketserver.StreamRequestHandler):
    要求代理认证 = False
    HTTP响应闸门: threading.Event | None = None
    握手记录: list[dict[str, str]] = []
    记录锁 = threading.Lock()

    def handle(self) -> None:
        request_line = self.rfile.readline().decode("latin1").strip()
        headers: dict[str, str] = {}
        while True:
            line = self.rfile.readline()
            if line in {b"\r\n", b"\n", b""}:
                break
            name, value = line.decode("latin1").split(":", 1)
            headers[name.lower()] = value.strip()

        with type(self).记录锁:
            type(self).握手记录.append(
                {
                    "request_line": request_line,
                    "authorization": headers.get("proxy-authorization", ""),
                    "cookie": headers.get("cookie", ""),
                    "custom": headers.get("x-websocket-test", ""),
                    "protocol": headers.get("sec-websocket-protocol", ""),
                    "client_port": str(self.client_address[1]),
                }
            )
        if type(self).要求代理认证 and headers.get("proxy-authorization") != 预期代理认证:
            self.wfile.write(
                b"HTTP/1.1 407 Proxy Authentication Required\r\n"
                b"Proxy-Authenticate: Basic realm=\"websocket-test\"\r\n"
                b"Content-Length: 0\r\nConnection: close\r\n\r\n"
            )
            self.wfile.flush()
            return

        if headers.get("upgrade", "").lower() != "websocket":
            body = b"mixed-http-ok"
            self.wfile.write(
                b"HTTP/1.1 200 OK\r\n"
                + f"Content-Length: {len(body)}\r\n".encode()
                + b"Connection: close\r\n\r\n"
            )
            self.wfile.flush()
            if type(self).HTTP响应闸门 is not None:
                type(self).HTTP响应闸门.wait(timeout=10)
            self.wfile.write(body)
            self.wfile.flush()
            return

        key = headers["sec-websocket-key"]
        accept = base64.b64encode(
            hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()
        ).decode()
        response = (
            "HTTP/1.1 101 Switching Protocols\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {accept}\r\n"
        )
        if "chat" in headers.get("sec-websocket-protocol", "").split(", "):
            response += "Sec-WebSocket-Protocol: chat\r\n"
        self.wfile.write((response + "\r\n").encode())
        self.wfile.flush()
        发送帧(self.wfile, 1, b"welcome")

        while True:
            opcode, payload = 读取帧(self.rfile)
            if opcode == 1:
                if payload == b"server-close":
                    发送帧(self.wfile, 8, struct.pack("!H", 4001) + "测试关闭".encode())
                    return
                发送帧(self.wfile, 1, b"echo:" + payload)
            elif opcode == 2:
                发送帧(self.wfile, 2, payload)
            elif opcode == 9:
                发送帧(self.wfile, 10, payload)
            elif opcode == 10:
                continue
            elif opcode == 8:
                发送帧(self.wfile, 8, payload)
                return


class 静默线程服务(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
    request_queue_size = 128

    def handle_error(self, *_: object) -> None:
        pass


class TLS线程服务(静默线程服务):
    def __init__(self, address, handler) -> None:
        super().__init__(address, handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(证书文件, 私钥文件)
        self.context = context

    def get_request(self):
        connection, address = super().get_request()
        return self.context.wrap_socket(connection, server_side=True), address


class CONNECT代理处理器(socketserver.StreamRequestHandler):
    目标端口 = 0
    请求记录: list[dict[str, str]] = []
    记录锁 = threading.Lock()

    def handle(self) -> None:
        request_line = self.rfile.readline().decode("latin1").strip()
        headers: dict[str, str] = {}
        while True:
            line = self.rfile.readline()
            if line in {b"\r\n", b"\n", b""}:
                break
            name, value = line.decode("latin1").split(":", 1)
            headers[name.lower()] = value.strip()
        with type(self).记录锁:
            type(self).请求记录.append(
                {
                    "request_line": request_line,
                    "authorization": headers.get("proxy-authorization", ""),
                }
            )
        if headers.get("proxy-authorization") != 预期代理认证:
            self.wfile.write(
                b"HTTP/1.1 407 Proxy Authentication Required\r\n"
                b"Proxy-Authenticate: Basic realm=\"websocket-connect\"\r\n"
                b"Content-Length: 0\r\nConnection: close\r\n\r\n"
            )
            self.wfile.flush()
            return
        upstream = socket.create_connection(("127.0.0.1", type(self).目标端口), timeout=5)
        try:
            self.wfile.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            self.wfile.flush()
            双向转发(self.connection, upstream)
        finally:
            upstream.close()


class SOCKS5代理处理器(socketserver.StreamRequestHandler):
    接受动态用户名 = False
    请求记录: list[dict[str, object]] = []
    记录锁 = threading.Lock()

    def handle(self) -> None:
        version, method_count = 读取定长(self.rfile, 2)
        methods = list(读取定长(self.rfile, method_count))
        if version != 5 or 2 not in methods:
            self.wfile.write(b"\x05\xff")
            return
        self.wfile.write(b"\x05\x02")
        self.wfile.flush()
        auth_version, username_length = 读取定长(self.rfile, 2)
        username = 读取定长(self.rfile, username_length).decode()
        password_length = 读取定长(self.rfile, 1)[0]
        password = 读取定长(self.rfile, password_length).decode()
        authenticated = (
            auth_version == 1
            and (
                username == 代理用户名
                or (
                    type(self).接受动态用户名
                    and username.startswith(("session-", "mixed-socks-"))
                )
            )
            and password == 代理密码
        )
        self.wfile.write(b"\x01\x00" if authenticated else b"\x01\x01")
        self.wfile.flush()
        if not authenticated:
            return

        request_version, command, reserved, address_type = 读取定长(self.rfile, 4)
        if address_type == 1:
            target_host = socket.inet_ntoa(读取定长(self.rfile, 4))
        elif address_type == 3:
            target_host = 读取定长(self.rfile, 读取定长(self.rfile, 1)[0]).decode()
        else:
            raise AssertionError(f"测试SOCKS5代理不支持地址类型{address_type}")
        target_port = int.from_bytes(读取定长(self.rfile, 2), "big")
        with type(self).记录锁:
            type(self).请求记录.append(
                {
                    "methods": methods,
                    "username": username,
                    "password": password,
                    "request": (request_version, command, reserved),
                    "target": (target_host, target_port),
                    "client_port": self.client_address[1],
                }
            )
        upstream = socket.create_connection(("127.0.0.1", target_port), timeout=5)
        try:
            self.wfile.write(b"\x05\x00\x00\x01\x7f\x00\x00\x01\x00\x00")
            self.wfile.flush()
            双向转发(self.connection, upstream)
        finally:
            upstream.close()


def 启动服务(handler, *, tls: bool = False):
    server_type = TLS线程服务 if tls else 静默线程服务
    server = server_type(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, thread


def 创建处理器(name: str, *, proxy_auth: bool = False):
    return type(
        name,
        (WebSocket处理器,),
        {"要求代理认证": proxy_auth, "握手记录": [], "记录锁": threading.Lock()},
    )


async def 等待记录数量(handler, count: int) -> None:
    for _ in range(200):
        with handler.记录锁:
            if len(handler.握手记录) >= count:
                return
        await asyncio.sleep(0.01)
    raise AssertionError(f"等待{count}条请求记录超时")


def 断言消息(message, message_type: str, data) -> None:
    assert message is not None
    assert message.type == message_type, message
    assert message.data == data, message


def 测试同步直连(ws_url: str) -> None:
    from requests_rust import Session

    with Session(impersonate=测试版本) as session:
        session.cookies.set("session_cookie", "kept", url=ws_url.replace("ws://", "http://"))
        with session.websocket(
            ws_url,
            headers={"X-WebSocket-Test": "sync"},
            protocols=["chat"],
        ) as websocket:
            assert websocket.protocol == "chat"
            断言消息(websocket.recv(), "text", "welcome")
            websocket.send("hello")
            断言消息(websocket.recv(), "text", "echo:hello")
            websocket.send_bytes(b"\x00\x01binary")
            断言消息(websocket.recv(), "binary", b"\x00\x01binary")
            websocket.ping(b"sync-ping")
            断言消息(websocket.recv(), "pong", b"sync-ping")
            try:
                websocket.ping(b"x" * 126)
            except RuntimeError as error:
                assert "125" in str(error)
            else:
                raise AssertionError("超过125字节的Ping没有被拒绝")

        with session.websocket(ws_url) as websocket:
            断言消息(websocket.recv(), "text", "welcome")
            websocket.send("server-close")
            close_message = websocket.recv()
            assert close_message is not None
            assert close_message.type == "close"
            assert close_message.code == 4001
            assert close_message.reason == "测试关闭"


async def 测试异步直连(ws_url: str) -> None:
    from requests_rust import AsyncSession

    async with AsyncSession(impersonate=测试版本) as session:
        async with await session.websocket(ws_url) as websocket:
            断言消息(await websocket.recv(), "text", "welcome")
            receive_task = asyncio.create_task(websocket.recv())
            await asyncio.sleep(0)
            await websocket.send("async")
            断言消息(await asyncio.wait_for(receive_task, 2), "text", "echo:async")
            await websocket.ping(b"async-ping")
            断言消息(await websocket.recv(), "pong", b"async-ping")


async def 测试动态代理并发连接(proxy_port: int) -> None:
    from requests_rust import AsyncSession

    async with AsyncSession(impersonate=测试版本) as session:
        async def 建立连接(index: int):
            proxy = (
                f"http://session-{index}:{quote(代理密码, safe='')}"
                f"@127.0.0.1:{proxy_port}"
            )
            websocket = await session.websocket(
                "ws://dynamic-proxy.test/socket",
                proxy=proxy,
            )
            断言消息(await websocket.recv(), "text", "welcome")
            return websocket

        websockets = await asyncio.gather(
            *(建立连接(index) for index in range(动态代理连接数))
        )
        try:
            assert all(not websocket.closed for websocket in websockets)
            await asyncio.gather(
                *(websocket.send(f"connection-{index}") for index, websocket in enumerate(websockets))
            )
            messages = await asyncio.gather(*(websocket.recv() for websocket in websockets))
            for index, message in enumerate(messages):
                断言消息(message, "text", f"echo:connection-{index}")
        finally:
            await asyncio.gather(*(websocket.close() for websocket in websockets))


async def 测试动态SOCKS5并发连接(ws_url: str, proxy_port: int) -> None:
    from requests_rust import AsyncSession

    async with AsyncSession(impersonate=测试版本) as session:
        async def 建立连接(index: int):
            proxy = (
                f"socks5h://session-{index}:{quote(代理密码, safe='')}"
                f"@127.0.0.1:{proxy_port}"
            )
            websocket = await session.websocket(ws_url, proxy=proxy)
            断言消息(await websocket.recv(), "text", "welcome")
            return websocket

        websockets = await asyncio.gather(
            *(建立连接(index) for index in range(动态代理连接数))
        )
        try:
            assert all(not websocket.closed for websocket in websockets)
        finally:
            await asyncio.gather(*(websocket.close() for websocket in websockets))


async def 测试混合连接池(
    ws_proxy_port: int,
    socks_proxy_port: int,
    socks_target_port: int,
    http_handler,
    socks_target_handler,
    response_gate: threading.Event,
) -> None:
    from requests_rust import AsyncSession

    async with AsyncSession(impersonate=测试版本, max_connections=50) as session:
        async def 建立WebSocket(index: int):
            proxy = (
                f"http://mixed-ws-{index}:{quote(代理密码, safe='')}"
                f"@127.0.0.1:{ws_proxy_port}"
            )
            websocket = await session.websocket(
                "ws://mixed-proxy.test/socket",
                proxy=proxy,
            )
            断言消息(await websocket.recv(), "text", "welcome")
            return websocket

        websockets = await asyncio.gather(*(建立WebSocket(index) for index in range(25)))
        try:
            http_tasks = [
                asyncio.create_task(
                    session.get(
                        "http://mixed-http.test/resource",
                        proxy=(
                            f"http://mixed-http-{index}:{quote(代理密码, safe='')}"
                            f"@127.0.0.1:{ws_proxy_port}"
                        ),
                    )
                )
                for index in range(15)
            ]
            socks_tasks = [
                asyncio.create_task(
                    session.get(
                        f"http://127.0.0.1:{socks_target_port}/resource",
                        proxy=(
                            f"socks5h://mixed-socks-{index}:{quote(代理密码, safe='')}"
                            f"@127.0.0.1:{socks_proxy_port}"
                        ),
                    )
                )
                for index in range(10)
            ]
            await asyncio.gather(
                等待记录数量(http_handler, 40),
                等待记录数量(socks_target_handler, 10),
            )
            overflow_task = asyncio.create_task(
                session.get(
                    "http://mixed-overflow.test/resource",
                    proxy=(
                        f"http://mixed-overflow:{quote(代理密码, safe='')}"
                        f"@127.0.0.1:{ws_proxy_port}"
                    ),
                )
            )
            await asyncio.sleep(0.05)
            assert not overflow_task.done()
            assert len(http_handler.握手记录) == 40
            await websockets[0].close()
            await 等待记录数量(http_handler, 41)
            response_gate.set()
            responses = await asyncio.gather(*http_tasks, *socks_tasks, overflow_task)
            assert all(response.content == b"mixed-http-ok" for response in responses)
        finally:
            response_gate.set()
            await asyncio.gather(*(websocket.close() for websocket in websockets))


async def 测试连接槽位生命周期(ws_url: str) -> None:
    from requests_rust import AsyncSession

    http_url = ws_url.replace("ws://", "http://")

    async with AsyncSession(impersonate=测试版本, max_connections=1) as session:
        websocket = await session.websocket(ws_url)
        断言消息(await websocket.recv(), "text", "welcome")
        await websocket.send("server-close")
        response = await asyncio.wait_for(session.get(http_url), 2)
        assert response.content == b"mixed-http-ok"

    async with AsyncSession(impersonate=测试版本, max_connections=1) as session:
        websocket = await session.websocket(ws_url)
        断言消息(await websocket.recv(), "text", "welcome")
        await asyncio.gather(websocket.close(), websocket.close())
        response = await asyncio.wait_for(session.get(http_url), 2)
        assert response.content == b"mixed-http-ok"

    async with AsyncSession(impersonate=测试版本, max_connections=1) as session:
        websocket = await session.websocket(ws_url)
        断言消息(await websocket.recv(), "text", "welcome")
        close_task = asyncio.create_task(websocket.close())
        close_task.cancel()
        try:
            await close_task
        except asyncio.CancelledError:
            pass
        response = await asyncio.wait_for(session.get(http_url), 2)
        assert response.content == b"mixed-http-ok"

    async with AsyncSession(impersonate=测试版本, max_connections=1) as session:
        response = await session.get(http_url, stream=True)
        del response
        gc.collect()
        response = await asyncio.wait_for(session.get(http_url), 2)
        assert response.content == b"mixed-http-ok"

    session = AsyncSession(impersonate=测试版本, max_connections=1)
    websocket = await session.websocket(ws_url)
    断言消息(await websocket.recv(), "text", "welcome")
    waiting_request = asyncio.create_task(session.get(http_url))
    await asyncio.sleep(0)
    await session.close()
    try:
        await asyncio.wait_for(waiting_request, 2)
    except RuntimeError as error:
        assert "关闭" in str(error)
    else:
        raise AssertionError("Session关闭后等待连接槽位的请求没有失败")
    await websocket.close()


async def 测试并发流读取不释放槽位(
    slow_url: str,
    normal_url: str,
    response_gate: threading.Event,
) -> None:
    from requests_rust import AsyncSession

    async with AsyncSession(impersonate=测试版本, max_connections=1) as session:
        response = await session.get(slow_url, stream=True)
        first_read = asyncio.ensure_future(response._stream.read_async(1))
        await asyncio.sleep(0.05)
        try:
            await response._stream.read_async(1)
        except RuntimeError as error:
            assert "活动读取" in str(error)
        else:
            raise AssertionError("第二个并发流读取没有被拒绝")
        waiting_request = asyncio.create_task(session.get(normal_url))
        await asyncio.sleep(0.05)
        assert not waiting_request.done()
        await response.aclose()
        response_gate.set()
        assert await first_read == b""
        result = await asyncio.wait_for(waiting_request, 2)
        assert result.content == b"mixed-http-ok"


def 测试HTTP代理(proxy_port: int) -> None:
    from requests_rust import Session

    proxy = (
        f"http://{quote(代理用户名, safe='')}:{quote(代理密码, safe='')}"
        f"@127.0.0.1:{proxy_port}"
    )
    with Session(impersonate=测试版本, proxy=proxy) as session:
        with session.websocket("ws://proxy-target.test/socket") as websocket:
            断言消息(websocket.recv(), "text", "welcome")
            websocket.send("proxy")
            断言消息(websocket.recv(), "text", "echo:proxy")


def 测试WSS(wss_url: str) -> None:
    from requests_rust import Session

    with Session(impersonate=测试版本, verify=False) as session:
        with session.websocket(wss_url) as websocket:
            断言消息(websocket.recv(), "text", "welcome")
            websocket.send("secure")
            断言消息(websocket.recv(), "text", "echo:secure")


def 测试WSS_HTTP代理(wss_url: str, proxy_port: int) -> None:
    from requests_rust import Session

    proxy = (
        f"http://{quote(代理用户名, safe='')}:{quote(代理密码, safe='')}"
        f"@127.0.0.1:{proxy_port}"
    )
    with Session(impersonate=测试版本, verify=False, proxy=proxy) as session:
        with session.websocket(wss_url) as websocket:
            断言消息(websocket.recv(), "text", "welcome")
            websocket.send("connect-proxy")
            断言消息(websocket.recv(), "text", "echo:connect-proxy")


def 测试SOCKS5代理(ws_url: str, proxy_port: int) -> None:
    from requests_rust import Session

    proxy = (
        f"socks5h://{quote(代理用户名, safe='')}:{quote(代理密码, safe='')}"
        f"@127.0.0.1:{proxy_port}"
    )
    with Session(impersonate=测试版本, proxy=proxy) as session:
        with session.websocket(ws_url) as websocket:
            断言消息(websocket.recv(), "text", "welcome")
            websocket.send("socks-proxy")
            断言消息(websocket.recv(), "text", "echo:socks-proxy")


def 测试WSS_SOCKS5代理(wss_url: str, proxy_port: int) -> None:
    from requests_rust import Session

    proxy = (
        f"socks5h://{quote(代理用户名, safe='')}:{quote(代理密码, safe='')}"
        f"@127.0.0.1:{proxy_port}"
    )
    with Session(impersonate=测试版本, verify=False, proxy=proxy) as session:
        with session.websocket(wss_url) as websocket:
            断言消息(websocket.recv(), "text", "welcome")
            websocket.send("secure-socks")
            断言消息(websocket.recv(), "text", "echo:secure-socks")


def main() -> None:
    direct_handler = 创建处理器("直连WebSocket")
    proxy_handler = 创建处理器("代理WebSocket", proxy_auth=True)
    dynamic_proxy_handler = 创建处理器("动态代理WebSocket")
    mixed_response_gate = threading.Event()
    mixed_proxy_handler = type(
        "混合代理WebSocket",
        (WebSocket处理器,),
        {
            "HTTP响应闸门": mixed_response_gate,
            "握手记录": [],
            "记录锁": threading.Lock(),
        },
    )
    mixed_socks_target_handler = type(
        "混合SOCKS目标",
        (WebSocket处理器,),
        {
            "HTTP响应闸门": mixed_response_gate,
            "握手记录": [],
            "记录锁": threading.Lock(),
        },
    )
    slow_stream_gate = threading.Event()
    slow_stream_handler = type(
        "慢流目标",
        (WebSocket处理器,),
        {
            "HTTP响应闸门": slow_stream_gate,
            "握手记录": [],
            "记录锁": threading.Lock(),
        },
    )
    tls_handler = 创建处理器("TLSWebSocket")
    direct_server, direct_thread = 启动服务(direct_handler)
    proxy_server, proxy_thread = 启动服务(proxy_handler)
    dynamic_proxy_server, dynamic_proxy_thread = 启动服务(dynamic_proxy_handler)
    mixed_proxy_server, mixed_proxy_thread = 启动服务(mixed_proxy_handler)
    mixed_socks_target_server, mixed_socks_target_thread = 启动服务(
        mixed_socks_target_handler
    )
    slow_stream_server, slow_stream_thread = 启动服务(slow_stream_handler)
    tls_server, tls_thread = 启动服务(tls_handler, tls=True)
    connect_handler = type(
        "WSSCONNECT代理",
        (CONNECT代理处理器,),
        {
            "目标端口": tls_server.server_address[1],
            "请求记录": [],
            "记录锁": threading.Lock(),
        },
    )
    connect_server, connect_thread = 启动服务(connect_handler)
    socks_handler = type(
        "WebSocketSOCKS5代理",
        (SOCKS5代理处理器,),
        {"请求记录": [], "记录锁": threading.Lock()},
    )
    socks_server, socks_thread = 启动服务(socks_handler)
    dynamic_socks_handler = type(
        "动态WebSocketSOCKS5代理",
        (SOCKS5代理处理器,),
        {
            "接受动态用户名": True,
            "请求记录": [],
            "记录锁": threading.Lock(),
        },
    )
    dynamic_socks_server, dynamic_socks_thread = 启动服务(dynamic_socks_handler)
    mixed_socks_handler = type(
        "混合SOCKS5代理",
        (SOCKS5代理处理器,),
        {
            "接受动态用户名": True,
            "请求记录": [],
            "记录锁": threading.Lock(),
        },
    )
    mixed_socks_server, mixed_socks_thread = 启动服务(mixed_socks_handler)
    ws_url = f"ws://127.0.0.1:{direct_server.server_address[1]}/socket"
    wss_url = f"wss://127.0.0.1:{tls_server.server_address[1]}/socket"
    try:
        测试同步直连(ws_url)
        asyncio.run(测试异步直连(ws_url))
        asyncio.run(测试动态代理并发连接(dynamic_proxy_server.server_address[1]))
        asyncio.run(
            测试动态SOCKS5并发连接(
                ws_url,
                dynamic_socks_server.server_address[1],
            )
        )
        asyncio.run(
            测试混合连接池(
                mixed_proxy_server.server_address[1],
                mixed_socks_server.server_address[1],
                mixed_socks_target_server.server_address[1],
                mixed_proxy_handler,
                mixed_socks_target_handler,
                mixed_response_gate,
            )
        )
        asyncio.run(测试连接槽位生命周期(ws_url))
        asyncio.run(
            测试并发流读取不释放槽位(
                f"http://127.0.0.1:{slow_stream_server.server_address[1]}/slow-stream",
                ws_url.replace("ws://", "http://"),
                slow_stream_gate,
            )
        )
        测试HTTP代理(proxy_server.server_address[1])
        测试WSS(wss_url)
        测试WSS_HTTP代理(wss_url, connect_server.server_address[1])
        测试SOCKS5代理(ws_url, socks_server.server_address[1])
        测试WSS_SOCKS5代理(wss_url, socks_server.server_address[1])

        assert len(proxy_handler.握手记录) == 1, proxy_handler.握手记录
        assert proxy_handler.握手记录[0]["authorization"] == 预期代理认证
        assert proxy_handler.握手记录[0]["request_line"].startswith(
            "GET http://proxy-target.test/socket HTTP/1.1"
        ), proxy_handler.握手记录
        assert direct_handler.握手记录[0]["cookie"] == "session_cookie=kept"
        assert direct_handler.握手记录[0]["custom"] == "sync"
        assert direct_handler.握手记录[0]["protocol"] == "chat"
        assert len(dynamic_proxy_handler.握手记录) == 动态代理连接数
        dynamic_users = {
            base64.b64decode(record["authorization"][6:]).decode().split(":", 1)[0]
            for record in dynamic_proxy_handler.握手记录
        }
        assert dynamic_users == {
            f"session-{index}" for index in range(动态代理连接数)
        }
        assert len(
            {record["client_port"] for record in dynamic_proxy_handler.握手记录}
        ) == 动态代理连接数
        assert len(dynamic_socks_handler.请求记录) == 动态代理连接数
        assert {record["username"] for record in dynamic_socks_handler.请求记录} == {
            f"session-{index}" for index in range(动态代理连接数)
        }
        assert len(
            {record["client_port"] for record in dynamic_socks_handler.请求记录}
        ) == 动态代理连接数
        assert len(mixed_proxy_handler.握手记录) == 41
        assert len(mixed_socks_handler.请求记录) == 10
        assert len(mixed_socks_target_handler.握手记录) == 10
        assert connect_handler.请求记录 == [
            {
                "request_line": f"CONNECT 127.0.0.1:{tls_server.server_address[1]} HTTP/1.1",
                "authorization": 预期代理认证,
            }
        ]
        assert [
            {key: value for key, value in record.items() if key != "client_port"}
            for record in socks_handler.请求记录
        ] == [
            {
                "methods": [0, 2],
                "username": 代理用户名,
                "password": 代理密码,
                "request": (5, 1, 0),
                "target": ("127.0.0.1", direct_server.server_address[1]),
            },
            {
                "methods": [0, 2],
                "username": 代理用户名,
                "password": 代理密码,
                "request": (5, 1, 0),
                "target": ("127.0.0.1", tls_server.server_address[1]),
            },
        ]
        print("WebSocket同步、异步、WSS、HTTP CONNECT与SOCKS5代理验证通过")
    finally:
        for server, thread in (
            (slow_stream_server, slow_stream_thread),
            (mixed_socks_server, mixed_socks_thread),
            (dynamic_socks_server, dynamic_socks_thread),
            (socks_server, socks_thread),
            (connect_server, connect_thread),
            (tls_server, tls_thread),
            (mixed_socks_target_server, mixed_socks_target_thread),
            (mixed_proxy_server, mixed_proxy_thread),
            (dynamic_proxy_server, dynamic_proxy_thread),
            (proxy_server, proxy_thread),
            (direct_server, direct_thread),
        ):
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)


if __name__ == "__main__":
    main()
