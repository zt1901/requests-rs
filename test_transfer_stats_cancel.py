from __future__ import annotations

import asyncio
import gc
import os

import psutil

from requests_rs import requests

AsyncSession = requests.AsyncSession


# 可右键运行；黑洞地址只用于让计量隧道停留在连接阶段，不发送业务数据。
取消任务数 = 200
黑洞地址 = "https://10.255.255.1/"


async def main() -> None:
    process = psutil.Process(os.getpid())
    async def run_batch() -> None:
        session = AsyncSession(
            impersonate="chrome146",
            fingerprint_rotation=False,
            max_connections=取消任务数,
            verify=False,
        )
        tasks = [
            asyncio.create_task(
                session.get(
                    黑洞地址,
                    transfer_stats=True,
                    timeout=30,
                )
            )
            for _ in range(取消任务数)
        ]
        await asyncio.sleep(0.2)
        for task in tasks:
            task.cancel()
        results = await asyncio.gather(*tasks, return_exceptions=True)
        assert all(isinstance(result, asyncio.CancelledError) for result in results)
        await session.close()

    await run_batch()
    gc.collect()
    await asyncio.sleep(0.5)
    initial_handles = process.num_handles()
    initial_threads = process.num_threads()
    await run_batch()
    gc.collect()
    await asyncio.sleep(0.5)
    handle_delta = process.num_handles() - initial_handles
    thread_delta = process.num_threads() - initial_threads
    assert handle_delta < 16, handle_delta
    assert thread_delta <= 2, thread_delta
    print(
        f"transfer_stats取消{取消任务数}任务验证通过："
        f"句柄增量={handle_delta}，线程增量={thread_delta}",
        flush=True,
    )


if __name__ == "__main__":
    asyncio.run(main())
