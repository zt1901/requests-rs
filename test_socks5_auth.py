from __future__ import annotations

import socketserver
import threading
from urllib.parse import quote


# 可右键运行；测试只使用本机 SOCKS5 代理，不访问外网。
代理用户名 = "session-test"
代理密码 = "pass:word"
测试版本 = "chrome146"


def 读取定长(stream, length: int) -> bytes:
    data = stream.read(length)
    if len(data) != length:
        raise ConnectionError("SOCKS5客户端提前断开")
    return data


class SOCKS5处理器(socketserver.StreamRequestHandler):
    握手记录: list[dict[str, object]] = []
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
            and username == 代理用户名
            and password == 代理密码
        )
        self.wfile.write(b"\x01\x00" if authenticated else b"\x01\x01")
        self.wfile.flush()
        if not authenticated:
            return

        request_version, command, reserved, address_type = 读取定长(self.rfile, 4)
        if address_type == 1:
            target = ".".join(map(str, 读取定长(self.rfile, 4)))
        elif address_type == 3:
            target = 读取定长(self.rfile, 读取定长(self.rfile, 1)[0]).decode()
        elif address_type == 4:
            target = 读取定长(self.rfile, 16).hex()
        else:
            raise AssertionError(f"未知SOCKS5地址类型: {address_type}")
        port = int.from_bytes(读取定长(self.rfile, 2), "big")
        with type(self).记录锁:
            type(self).握手记录.append(
                {
                    "methods": methods,
                    "username": username,
                    "password": password,
                    "request": (request_version, command, reserved),
                    "target": (target, port),
                }
            )

        # 认证和 CONNECT 都已完成解析；拒绝转发，确保测试不会访问外网。
        self.wfile.write(b"\x05\x04\x00\x01\x00\x00\x00\x00\x00\x00")
        self.wfile.flush()


class SOCKS5服务(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main() -> None:
    from requests_rs import requests
    Session = requests.Session

    server = SOCKS5服务(("127.0.0.1", 0), SOCKS5处理器)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    proxy_url = (
        f"socks5h://{quote(代理用户名, safe='')}:{quote(代理密码, safe='')}"
        f"@127.0.0.1:{server.server_address[1]}"
    )

    try:
        with Session(impersonate=测试版本, proxy=proxy_url) as session:
            try:
                session.get("https://socks-auth.test/resource")
            except Exception:
                pass
            else:
                raise AssertionError("测试代理拒绝转发后，请求没有报错")

        assert SOCKS5处理器.握手记录 == [
            {
                "methods": [0, 2],
                "username": 代理用户名,
                "password": 代理密码,
                "request": (5, 1, 0),
                "target": ("socks-auth.test", 443),
            }
        ], SOCKS5处理器.握手记录
        print("SOCKS5用户名密码认证与CONNECT握手验证通过")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


if __name__ == "__main__":
    main()
