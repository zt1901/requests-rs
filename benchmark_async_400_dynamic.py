import asyncio
import base64
import gc
import json
import os
import statistics
import time

import psutil
from requests_rust import AsyncSession


# 可右键运行；脚本会自动启动本地代理，不访问外网。
测试版本 = "chrome142"
并发数 = 400
重复轮数 = 3
代理密码 = "password"
指纹轮换列表 = [False, True]


async def 读取请求头(reader):
    data = await reader.readuntil(b"\r\n\r\n")
    lines = data.decode("latin1").split("\r\n")
    headers = {}
    for line in lines[1:]:
        if ":" in line:
            name, value = line.split(":", 1)
            headers[name.lower()] = value.strip()
    return headers


async def 处理代理请求(reader, writer):
    try:
        headers = await 读取请求头(reader)
        authorization = headers.get("proxy-authorization", "")
        proxy_user = ""
        if authorization.startswith("Basic "):
            credentials = base64.b64decode(authorization[6:]).decode("utf-8")
            proxy_user = credentials.split(":", 1)[0]
        body = json.dumps(
            {
                "request_id": headers.get("x-request-id", ""),
                "cookie": headers.get("cookie", ""),
                "proxy_user": proxy_user,
            }
        ).encode()
        writer.write(
            b"HTTP/1.1 200 OK\r\n"
            b"Content-Type: application/json\r\n"
            + f"Content-Length: {len(body)}\r\n".encode()
            + b"Connection: close\r\n\r\n"
            + body
        )
        await writer.drain()
    finally:
        writer.close()
        await writer.wait_closed()


async def 执行一轮(session, proxy_port):
    async def 请求一次(index):
        response = await session.get(
            "http://dynamic.test/resource",
            headers=[
                ("X-Request-Id", str(index)),
            ],
            cookies={"request_cookie": index},
            proxy=f"http://session-{index}:{代理密码}@127.0.0.1:{proxy_port}",
        )
        result = response.json()
        assert result == {
            "request_id": str(index),
            "cookie": f"request_cookie={index}",
            "proxy_user": f"session-{index}",
        }
        return response

    started = time.perf_counter()
    responses = await asyncio.gather(*(请求一次(index) for index in range(并发数)))
    elapsed = time.perf_counter() - started
    assert len(responses) == 并发数
    return elapsed


async def main():
    server = await asyncio.start_server(
        处理代理请求,
        "127.0.0.1",
        0,
        backlog=并发数 * 2,
    )
    proxy_port = server.sockets[0].getsockname()[1]
    process = psutil.Process(os.getpid())
    try:
        for fingerprint_rotation in 指纹轮换列表:
            async with AsyncSession(
                impersonate=测试版本,
                fingerprint_rotation=fingerprint_rotation,
            ) as session:
                await session.get(
                    "http://dynamic.test/warmup",
                    headers=[("X-Request-Id", "warmup")],
                    cookies={"request_cookie": "warmup"},
                    proxy=f"http://session-warmup:{代理密码}@127.0.0.1:{proxy_port}",
                )
                初始内存 = process.memory_info().rss
                结果 = []
                mode = "指纹轮换" if fingerprint_rotation else "固定指纹"
                for round_index in range(重复轮数):
                    elapsed = await 执行一轮(session, proxy_port)
                    结果.append(elapsed)
                    gc.collect()
                    print(
                        f"{mode}第{round_index + 1}轮: 耗时={elapsed:.4f}秒，"
                        f"吞吐={并发数 / elapsed:.2f}请求/秒，"
                        f"线程={process.num_threads()}，"
                        f"内存增量={(process.memory_info().rss - 初始内存) / 1024 / 1024:.2f}MB"
                    )
                median = statistics.median(结果)
                print(f"{mode}400并发中位吞吐: {并发数 / median:.2f}请求/秒")
    finally:
        server.close()
        await server.wait_closed()


if __name__ == "__main__":
    asyncio.run(main())
