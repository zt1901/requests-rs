from __future__ import annotations

import asyncio
import json
import re
import statistics
import time
from datetime import UTC, datetime
from typing import Any

from .crawler import RequestsRustCrawler


中继变量 = {
    "__relay_internal__pv__GHLShouldChangeAdIdFieldNamerelayprovider": False,
    "__relay_internal__pv__CometFeedStory_enable_reactor_facepilerelayprovider": False,
    "__relay_internal__pv__CometFeedStory_enable_social_bubblesrelayprovider": False,
    "__relay_internal__pv__CometUFICommentActionLinksRewriteEnabledrelayprovider": False,
    "__relay_internal__pv__CometUFICommentAvatarStickerAnimatedImagerelayprovider": False,
    "__relay_internal__pv__CometUFICommentAutoTranslationTyperelayprovider": "AUTO_TRANSLATE",
    "__relay_internal__pv__CometFeedStory_enable_post_permalink_white_space_clickrelayprovider": False,
    "__relay_internal__pv__IsWorkUserrelayprovider": False,
    "__relay_internal__pv__TestPilotShouldIncludeDemoAdUseCaserelayprovider": False,
    "__relay_internal__pv__FBReels_deprecate_short_form_video_context_gkrelayprovider": False,
    "__relay_internal__pv__FBReels_enable_view_dubbed_audio_type_gkrelayprovider": False,
    "__relay_internal__pv__CometFeedShareMedia_shouldPrefetchShareImagerelayprovider": False,
    "__relay_internal__pv__CometImmersivePhotoCanUserDisable3DMotionrelayprovider": False,
    "__relay_internal__pv__WorkCometIsEmployeeGKProviderrelayprovider": False,
    "__relay_internal__pv__IsMergQAPollsrelayprovider": False,
    "__relay_internal__pv__FBReelsIFUTileContent_reelsIFULikeCountrelayprovider": False,
    "__relay_internal__pv__CometUFIReactionsEnableShortNamerelayprovider": False,
    "__relay_internal__pv__CometUFIShareActionMigrationrelayprovider": True,
    "__relay_internal__pv__CometUFISingleLineUFIrelayprovider": False,
    "__relay_internal__pv__relay_provider_comet_ufi_ssr_seo_deferrelayprovider": True,
    "__relay_internal__pv__CometUFI_dedicated_comment_routable_dialog_gkrelayprovider": True,
    "__relay_internal__pv__FBReelsMediaFooter_comet_enable_reels_ads_gkrelayprovider": True,
    "__relay_internal__pv__ReelsIFUCard_reelsIFULikeCountrelayprovider": False,
    "__relay_internal__pv__FBReelsIFUTileContent_reelsIFUPlayOnHoverrelayprovider": False,
    "__relay_internal__pv__GroupsCometGYSJFeedItemHeightrelayprovider": 150,
    "__relay_internal__pv__ShouldEnableBakedInTextStoriesrelayprovider": False,
    "__relay_internal__pv__StoriesShouldIncludeFbNotesrelayprovider": False,
}


