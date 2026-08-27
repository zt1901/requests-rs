"""右键运行：用 requests_rust 验证 Facebook Groups 首包与 GraphQL 分页。"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
from pathlib import Path
from urllib.parse import urlsplit


项目目录 = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(项目目录))
sys.path.insert(0, str(项目目录 / "python"))

from tests.facebook_groups_requests_rust.groups import FacebookGroupsCrawler  # noqa: E402
from tests.facebook_groups_requests_rust.crawler import 默认代理  # noqa: E402


# ══════════════════ 【可调参数】 ══════════════════
# 从浏览器打开的 Facebook 群组页面复制；需要登录时填写浏览器当前 Cookie 与请求 Header。
默认群组链接 = ["https://www.facebook.com/groups/1004276887930426/"]
默认帖子数量 = 30
默认指纹版本 = "chrome142"
默认指纹文件 = 项目目录 / "fingerprints.json"
默认代理地址 = ""
# home：主页 Set-Cookie 原样带到 GraphQL；fake：同名称、同长度但不用真实 Cookie 值。
默认Cookie模式 = "fake"
默认指纹轮换 = False
默认保留指纹连接池 = True
默认指纹连接池数量 = 100
默认缓存Origin数量 = 4
# 空字符串保持浏览器默认的 gzip, deflate, br, zstd 协商；可传 br、zstd 或 gzip 进行流量对比。
默认响应压缩协商 = ""
# ══════════════════════════════════════════════════


def 解析参数() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="requests_rust Facebook Groups 替换验证")
    parser.add_argument("--group-url", action="append", dest="group_urls")
    parser.add_argument("--results-limit", type=int, default=默认帖子数量)
    parser.add_argument("--proxy", default=默认代理地址)
    parser.add_argument("--impersonate", default=默认指纹版本)
    parser.add_argument("--fingerprints-path", default=str(默认指纹文件))
    parser.add_argument("--cookie-mode", choices=("home", "fake"), default=默认Cookie模式)
    parser.add_argument("--fingerprint-rotation", action="store_true", default=默认指纹轮换)
    parser.add_argument("--no-fingerprint-pool", action="store_true", help="轮换时不保存指纹Client和连接池")
    parser.add_argument("--fingerprint-pool-size", type=int, default=默认指纹连接池数量)
    parser.add_argument("--max-cached-origins", type=int, default=默认缓存Origin数量)
    parser.add_argument("--transfer-stats", action="store_true", help="启用每请求TLS/TCP传输层计量，显著影响时效")
    parser.add_argument("--accept-encoding", default=默认响应压缩协商, help="覆盖响应压缩协商，例如 br、zstd 或 gzip")
    parser.add_argument("--quiet", action="store_true", help="仅输出抓取与传输汇总，不打印帖子内容")
    return parser.parse_args()


async def main() -> None:
    args = 解析参数()
    group_urls = args.group_urls or 默认群组链接
    proxy = args.proxy or 默认代理()
    if not proxy:
        raise RuntimeError("Facebook Groups 替换测试必须使用代理")
    proxies = {"http": proxy, "https": proxy}
    crawler = FacebookGroupsCrawler(
        proxies=proxies,
        fallback_proxies=proxies,
        fingerprints_path=args.fingerprints_path,
        impersonate=args.impersonate,
        cookie_mode=args.cookie_mode,
        fingerprint_rotation=args.fingerprint_rotation,
        fingerprint_pool=默认保留指纹连接池 and not args.no_fingerprint_pool,
        fingerprint_pool_size=args.fingerprint_pool_size,
        max_cached_origins=args.max_cached_origins,
        transfer_stats=args.transfer_stats,
        accept_encoding=args.accept_encoding or None,
    )
    try:
        print("请求参数:")
        print(
            json.dumps(
                {
                    "groupUrls": group_urls,
                    "resultsLimit": args.results_limit,
                    "cookieMode": args.cookie_mode,
                    "fingerprintRotation": args.fingerprint_rotation,
                    "fingerprintPool": 默认保留指纹连接池 and not args.no_fingerprint_pool,
                    "fingerprintPoolSize": args.fingerprint_pool_size,
                    "maxCachedOrigins": args.max_cached_origins,
                    "transferStats": args.transfer_stats,
                    "acceptEncoding": args.accept_encoding or "browser-default",
                },
                ensure_ascii=True,
                indent=2,
            )
        )
        print("代理主机:", urlsplit(proxy).hostname)
        results = await crawler.crawl(group_urls, args.results_limit) if args.results_limit else []
        if not args.quiet:
            print("=== Facebook Groups requests_rust 最终结果 ===")
            print(json.dumps(results, ensure_ascii=True, indent=2))
        print(f"抓取帖子数: {len(results)}")
        print(f"请求次数: {crawler.request_attempts}，重试次数: {crawler.request_retries}")
        if args.transfer_stats:
            print(
                f"真实传输字节: 上行={crawler.transfer_upload_bytes}，"
                f"下行={crawler.transfer_download_bytes}，"
                f"合计={crawler.transfer_upload_bytes + crawler.transfer_download_bytes}"
            )
    finally:
        await crawler.aclose()


if __name__ == "__main__":
    asyncio.run(main())
