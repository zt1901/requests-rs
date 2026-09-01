from __future__ import annotations

import argparse
from contextlib import suppress
from http.client import HTTPConnection, HTTPSConnection
import json
from pathlib import Path
import ssl
import tempfile

from http3_local_gateway import 本地HTTP3网关
from test_http3 import 启动服务, 获取UDP端口


# 【可调参数】默认仅验证本机CONNECT转换；--public额外访问公网HTTP/3目标。
公网HTTP3主机 = "cloudflare-quic.com"
公网HTTP3路径 = "/b/"


def 读取JSON(response) -> dict:
    body = response.read()
    return json.loads(body.decode("utf-8"))


def 验证本机CONNECT() -> None:
    backend_port = 获取UDP端口()
    backend = 启动服务(backend_port)
    try:
        with tempfile.TemporaryDirectory(prefix="http3-gateway-test-") as directory:
            gateway = 本地HTTP3网关(
                port=0,
                ca_dir=Path(directory),
                verify_upstream=False,
            )
            gateway.start()
            try:
                context = ssl.create_default_context(cafile=str(gateway.ca_certificate))
                connection = HTTPSConnection(*gateway.address, context=context, timeout=30)
                connection.set_tunnel("127.0.0.1", backend_port)
                connection.request("GET", "/echo", headers={"X-Echo": "gateway"})
                first = connection.getresponse()
                assert first.status == 200
                assert first.getheader("X-Upstream-HTTP-Version") == "HTTP/3"
                first_data = 读取JSON(first)
                assert first_data["x_echo"] == "gateway"
                connection_id = first_data["connection_id"]

                payload = b"gateway-body"
                connection.request(
                    "POST",
                    "/echo",
                    body=payload,
                    headers={"Content-Length": str(len(payload))},
                )
                second = connection.getresponse()
                assert second.status == 200
                assert second.getheader("X-Local-HTTP3-Gateway") == "1"
                second_data = 读取JSON(second)
                assert second_data["body"] == "gateway-body"
                assert second_data["connection_id"] == connection_id
                connection.close()

                from requests_rust import Session

                with Session(
                    impersonate="chrome150",
                    proxy=gateway.proxy_url,
                    verify=False,
                    cookie_store=False,
                ) as client:
                    first_proxy = client.get(f"https://127.0.0.1:{backend_port}/echo")
                    second_proxy = client.get(f"https://127.0.0.1:{backend_port}/echo")
                    assert first_proxy.headers["x-upstream-http-version"] == "HTTP/3"
                    assert second_proxy.headers["x-upstream-http-version"] == "HTTP/3"
                    assert first_proxy.json()["connection_id"] == second_proxy.json()["connection_id"]

                plain = HTTPConnection(*gateway.address, timeout=30)
                plain.request("GET", f"https://127.0.0.1:{backend_port}/echo")
                response = plain.getresponse()
                assert response.status == 200
                assert response.getheader("X-Upstream-HTTP-Version") == "HTTP/3"
                assert 读取JSON(response)["path"] == "/echo"
                plain.close()

                rejected = HTTPConnection(*gateway.address, timeout=30)
                rejected.request("GET", "http://127.0.0.1/plain")
                rejected_response = rejected.getresponse()
                assert rejected_response.status == 400
                rejected_response.read()
                rejected.close()

                assert gateway.server.successes == 5
                assert gateway.server.errors == 0
            finally:
                gateway.close()
    finally:
        backend.terminate()
        with suppress(Exception):
            backend.wait(timeout=5)
        if backend.poll() is None:
            backend.kill()
            backend.wait(timeout=5)


def 验证公网CONNECT() -> None:
    with tempfile.TemporaryDirectory(prefix="http3-gateway-public-") as directory:
        gateway = 本地HTTP3网关(port=0, ca_dir=Path(directory), verify_upstream=True)
        gateway.start()
        try:
            context = ssl.create_default_context(cafile=str(gateway.ca_certificate))
            connection = HTTPSConnection(*gateway.address, context=context, timeout=30)
            connection.set_tunnel(公网HTTP3主机, 443)
            connection.request("GET", 公网HTTP3路径)
            response = connection.getresponse()
            body = response.read()
            assert response.getheader("X-Upstream-HTTP-Version") == "HTTP/3"
            assert body
            print(f"公网网关响应: status={response.status}, bytes={len(body)}, upstream=HTTP/3")
            connection.close()
        finally:
            gateway.close()


def main(public: bool = False) -> None:
    验证本机CONNECT()
    if public:
        验证公网CONNECT()
    print("本地CONNECT转HTTP/3网关验证通过")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--public", action="store_true")
    args = parser.parse_args()
    main(args.public)
