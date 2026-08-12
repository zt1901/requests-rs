"""Facebook Groups 的 requests_rust 请求基类。"""

from __future__ import annotations

import asyncio
import random
import re
import string
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any
from urllib.parse import quote, unquote, urlsplit, urlunsplit

from requests_rust import AsyncSession, Response


# 本地直接运行的默认代理；传入代理时优先使用传入值。
默认代理用户名 = "customer-liuxianqun"
默认代理密码 = "xiaoxi"
默认代理地址 = "proxy.ipipgo.com:31212"
_IPIPGO_USER_OPTION_RE = re.compile(r"-(?:country|region|city|session|time)-[^-@]+")


class 可重试请求错误(Exception):
    """网络、5xx 或 429 响应触发的立即重试。"""


class 可重试GraphQL限流错误(Exception):
    """GraphQL HTTP 200 响应中的限流业务错误。"""


class 不可重试HTTP错误(Exception):
    """原 crawler 不对普通 4xx 立即重试，保留响应正文供定位。"""


def 默认代理() -> str:
    """返回本地测试使用的代理 URL。"""
    return f"http://{quote(默认代理用户名)}:{quote(默认代理密码)}@{默认代理地址}"


def _随机会话ID(length: int = 12) -> str:
    alphabet = string.ascii_letters + string.digits
    return "".join(random.choices(alphabet, k=length))


def _带随机代理会话(proxy_url: str | None) -> str | None:
    """为带认证的代理生成一次粘性 session；没有认证信息时原样使用。"""
    if proxy_url is None:
        return None
    parsed = urlsplit(proxy_url)
    if not parsed.username or not parsed.hostname or not parsed.password:
        return proxy_url
    username = unquote(parsed.username)
    base_username = _IPIPGO_USER_OPTION_RE.sub("", username)
    session_username = f"{base_username}-session-{_随机会话ID()}-time-5"
    port = f":{parsed.port}" if parsed.port else ""
    netloc = f"{quote(session_username)}:{quote(unquote(parsed.password))}@{parsed.hostname}{port}"
    return urlunsplit((parsed.scheme, netloc, parsed.path, parsed.query, parsed.fragment))


class RequestsRustCrawler:
    """使用 requests_rust 的长期 Session 请求封装。"""

    # 保留原 CurlCrawler 的“按传输 profile 强制覆盖 UA”语义。
    指纹请求头 = {
        "chrome146": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        "chrome150": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36",
        "chrome142": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36",
        "firefox151": "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:151.0) Gecko/20100101 Firefox/151.0",
    }
    默认指纹版本 = "chrome142"
    请求并发 = 20
    解析线程数 = 8

    def __init__(
        self,
        *,
        proxy: str | None = None,
        fallback_proxy: str | None = None,
        fingerprints_path: str | Path | None = None,
        impersonate: str | None = None,
        fingerprint_rotation: bool = False,
    ) -> None:
        self.proxy = proxy or 默认代理()
        self.fallback_proxy = fallback_proxy or self.proxy
        self.impersonate = impersonate or self.默认指纹版本
        self.session = AsyncSession(
            impersonate=self.impersonate,
            fingerprints_path=fingerprints_path,
            fingerprint_rotation=fingerprint_rotation,
        )
        self.parse_thread_pool = ThreadPoolExecutor(max_workers=self.解析线程数)
        self.request_semaphore = asyncio.Semaphore(self.请求并发)
        self.request_attempts = 0
        self.request_retries = 0

    async def process(
        self,
        method: str,
        url: str,
        *,
        headers: dict[str, str] | None = None,
        cookies: dict[str, str] | None = None,
        proxy_state: dict[str, Any] | None = None,
        **kwargs: Any,
    ) -> Response:
        """每次独立请求使用一个粘性代理；失败后切换主/备用代理源。"""
        headers = dict(headers or {})
        # 原项目的 CurlCrawler 在 process() 内覆盖传入 UA，此处保留该行为。
        if user_agent := self.指纹请求头.get(self.impersonate):
            headers["user-agent"] = user_agent
        if proxy_state is None:
            proxy_state = {}
        sources = proxy_state.setdefault("sources", [self.proxy, self.fallback_proxy])
        proxy_state.setdefault("source_index", 0)
        proxy_state.setdefault("proxy", _带随机代理会话(sources[proxy_state["source_index"]]))
        proxy_state.setdefault("generation", 0)
        last_error: Exception | None = None
        for attempt in range(5):
            self.request_attempts += 1
            try:
                async with self.request_semaphore:
                    response = await self.session.request(
                        method,
                        url,
                        headers=headers,
                        cookies=cookies,
                        proxy=proxy_state["proxy"],
                        **kwargs,
                    )
                # 对齐原 CurlCrawler 的 discard_cookies=True：本测试链路不保留响应 Set-Cookie。
                self.session.cookies.clear()
                if response.status_code >= 500 or response.status_code == 429:
                    raise 可重试请求错误(f"HTTP {response.status_code}: {response.text[:300]!r}")
                if response.status_code >= 400:
                    raise 不可重试HTTP错误(
                        f"HTTP {response.status_code}: {response.url}，响应前缀={response.text[:1000]!r}"
                    )
                if "/api/graphql" in url:
                    prefix = response.text[:2048].lower()
                    if '"code":1675004' in prefix or "rate limit exceeded" in prefix:
                        raise 可重试GraphQL限流错误("Facebook GraphQL 限流")
                return response
            except (RuntimeError, 可重试请求错误, 可重试GraphQL限流错误) as error:
                last_error = error
                if attempt == 4:
                    raise
                self.request_retries += 1
                proxy_state["source_index"] = (proxy_state["source_index"] + 1) % len(sources)
                proxy_state["proxy"] = _带随机代理会话(sources[proxy_state["source_index"]])
                proxy_state["generation"] += 1
                print(f"请求重试 {attempt + 2}/5: {type(error).__name__}", flush=True)
        raise RuntimeError("请求重试状态异常") from last_error

    async def get(self, url: str, **kwargs: Any) -> Response:
        return await self.process("GET", url, **kwargs)

    async def post(self, url: str, **kwargs: Any) -> Response:
        return await self.process("POST", url, **kwargs)

    async def aclose(self) -> None:
        await self.session.close()
        self.parse_thread_pool.shutdown(wait=False)
