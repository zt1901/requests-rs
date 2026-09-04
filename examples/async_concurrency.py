import asyncio

from requests_rs import requests


# 【可调参数】同一个AsyncSession会复用DNS、Cookie和匹配的连接池。
目标地址列表 = [
    "https://example.com/?task=1",
    "https://example.com/?task=2",
    "https://example.com/?task=3",
]
指纹版本 = "edge152"
最大连接数 = 10
默认代理地址 = ""  # 留空表示直连；也可填写http://或socks5://代理。


async def 请求一条(会话: requests.AsyncSession, 地址: str) -> tuple[str, int, str]:
    响应 = await 会话.get(
        地址,
        proxy=默认代理地址 or None,
        headers={"accept": "text/html,application/xhtml+xml"},
    )
    响应.raise_for_status()
    return 地址, 响应.status_code, 响应.fingerprint_id


async def 异步主程序() -> None:
    async with requests.AsyncSession(
        impersonate=指纹版本,
        max_connections=最大连接数,
        timeout=30,
    ) as 会话:
        结果列表 = await asyncio.gather(*(请求一条(会话, 地址) for 地址 in 目标地址列表))
    for 地址, 状态码, 指纹编号 in 结果列表:
        print(状态码, 指纹编号, 地址)


def main() -> None:
    asyncio.run(异步主程序())


if __name__ == "__main__":
    main()
