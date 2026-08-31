from __future__ import annotations

import asyncio
from concurrent.futures import ThreadPoolExecutor
from contextlib import suppress
import multiprocessing
import statistics
import time

import psutil


# 【可调参数】使用独立进程比较原生PyO3 Future与Python线程池的控制端成本。
测试版本 = "firefox151"
测试请求总数 = 20_000
同Tick并发数 = 10
预热请求数 = 200
重复轮数 = 3


async def 处理请求(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    try:
        while True:
            try:
                await reader.readuntil(b"\r\n\r\n")
            except asyncio.IncompleteReadError:
                break
            响应体 = b"runtime-bridge-ok"
            writer.write(
                b"HTTP/1.1 200 OK\r\n"
                + f"Content-Length: {len(响应体)}\r\n".encode()
                + b"Connection: keep-alive\r\n\r\n"
                + 响应体
            )
            await writer.drain()
    except (ConnectionError, asyncio.LimitOverrunError):
        pass
    finally:
        writer.close()
        with suppress(ConnectionError):
            await writer.wait_closed()


def 运行本地服务(端口发送端) -> None:
    async def 启动服务() -> None:
        服务 = await asyncio.start_server(处理请求, "127.0.0.1", 0, backlog=1024)
        端口发送端.send(服务.sockets[0].getsockname()[1])
        端口发送端.close()
        async with 服务:
            await 服务.serve_forever()

    asyncio.run(启动服务())


class 计数事件循环(asyncio.SelectorEventLoop):
    def __init__(self) -> None:
        super().__init__()
        self.创建Future数量 = 0
        self.创建Task数量 = 0

    def create_future(self):
        self.创建Future数量 += 1
        return super().create_future()


def 安装Task计数器(事件循环: 计数事件循环) -> None:
    def 创建Task(loop, coroutine, **kwargs):
        事件循环.创建Task数量 += 1
        return asyncio.Task(coroutine, loop=loop, **kwargs)

    事件循环.set_task_factory(创建Task)


async def 运行批次(请求函数, 请求数量: int) -> None:
    已完成 = 0
    while 已完成 < 请求数量:
        本批数量 = min(同Tick并发数, 请求数量 - 已完成)
        响应列表 = await asyncio.gather(*(请求函数() for _ in range(本批数量)))
        if any(响应.content != b"runtime-bridge-ok" for 响应 in 响应列表):
            raise AssertionError("本地响应内容不一致")
        已完成 += 本批数量


async def 测量原生Future(测试地址: str, 事件循环: 计数事件循环) -> dict:
    from requests_rust import AsyncSession

    async with AsyncSession(
        impersonate=测试版本,
        max_connections=同Tick并发数,
    ) as 会话:
        async def 请求一次():
            return await 会话.get(测试地址)

        await 运行批次(请求一次, 预热请求数)
        事件循环.创建Future数量 = 0
        事件循环.创建Task数量 = 0
        进程 = psutil.Process()
        初始CPU = 进程.cpu_times()
        初始RSS = 进程.memory_info().rss
        初始线程 = 进程.num_threads()
        开始时间 = time.perf_counter()
        await 运行批次(请求一次, 测试请求总数)
        墙钟秒数 = time.perf_counter() - 开始时间
        结束CPU = 进程.cpu_times()
        return {
            "mode": "native_future",
            "wall_seconds": 墙钟秒数,
            "cpu_seconds": (结束CPU.user - 初始CPU.user) + (结束CPU.system - 初始CPU.system),
            "future_count": 事件循环.创建Future数量,
            "task_count": 事件循环.创建Task数量,
            "threads_before": 初始线程,
            "threads_after": 进程.num_threads(),
            "rss_delta": 进程.memory_info().rss - 初始RSS,
        }


async def 测量线程池(测试地址: str, 事件循环: 计数事件循环) -> dict:
    from requests_rust import Session

    with Session(
        impersonate=测试版本,
        max_connections=同Tick并发数,
    ) as 会话:
        async def 请求一次():
            return await asyncio.to_thread(会话.get, 测试地址)

        await 运行批次(请求一次, 预热请求数)
        事件循环.创建Future数量 = 0
        事件循环.创建Task数量 = 0
        进程 = psutil.Process()
        初始CPU = 进程.cpu_times()
        初始RSS = 进程.memory_info().rss
        初始线程 = 进程.num_threads()
        开始时间 = time.perf_counter()
        await 运行批次(请求一次, 测试请求总数)
        墙钟秒数 = time.perf_counter() - 开始时间
        结束CPU = 进程.cpu_times()
        return {
            "mode": "threadpool",
            "wall_seconds": 墙钟秒数,
            "cpu_seconds": (结束CPU.user - 初始CPU.user) + (结束CPU.system - 初始CPU.system),
            "future_count": 事件循环.创建Future数量,
            "task_count": 事件循环.创建Task数量,
            "threads_before": 初始线程,
            "threads_after": 进程.num_threads(),
            "rss_delta": 进程.memory_info().rss - 初始RSS,
        }


def 运行客户端(模式: str, 测试地址: str, 结果发送端) -> None:
    事件循环 = 计数事件循环()
    asyncio.set_event_loop(事件循环)
    安装Task计数器(事件循环)
    线程池 = ThreadPoolExecutor(max_workers=同Tick并发数, thread_name_prefix="bridge-threadpool")
    事件循环.set_default_executor(线程池)
    try:
        测量函数 = 测量原生Future if 模式 == "native_future" else 测量线程池
        结果 = 事件循环.run_until_complete(测量函数(测试地址, 事件循环))
        结果发送端.send(结果)
    finally:
        结果发送端.close()
        事件循环.run_until_complete(事件循环.shutdown_asyncgens())
        线程池.shutdown(wait=True)
        事件循环.close()


def 执行一次(模式: str, 测试地址: str) -> dict:
    结果接收端, 结果发送端 = multiprocessing.Pipe(duplex=False)
    客户端进程 = multiprocessing.Process(target=运行客户端, args=(模式, 测试地址, 结果发送端))
    客户端进程.start()
    结果发送端.close()
    结果 = 结果接收端.recv()
    结果接收端.close()
    客户端进程.join(timeout=120)
    if 客户端进程.exitcode != 0:
        raise RuntimeError(f"{模式}客户端进程失败: {客户端进程.exitcode}")
    结果["requests_per_second"] = 测试请求总数 / 结果["wall_seconds"]
    结果["cpu_us_per_request"] = 结果["cpu_seconds"] * 1_000_000 / 测试请求总数
    return 结果


def main() -> None:
    端口接收端, 端口发送端 = multiprocessing.Pipe(duplex=False)
    服务进程 = multiprocessing.Process(target=运行本地服务, args=(端口发送端,))
    服务进程.start()
    端口发送端.close()
    测试地址 = f"http://127.0.0.1:{端口接收端.recv()}/"
    端口接收端.close()
    结果集合 = {"native_future": [], "threadpool": []}
    try:
        for 轮次 in range(重复轮数):
            模式顺序 = ("native_future", "threadpool") if 轮次 % 2 == 0 else ("threadpool", "native_future")
            for 模式 in 模式顺序:
                结果 = 执行一次(模式, 测试地址)
                结果集合[模式].append(结果)
                print(结果)
    finally:
        服务进程.terminate()
        服务进程.join(timeout=5)

    for 模式, 结果列表 in 结果集合.items():
        print(
            模式,
            {
                "median_rps": statistics.median(结果["requests_per_second"] for 结果 in 结果列表),
                "median_cpu_us_per_request": statistics.median(结果["cpu_us_per_request"] for 结果 in 结果列表),
                "median_future_count": statistics.median(结果["future_count"] for 结果 in 结果列表),
                "median_task_count": statistics.median(结果["task_count"] for 结果 in 结果列表),
                "threads": [(结果["threads_before"], 结果["threads_after"]) for 结果 in 结果列表],
            },
        )


if __name__ == "__main__":
    multiprocessing.freeze_support()
    main()