class FacebookGroupsCrawler(RequestsRustCrawler):
    """Facebook 群组帖子抓取爬虫，仅替换底层发包库。"""

    GRAPHQL_URL = "https://www.facebook.com/api/graphql/"
    DOC_ID = "27489248654050164"
    FRIENDLY_NAME = "GroupsCometFeedRegularStoriesPaginationQuery"
    POSTS_PER_PAGE = 3
    REQUEST_TIMEOUT = 30
    GRAPHQL_TIMEOUT = 60
    RETRY_TIMES = 5

    headers = {
        # 原 curl_cffi chrome142 impersonate 自动注入的页面导航 Header。
        # 这是 Facebook 群组首页这一具体业务动作的固定请求模板，不属于指纹库默认配置。
        "sec-ch-ua": '"Chromium";v="142", "Google Chrome";v="142", "Not_A Brand";v="99"',
        "sec-ch-ua-mobile": "?0",
        "sec-ch-ua-platform": '"Windows"',
        "upgrade-insecure-requests": "1",
        "accept-language": "en-US,en;q=0.9",
        "user-agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:135.0) Gecko/20100101 Firefox/135.0",
        "accept": "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
        "sec-fetch-site": "none",
        "sec-fetch-mode": "navigate",
        "sec-fetch-user": "?1",
        "sec-fetch-dest": "document",
        "accept-encoding": "gzip, deflate, br, zstd",
        "priority": "u=0, i",
    }

    class Parser:
        @staticmethod
        def _extract_one(text: str, patterns: tuple[str, ...], name: str) -> str:
            for pattern in patterns:
                match = re.search(pattern, text)
                if match:
                    return match.group(1)
            raise RuntimeError(f"页面中未找到 {name}，Facebook 页面结构可能已更新")

        def extract_page_data(self, html: str) -> dict[str, str]:
            cursors = re.findall(r'"end_cursor":"(Cg8TZXhpc3RpbmdfdW5pdF9jb3VudA[^"\\]+)', html)
            if not cursors:
                raise RuntimeError("页面中未找到群组 Feed cursor，可能需要登录或群组已限制访问")
            return {
                "group_id": self._extract_one(
                    html,
                    (r'content="fb://group/(\d+)"', r'content="fb://group/\?id=(\d+)"'),
                    "group_id",
                ),
                "lsd": self._extract_one(
                    html,
                    (r'\["LSD",\[\],\{"token":"([^"]+)"', r'"LSD".*?"token":"([^"]+)"'),
                    "lsd",
                ),
                "jazoest": self._extract_one(html, (r"jazoest=(\d+)", r'"jazoest":"?(\d+)'), "jazoest"),
                "spin_r": self._extract_one(html, (r'"__spin_r":(\d+)',), "__spin_r"),
                "spin_b": self._extract_one(html, (r'"__spin_b":"([^"]+)',), "__spin_b"),
                "spin_t": self._extract_one(html, (r'"__spin_t":(\d+)',), "__spin_t"),
                "hsi": self._extract_one(html, (r'"__hsi":"?(\d+)', r'"__eqmc".*?"e":"(\d+)'), "__hsi"),
                "cursor": cursors[-1],
            }

        @staticmethod
        def _decode_stream(text: str) -> list[dict[str, Any]]:
            # Facebook GraphQL 响应常以前缀防止 JSON 被当作脚本执行；这不是 JSON 内容。
            if text.startswith("for (;;);"):
                text = text[len("for (;;);") :]
            decoder = json.JSONDecoder()
            values: list[dict[str, Any]] = []
            index = 0
            while index < len(text):
                while index < len(text) and text[index].isspace():
                    index += 1
                if index >= len(text):
                    break
                try:
                    value, index = decoder.raw_decode(text, index)
                except json.JSONDecodeError as error:
                    raise RuntimeError(f"GraphQL 响应无法解析: {text[index:index + 200]!r}") from error
                if isinstance(value, dict):
                    values.append(value)
            return values

        @staticmethod
        def _get_path(data: Any, *keys: str, default: Any = None) -> Any:
            value = data
            for key in keys:
                if not isinstance(value, dict):
                    return default
                value = value.get(key)
            return default if value is None else value

        @staticmethod
        def _collect_values(data: Any, keys: set[str]) -> list[str]:
            values: list[str] = []

            def walk(value: Any) -> None:
                if isinstance(value, dict):
                    for key, child in value.items():
                        if key in keys and isinstance(child, str) and child and child not in values:
                            values.append(child)
                        walk(child)
                elif isinstance(value, list):
                    for child in value:
                        walk(child)

            walk(data)
            return values

        @classmethod
        def _attachments(cls, attachments: list[dict[str, Any]]) -> list[dict[str, Any]]:
            values = []
            for item in attachments:
                attachment = (item.get("styles") or {}).get("attachment") or {}
                media = attachment.get("media") or item.get("media") or {}
                values.append(
                    {
                        "type": media.get("__typename"),
                        "title": cls._get_path(attachment, "title_with_entities", "text"),
                        "url": cls._get_path(
                            attachment,
                            "story_attachment_link_renderer",
                            "attachment",
                            "web_link",
                            "url",
                        )
                        or attachment.get("url"),
                        "imageUrls": cls._collect_values(attachment or item, {"uri"}),
                        "videoUrls": cls._collect_values(
                            attachment or item,
                            {"playable_url", "playable_url_quality_hd", "browser_native_hd_url", "browser_native_sd_url"},
                        ),
                    }
                )
            return values

        @classmethod
        def _post(cls, edge: dict[str, Any], group_url: str, group_id: str) -> dict[str, Any]:
            node = edge.get("node") or {}
            sections = node.get("comet_sections") or {}
            content = cls._get_path(sections, "content", "story", default={})
            actors = content.get("actors") or node.get("actors") or []
            author = actors[0] if actors else (node.get("feedback") or {}).get("owning_profile") or {}
            message = (node.get("message") or {}).get("text") or (content.get("message") or {}).get("text")
            if not message:
                message = cls._get_path(content, "comet_sections", "message", "story", "message", "text")
            feedback = cls._get_path(
                sections, "feedback", "story", "story_ufi_container", "story", "feedback_context",
                "feedback_target_with_context", "comet_ufi_summary_and_actions_renderer", "feedback", default={}
            )
            created = node.get("creation_time")
            created_time = datetime.fromtimestamp(created, UTC).isoformat() if isinstance(created, (int, float)) else None
            group = node.get("to") or {}
            reactions = [
                {"name": cls._get_path(item, "node", "localized_name"), "count": item.get("reaction_count", 0)}
                for item in cls._get_path(feedback, "top_reactions", "edges", default=[])
            ]
            return {
                "postId": node.get("post_id"),
                "permalinkUrl": node.get("permalink_url"),
                "message": message or "",
                "createdTime": created_time,
                "group": {"groupId": group.get("id") or group_id, "name": group.get("name"), "url": group.get("url") or group_url},
                "author": {"id": author.get("id"), "name": author.get("name"), "url": author.get("url"), "profilePictureUrl": cls._get_path(author, "profile_picture", "uri")},
                "engagement": {
                    "reactionsCount": cls._get_path(feedback, "reaction_count", "count", default=0),
                    "commentsCount": cls._get_path(feedback, "comment_rendering_instance", "comments", "total_count", default=0),
                    "sharesCount": cls._get_path(feedback, "share_count", "count", default=0),
                    "topReactions": reactions,
                },
                "attachments": cls._attachments(node.get("attachments") or []),
                "isSponsored": node.get("sponsored_data") is not None,
                "isRepost": bool(node.get("work_is_repost") or node.get("attached_story")),
            }

        def parse_graphql(self, text: str, group_url: str, group_id: str) -> tuple[list[dict[str, Any]], str | None, bool]:
            edges: list[dict[str, Any]] = []
            cursor = None
            has_next_page = False
            for item in self._decode_stream(text):
                if "error" in item:
                    raise RuntimeError(
                        f"Facebook GraphQL 业务错误 {item.get('error')}: "
                        f"{item.get('errorSummary') or item.get('errorDescription') or item}"
                    )
                data = item.get("data") or {}
                group_feed = (data.get("node") or {}).get("group_feed") or {}
                edges.extend(group_feed.get("edges") or [])
                if len(item.get("path") or []) >= 2 and (item.get("path") or [])[-2] == "edges" and data.get("node"):
                    edges.append(data)
                page_info = group_feed.get("page_info") or data.get("page_info") or {}
                if page_info.get("end_cursor"):
                    cursor = page_info["end_cursor"]
                    has_next_page = bool(page_info.get("has_next_page"))
            return [self._post(edge, group_url, group_id) for edge in edges], cursor, has_next_page

    def __init__(
        self,
        *,
        proxies: dict | None = None,
        fallback_proxies: dict | None = None,
        cookie_mode: str = "home",
        **kwargs: Any,
    ) -> None:
        if cookie_mode not in {"home", "fake"}:
            raise ValueError("cookie_mode 只能是 home 或 fake")
        proxy = (proxies or {}).get("https") or (proxies or {}).get("http")
        fallback_proxy = (fallback_proxies or {}).get("https") or (fallback_proxies or {}).get("http")
        super().__init__(proxy=proxy, fallback_proxy=fallback_proxy, **kwargs)
        self._parser = self.Parser()
        self.cookie_mode = cookie_mode

    async def _parse(self, function: Any, *args: Any) -> Any:
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(self.parse_thread_pool, function, *args)

    @staticmethod
    def _homepage_cookies(response, fake: bool) -> dict[str, str]:
        """读取主页 Set-Cookie；fake 模式仅保留名称和值长度，不保留真实会话值。"""
        cookies: dict[str, str] = {}
        for value in response.headers.get_list("set-cookie"):
            pair = value.split(";", 1)[0]
            if "=" not in pair:
                continue
            name, cookie_value = pair.split("=", 1)
            if name:
                cookies[name] = "A" * max(1, len(cookie_value)) if fake else cookie_value
        return cookies

    @staticmethod
    def _variables(cursor: str, group_id: str, count: int) -> dict[str, Any]:
        variables = {
            "count": count,
            "cursor": cursor,
            "feedLocation": "GROUP",
            "feedType": "DISCUSSION",
            "feedbackSource": 0,
            "filterTopicId": None,
            "focusCommentID": None,
            "privacySelectorRenderLocation": "COMET_STREAM",
            "referringStoryRenderLocation": None,
            "renderLocation": "group",
            "scale": 1,
            "sortingSetting": "TOP_POSTS",
            "stream_initial_count": 1,
            "useDefaultActor": False,
            "id": group_id,
        }
        variables.update(中继变量)
        return variables

    async def _crawl_one(self, group_url: str, results_limit: int) -> list[dict[str, Any]]:
        # 首页与本组所有 GraphQL 分页共享同一个 sticky 代理 session。
        # 只有 process() 发生可重试失败时才会替换该 sessionid。
        proxy_state: dict[str, Any] = {}
        page_data = None
        last_error = None
        for _ in range(self.RETRY_TIMES):
            response = await self.get(
                group_url,
                headers=self.headers,
                timeout=self.REQUEST_TIMEOUT,
                proxy_state=proxy_state,
            )
            try:
                page_data = await self._parse(self._parser.extract_page_data, response.text)
                print(
                    f"主页成功: 群组={page_data['group_id']}，cursor已获取，HTTP={response.status_code}",
                    flush=True,
                )
                break
            except RuntimeError as error:
                last_error = error
        if page_data is None:
            raise RuntimeError(
                f"{group_url} 首页缺少群组协议数据；最后响应状态={response.status_code}，"
                f"最终URL={response.url}，内容前缀={response.text[:300]!r}"
            ) from last_error

        request_cookies = self._homepage_cookies(response, fake=self.cookie_mode == "fake")
        print(f"主页 Cookie: 模式={self.cookie_mode}，数量={len(request_cookies)}", flush=True)

        headers = {
            "accept": "*/*",
            "content-type": "application/x-www-form-urlencoded",
            "origin": "https://www.facebook.com",
            "referer": group_url,
            # curl_cffi 的浏览器 impersonate 会隐式提供这些 XHR 上下文 Header。
            # requests_rust 不猜测业务上下文，因此替换测试显式保留实际浏览器语义。
            "sec-ch-ua": '"Chromium";v="142", "Google Chrome";v="142", "Not_A Brand";v="99"',
            "sec-ch-ua-mobile": "?0",
            "sec-ch-ua-platform": '"Windows"',
            "sec-fetch-dest": "empty",
            "sec-fetch-mode": "cors",
            "sec-fetch-site": "same-origin",
            "x-asbd-id": "359341",
            "x-fb-friendly-name": self.FRIENDLY_NAME,
            "x-fb-lsd": page_data["lsd"],
        }
        cursor = page_data["cursor"]
        posts: list[dict[str, Any]] = []
        request_number = 0
        分页耗时列表: list[float] = []
        while len(posts) < results_limit:
                request_number += 1
                count = min(self.POSTS_PER_PAGE, results_limit - len(posts))
                form = {
                    "av": "0",
                    "__aaid": "0",
                    "__user": "0",
                    "__a": "1",
                    "__req": format(request_number + 11, "x"),
                    "dpr": "1",
                    "__ccg": "EXCELLENT",
                    "__rev": page_data["spin_r"],
                    "__hsi": page_data["hsi"],
                    "__comet_req": "15",
                    "lsd": page_data["lsd"],
                    "jazoest": page_data["jazoest"],
                    "__spin_r": page_data["spin_r"],
                    "__spin_b": page_data["spin_b"],
                    "__spin_t": page_data["spin_t"],
                    "__crn": "comet.fbweb.CometGroupDiscussionRoute",
                    "fb_api_caller_class": "RelayModern",
                    "fb_api_req_friendly_name": self.FRIENDLY_NAME,
                    "server_timestamps": "true",
                    "variables": json.dumps(
                        self._variables(cursor, page_data["group_id"], count), separators=(",", ":")
                    ),
                    "doc_id": self.DOC_ID,
                }
                page_posts: list[dict[str, Any]] = []
                next_cursor = None
                has_next_page = False
                for _ in range(self.RETRY_TIMES):
                    翻页开始时间 = time.perf_counter()
                    response = await self.post(
                        self.GRAPHQL_URL,
                        headers=headers,
                        data=form,
                        cookies=request_cookies,
                        timeout=self.GRAPHQL_TIMEOUT,
                        proxy_state=proxy_state,
                    )
                    page_posts, next_cursor, has_next_page = await self._parse(
                        self._parser.parse_graphql,
                        response.text,
                        group_url,
                        page_data["group_id"],
                    )
                    翻页耗时 = time.perf_counter() - 翻页开始时间
                    print(
                        f"分页成功: 第{request_number}页，帖子={len(page_posts)}，"
                        f"下一页={'是' if has_next_page and next_cursor else '否'}，HTTP={response.status_code}，"
                        f"耗时={翻页耗时:.3f}秒",
                        flush=True,
                    )
                    if page_posts and (len(page_posts) >= count or has_next_page):
                        分页耗时列表.append(翻页耗时)
                        break
                posts.extend(page_posts[: results_limit - len(posts)])
                if len(posts) >= results_limit or not next_cursor or not has_next_page:
                    if 分页耗时列表:
                        sorted_times = sorted(分页耗时列表)
                        p95_index = min(len(sorted_times) - 1, int(len(sorted_times) * 0.95))
                        print(
                            "分页时效汇总: "
                            f"页数={len(分页耗时列表)}，"
                            f"总耗时={sum(分页耗时列表):.3f}秒，"
                            f"平均={statistics.mean(分页耗时列表):.3f}秒，"
                            f"中位数={statistics.median(分页耗时列表):.3f}秒，"
                            f"P95={sorted_times[p95_index]:.3f}秒，"
                            f"最快={min(分页耗时列表):.3f}秒，"
                            f"最慢={max(分页耗时列表):.3f}秒",
                            flush=True,
                        )
                    return posts
                if next_cursor == cursor:
                    raise RuntimeError(f"{group_url} 分页 cursor 未变化，停止避免无限循环")
                cursor = next_cursor
        return posts

    async def crawl(self, group_urls: list[str], results_limit: int, max_count: int | None = None) -> list[dict[str, Any]]:
        if max_count is not None:
            posts: list[dict[str, Any]] = []
            for url in group_urls:
                remaining = max_count - len(posts)
                if remaining <= 0:
                    break
                posts.extend(await self._crawl_one(url, min(results_limit, remaining)))
            return posts
        all_posts: list[dict[str, Any]] = []
        for start in range(0, len(group_urls), 50):
            batch = group_urls[start:start + 50]
            results = await asyncio.gather(*(self._crawl_one(url, results_limit) for url in batch))
            all_posts.extend(post for group_posts in results for post in group_posts)
        return all_posts
