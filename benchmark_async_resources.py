import asyncio
from contextlib import suppress
import gc
import os
import time

import psutil
from requests_rust import AsyncSession


# 可右键运行；脚本自动启动本地HTTP服务，不访问外网。
测试版本 = "chrome150"
请求次数 = 5000
并发数 = 100
采样间隔秒 = 0.005
测试地址 = ""


async def 处理请求(reader, writer):
    try:
        while True:
            try:
                await reader.readuntil(b"\r\n\r\n")
            except asyncio.IncompleteReadError:
                break
            body = b"resource-benchmark"
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


async def 采样资源(停止事件, 结果):
    process = psutil.Process(os.getpid())
    while not 停止事件.is_set():
        memory = process.memory_info().rss
        结果["峰值内存"] = max(结果["峰值内存"], memory)
        结果["峰值线程"] = max(结果["峰值线程"], process.num_threads())
        await asyncio.sleep(采样间隔秒)


async def 测量(session):
    gc.collect()
    process = psutil.Process(os.getpid())
    初始内存 = process.memory_info().rss
    结果 = {"峰值内存": 初始内存, "峰值线程": process.num_threads()}
    停止事件 = asyncio.Event()
    采样任务 = asyncio.create_task(采样资源(停止事件, 结果))
    started = time.perf_counter()
    try:
        next_index = 0
        index_lock = asyncio.Lock()

        async def worker():
            nonlocal next_index
            while True:
                async with index_lock:
                    if next_index >= 请求次数:
                        return
                    index = next_index
                    next_index += 1
                response = await session.get(
                    测试地址,
                    headers={"X-Request-Id": str(index)},
                    cookies={"request_cookie": str(index)},
                    proxy=None,
                )
                response.raise_for_status()

        await asyncio.gather(*(worker() for _ in range(并发数)))
    finally:
        耗时 = time.perf_counter() - started
        停止事件.set()
        await 采样任务
    gc.collect()
    结束内存 = process.memory_info().rss
    print(
        f"requests_rust普通请求Worker: 吞吐={请求次数 / 耗时:.2f}请求/秒，"
        f"峰值增量={(结果['峰值内存'] - 初始内存) / 1024 / 1024:.2f}MB，"
        f"结束增量={(结束内存 - 初始内存) / 1024 / 1024:.2f}MB，"
        f"峰值线程={结果['峰值线程']}"
    )


async def main():
    global 测试地址
    server = await asyncio.start_server(处理请求, "127.0.0.1", 0, backlog=1024)
    测试地址 = f"http://127.0.0.1:{server.sockets[0].getsockname()[1]}/"
    try:
        async with AsyncSession(impersonate=测试版本) as session:
            await 测量(session)
    finally:
        server.close()
        await server.wait_closed()


if __name__ == "__main__":
    asyncio.run(main())
