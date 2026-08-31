import asyncio
from contextlib import suppress
import multiprocessing
import subprocess
import statistics
import time

from curl_cffi import requests as curl_requests
from requests_rust import AsyncSession


# 可右键运行；脚本自动启动本地HTTP服务，不访问外网。
测试版本 = "chrome150"
请求次数 = 4000
重复轮数 = 3
并发列表 = [50, 500]
测试地址 = ""


def 检查本地端口状态():
    result = subprocess.run(
        ["netstat", "-ano"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="ignore",
        check=False,
    )
    time_wait_count = result.stdout.count("TIME_WAIT")
    if time_wait_count >= 12000:
        raise RuntimeError(
            f"Windows当前有{time_wait_count}个TIME_WAIT连接，动态端口接近耗尽。"
            "这是上一次短连接压测的残留，请等待约2至4分钟后重新右键运行。"
        )


async def 处理请求(reader, writer):
    try:
        while True:
            try:
                await reader.readuntil(b"\r\n\r\n")
            except asyncio.IncompleteReadError:
                break
            body = b"requests-rust-benchmark"
            writer.write(
                b"HTTP/1.1 200 OK\r\n"
                b"Content-Type: text/plain\r\n"
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


def 运行本地服务(端口发送端):
    async def 启动服务():
        server = await asyncio.start_server(处理请求, "127.0.0.1", 0, backlog=1024)
        端口发送端.send(server.sockets[0].getsockname()[1])
        端口发送端.close()
        async with server:
            await server.serve_forever()

    asyncio.run(启动服务())


async def 测量requests_rust(并发数, 轮换=False):
    async with AsyncSession(
        impersonate=测试版本,
        fingerprint_rotation=轮换,
        max_connections=并发数,
    ) as session:
        async def 请求一次(index):
            response = await session.get(
                测试地址,
                headers={"X-Request-Id": str(index)},
                cookies={"request_cookie": str(index)},
                proxy=None,
            )
            response.raise_for_status()

        await asyncio.gather(*(请求一次(index) for index in range(min(并发数, 25))))
        结果 = []
        for _ in range(重复轮数):
            started = time.perf_counter()
            await 运行固定并发(请求一次, 并发数)
            结果.append(time.perf_counter() - started)
        return 结果


async def 测量curl_cffi(并发数):
    async with curl_requests.AsyncSession(impersonate="chrome", max_clients=并发数) as session:
        async def 请求一次(index):
            return await session.get(
                测试地址,
                headers={"X-Request-Id": str(index)},
                cookies={"request_cookie": str(index)},
                proxy=None,
            )

        await asyncio.gather(*(请求一次(index) for index in range(min(并发数, 25))))
        结果 = []
        for _ in range(重复轮数):
            started = time.perf_counter()
            await 运行固定并发(请求一次, 并发数)
            结果.append(time.perf_counter() - started)
        return 结果


async def 运行固定并发(请求函数, 并发数):
    下一个序号 = 0

    async def worker():
        nonlocal 下一个序号
        while True:
            index = 下一个序号
            下一个序号 += 1
            if index >= 请求次数:
                return
            response = await 请求函数(index)
            if response is not None:
                response.raise_for_status()

    await asyncio.gather(*(worker() for _ in range(min(并发数, 请求次数))))


async def main():
    global 测试地址
    检查本地端口状态()
    端口接收端, 端口发送端 = multiprocessing.Pipe(duplex=False)
    服务进程 = multiprocessing.Process(target=运行本地服务, args=(端口发送端,))
    服务进程.start()
    端口发送端.close()
    测试地址 = f"http://127.0.0.1:{端口接收端.recv()}/"
    端口接收端.close()
    print(f"本地测试地址: {测试地址}")
    try:
        for 并发数 in 并发列表:
            for 名称, 测量函数 in (
                ("requests_rust固定指纹", lambda: 测量requests_rust(并发数)),
                ("requests_rust指纹轮换", lambda: 测量requests_rust(并发数, True)),
                ("curl_cffi", lambda: 测量curl_cffi(并发数)),
            ):
                结果 = await 测量函数()
                耗时 = statistics.median(结果)
                print(
                    f"{名称}: 并发={并发数}，耗时={耗时:.4f}秒，"
                    f"吞吐={请求次数 / 耗时:.2f}请求/秒"
                )
    finally:
        服务进程.terminate()
        服务进程.join(timeout=5)


if __name__ == "__main__":
    multiprocessing.freeze_support()
    asyncio.run(main())
