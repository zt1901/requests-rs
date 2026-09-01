from __future__ import annotations

import asyncio
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


# 【可调参数】右键运行即可验证本机wheel中的Edge指纹。
测试版本 = "edge152"
预期指纹数量 = 1

# 【可调参数】完整导航Header用于验证捕获顺序能穿过Python和Rust请求层。
浏览器导航请求头 = [
    ("sec-ch-ua", '"Chromium";v="152", "Not?A_Brand";v="24", "Microsoft Edge";v="152"'),
    ("sec-ch-ua-mobile", "?0"),
    ("sec-ch-ua-platform", '"Windows"'),
    ("upgrade-insecure-requests", "1"),
    ("user-agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36 Edg/152.0.0.0"),
    ("accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"),
    ("sec-fetch-site", "none"),
    ("sec-fetch-mode", "navigate"),
    ("sec-fetch-user", "?1"),
    ("sec-fetch-dest", "document"),
    ("accept-encoding", "gzip, deflate, br, zstd"),
    ("accept-language", "zh-CN,zh;q=0.9,en;q=0.8,en-GB;q=0.7,en-US;q=0.6"),
    ("priority", "u=0, i"),
]


class 请求头处理器(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_: object) -> None:
        pass

    def do_GET(self) -> None:
        响应数据 = json.dumps(
            {
                "user_agent": self.headers.get("User-Agent", ""),
                "sec_ch_ua": self.headers.get("sec-ch-ua", ""),
                "sec_ch_ua_mobile": self.headers.get("sec-ch-ua-mobile", ""),
                "sec_ch_ua_platform": self.headers.get("sec-ch-ua-platform", ""),
                "header_order": [
                    name.lower() for name, _ in self.headers.items() if name.lower() != "host"
                ],
            }
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(响应数据)))
        self.end_headers()
        self.wfile.write(响应数据)


def 验证Edge请求头(请求头: dict[str, str]) -> None:
    assert "Chrome/152.0.0.0" in 请求头["user_agent"], 请求头
    assert "Edg/152.0.0.0" in 请求头["user_agent"], 请求头
    assert '"Chromium";v="152"' in 请求头["sec_ch_ua"], 请求头
    assert '"Microsoft Edge";v="152"' in 请求头["sec_ch_ua"], 请求头
    assert 请求头["sec_ch_ua_mobile"] == "?0", 请求头
    assert 请求头["sec_ch_ua_platform"] == '"Windows"', 请求头


async def 验证异步请求(目标地址: str) -> None:
    from requests_rust import AsyncSession

    async with AsyncSession(
        impersonate=测试版本,
        fingerprint_rotation=False,
    ) as 会话:
        assert 会话.fingerprint_count == 预期指纹数量
        响应 = await 会话.get(目标地址 + "/async")
        验证Edge请求头(响应.json())


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
            验证Edge请求头(固定响应.json())

        with Session(
            impersonate=测试版本,
            fingerprint_rotation=False,
            headers=浏览器导航请求头,
        ) as 顺序会话:
            顺序响应 = 顺序会话.get(目标地址 + "/ordered")
            顺序结果 = 顺序响应.json()
            assert 顺序结果["header_order"] == [名称 for 名称, _ in 浏览器导航请求头], 顺序结果

        with Session(
            impersonate=测试版本,
            fingerprint_rotation=True,
            fingerprint_pool=True,
            fingerprint_pool_size=预期指纹数量,
        ) as 轮换会话:
            指纹编号 = []
            for 序号 in range(5):
                响应 = 轮换会话.get(f"{目标地址}/rotation?sample={序号 + 1}")
                验证Edge请求头(响应.json())
                指纹编号.append(响应.fingerprint_id)
            assert len(set(指纹编号)) == 1, 指纹编号
            assert 轮换会话.fingerprint_pool_count == 1

        asyncio.run(验证异步请求(目标地址))
        print("Edge 152单火种Profile、默认请求头、完整Header顺序和请求级新Client验证通过")
    finally:
        服务.shutdown()
        服务.server_close()
        服务线程.join(timeout=5)


if __name__ == "__main__":
    main()
