from __future__ import annotations

import asyncio
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


# 【可调参数】右键运行即可验证本机wheel中的Firefox单火种指纹。
测试版本 = "firefox151"
预期指纹数量 = 1
轮换检查次数 = 5


class 请求头处理器(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_: object) -> None:
        pass

    def do_GET(self) -> None:
        响应数据 = json.dumps(
            {
                "user_agent": self.headers.get("User-Agent", ""),
                "accept": self.headers.get("Accept", ""),
                "accept_language": self.headers.get("Accept-Language", ""),
            }
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(响应数据)))
        self.end_headers()
        self.wfile.write(响应数据)


def 验证Firefox请求头(请求头: dict[str, str]) -> None:
    assert "Firefox/151.0" in 请求头["user_agent"], 请求头
    assert "rv:151.0" in 请求头["user_agent"], 请求头
    assert 请求头["accept"] == "", 请求头
    assert 请求头["accept_language"], 请求头


async def 验证异步请求(目标地址: str) -> None:
    from requests_rust import AsyncSession

    async with AsyncSession(
        impersonate=测试版本,
        fingerprint_rotation=True,
    ) as 会话:
        assert 会话.fingerprint_count == 预期指纹数量
        响应 = await 会话.get(目标地址 + "/async")
        验证Firefox请求头(响应.json())


def main() -> None:
    from requests_rust import Session, available_profiles

    assert 测试版本 in available_profiles(), available_profiles()
    服务 = ThreadingHTTPServer(("127.0.0.1", 0), 请求头处理器)
    服务线程 = threading.Thread(target=服务.serve_forever, daemon=True)
    服务线程.start()
    目标地址 = f"http://127.0.0.1:{服务.server_port}"
    try:
        with Session(
            impersonate=测试版本,
            fingerprint_rotation=False,
        ) as 固定会话:
            assert 固定会话.fingerprint_count == 预期指纹数量
            固定响应 = 固定会话.get(目标地址 + "/fixed")
            验证Firefox请求头(固定响应.json())

        with Session(
            impersonate=测试版本,
            fingerprint_rotation=True,
            fingerprint_pool=True,
        ) as 轮换会话:
            指纹编号 = []
            for 序号 in range(轮换检查次数):
                响应 = 轮换会话.get(f"{目标地址}/rotation?sample={序号 + 1}")
                验证Firefox请求头(响应.json())
                指纹编号.append(响应.fingerprint_id)
            assert len(set(指纹编号)) == 1, 指纹编号
            assert 轮换会话.fingerprint_pool_count == 1

        asyncio.run(验证异步请求(目标地址))
        print("Firefox 151单火种Profile、默认请求头、固定模式和单池复用验证通过")
    finally:
        服务.shutdown()
        服务.server_close()
        服务线程.join(timeout=5)


if __name__ == "__main__":
    main()
