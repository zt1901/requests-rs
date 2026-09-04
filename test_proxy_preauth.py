from __future__ import annotations

import base64
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import quote


# 可右键运行；测试只使用本机代理，不访问外网。
代理用户名 = "session-test"
代理密码 = "p@ss word:测试"
测试版本 = "chrome146"
预期认证头 = "Basic " + base64.b64encode(
    f"{代理用户名}:{代理密码}".encode()
).decode()


class 预认证代理处理器(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    请求记录: list[tuple[str, str]] = []
    记录锁 = threading.Lock()

    def log_message(self, *_: object) -> None:
        pass

    def 记录请求(self) -> None:
        with type(self).记录锁:
            type(self).请求记录.append(
                (self.command, self.headers.get("Proxy-Authorization", ""))
            )

    def 认证通过(self) -> bool:
        if self.headers.get("Proxy-Authorization") == 预期认证头:
            return True
        self.send_response(407, "Proxy Authentication Required")
        self.send_header("Proxy-Authenticate", 'Basic realm="preauth-test"')
        self.send_header("Content-Length", "0")
        self.send_header("Connection", "close")
        self.end_headers()
        return False

    def do_GET(self) -> None:
        self.记录请求()
        if not self.认证通过():
            return
        body = json.dumps({"preauthenticated": True}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_CONNECT(self) -> None:
        self.记录请求()
        if not self.认证通过():
            return
        # 认证成功后故意终止隧道，测试只关心首个 CONNECT 请求头。
        self.send_response(502)
        self.send_header("Content-Length", "0")
        self.send_header("Connection", "close")
        self.end_headers()


def main() -> None:
    from requests_rs import requests
    Session = requests.Session

    server = ThreadingHTTPServer(("127.0.0.1", 0), 预认证代理处理器)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    encoded_user = quote(代理用户名, safe="")
    encoded_password = quote(代理密码, safe="")
    proxy_url = (
        f"http://{encoded_user}:{encoded_password}@127.0.0.1:{server.server_port}"
    )
    try:
        with Session(impersonate=测试版本, proxy=proxy_url) as session:
            response = session.get("http://preauth.test/plain")
            assert response.json() == {"preauthenticated": True}

            try:
                session.get("https://preauth.test/tunnel")
            except Exception:
                pass
            else:
                raise AssertionError("测试代理拒绝隧道后，HTTPS请求没有报错")

        assert 预认证代理处理器.请求记录 == [
            ("GET", 预期认证头),
            ("CONNECT", 预期认证头),
        ], 预认证代理处理器.请求记录
        print("HTTP与HTTPS CONNECT代理首包预认证验证通过")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


if __name__ == "__main__":
    main()
