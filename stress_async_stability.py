import asyncio
from contextlib import suppress
import gc
import os
import time

import psutil
from requests_rust import AsyncSession


# 可右键运行；脚本自动启动本地HTTP服务，不访问外网。
测试版本 = "chrome150"
每轮请求数 = 5000
并发数 = 100
测试轮数 = 4
启用指纹轮换 = False
指纹池上限 = 100
缓存Origin上限 = 4
测试地址 = ""


async def 处理请求(reader, writer):
    try:
        while True:
            try:
                await reader.readuntil(b"\r\n\r\n")
            except asyncio.IncompleteReadError:
                break
            body = b"stability"
            writer.write(
                b"HTTP/1.1 200 OK\r\n"
                + f"Content-Length: {len(body)}\r\n".encode()
                + b"Connection: keep-alive\r\n\r\n"
                + body
            )
            await writer.drain()
    except (ConnectionError, asyncio.LimitOverrunError):
        pass
    finally:
        writer.close()
        with suppress(ConnectionError):
            await writer.wait_closed()


async def main():
    global 测试地址
    process = psutil.Process(os.getpid())
    初始内存 = process.memory_info().rss
    server = await asyncio.start_server(处理请求, "127.0.0.1", 0, backlog=1024)
    测试地址 = f"http://127.0.0.1:{server.sockets[0].getsockname()[1]}/"
    try:
        async with AsyncSession(
            impersonate=测试版本,
            fingerprint_rotation=启用指纹轮换,
            fingerprint_pool=True,
            fingerprint_pool_size=指纹池上限,
            max_cached_origins=缓存Origin上限,
            max_connections=并发数,
        ) as session:
            for 轮次 in range(1, 测试轮数 + 1):
                next_index = 0
                index_lock = asyncio.Lock()

                async def worker():
                    nonlocal next_index
                    while True:
                        async with index_lock:
                            if next_index >= 每轮请求数:
                                return
                            index = next_index
                            next_index += 1
                        response = await session.get(
                            测试地址,
                            headers={"X-Request-Id": f"{轮次}-{index}"},
                            cookies={"request_cookie": f"{轮次}-{index}"},
                            proxy=None,
                        )
                        response.raise_for_status()

                started = time.perf_counter()
                await asyncio.gather(*(worker() for _ in range(并发数)))
                gc.collect()
                当前内存 = process.memory_info().rss
                print(
                    f"第{轮次}轮: 吞吐={每轮请求数 / (time.perf_counter() - started):.2f}请求/秒，"
                    f"内存增量={(当前内存 - 初始内存) / 1024 / 1024:.2f}MB，"
                    f"线程={process.num_threads()}，句柄={process.num_handles()}"
                )
    finally:
        server.close()
        await server.wait_closed()


if __name__ == "__main__":
    asyncio.run(main())
