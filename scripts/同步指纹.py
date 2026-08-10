import json
import re
from pathlib import Path


# 修改这些路径后可右键运行，把最新采集记录固化到Python wheel。
项目目录 = Path(__file__).resolve().parents[1]
记录文件 = 项目目录.parent / "fingerprint_records.json"
浏览器基准文件 = 项目目录 / "browser_baselines.json"
输出文件 = 项目目录 / "fingerprints.json"


def 获取请求头(record, name):
    for header_name, value in record.get("http", {}).get("headers", []):
        if header_name.lower() == name.lower():
            return value
    return ""


def 识别版本(record):
    user_agent = 获取请求头(record, "user-agent")
    patterns = (
        (r"Firefox/(\d+)", "firefox"),
        (r"Edg/(\d+)", "edge"),
        (r"OPR/(\d+)", "opera"),
        (r"Chrome/(\d+)", "chrome"),
        (r"Version/(\d+).+Safari/", "safari"),
    )
    for pattern, browser in patterns:
        match = re.search(pattern, user_agent, re.IGNORECASE)
        if match:
            return f"{browser}{match.group(1)}"
    return None


def 可嵌入(record):
    tls = record.get("tls", {})
    http = record.get("http", {})
    target = http.get("target", "")
    return bool(
        识别版本(record)
        and tls.get("cipher_suites")
        and tls.get("extensions")
        and tls.get("supported_groups")
        and tls.get("signature_algorithms")
        and http.get("headers")
        and "source=wreq" not in target
        and "source=python-package" not in target
    )


def main():
    records = json.loads(记录文件.read_text(encoding="utf-8"))
    baselines = []
    if 浏览器基准文件.exists():
        baselines = json.loads(浏览器基准文件.read_text(encoding="utf-8"))
    baseline_profiles = {识别版本(record) for record in baselines if 可嵌入(record)}
    embedded = []
    counts = {}
    sources = baselines + [
        record for record in records
        if 识别版本(record) not in baseline_profiles
    ]
    for record in sources:
        if not 可嵌入(record):
            continue
        profile = 识别版本(record)
        item = {
            "id": record["id"],
            "profile": profile,
            "tls": record["tls"],
            "http": record["http"],
        }
        embedded.append(item)
        counts[profile] = counts.get(profile, 0) + 1

    if not embedded:
        raise RuntimeError("没有找到包含浏览器版本和完整网络字段的指纹记录")
    输出文件.write_text(
        json.dumps(embedded, ensure_ascii=False, separators=(",", ":")),
        encoding="utf-8",
    )
    print(f"已嵌入 {len(embedded)} 条指纹: {counts}，浏览器基准版本: {sorted(baseline_profiles)}")


if __name__ == "__main__":
    main()
