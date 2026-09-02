import json
import re
from pathlib import Path

HTTP3_FIXTURE_GLOB = "tests/fixtures/*_schema2.json"
PROJECT_DIR = Path(__file__).resolve().parents[1]


# 修改这些路径后可右键运行，把最新采集记录固化到Python wheel。
项目目录 = Path(__file__).resolve().parents[1]
记录文件 = 项目目录.parent / "fingerprint_records.json"
浏览器基准文件 = 项目目录 / "browser_baselines.json"
输出文件 = 项目目录 / "fingerprints.json"
最低Chrome版本 = 146



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


def 需要单火种(profile):
    return profile.startswith(("chrome", "edge", "firefox"))


def 读取记录(path):
    document = json.loads(path.read_text(encoding="utf-8"))
    if isinstance(document, list):
        return document
    if isinstance(document, dict) and isinstance(document.get("records"), list):
        return document["records"]
    raise RuntimeError(f"{path} 必须是指纹数组或包含 records 数组的 MCP envelope")


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
    records = 读取记录(记录文件)
    baselines = []
    if 浏览器基准文件.exists():
        baselines = 读取记录(浏览器基准文件)
    baseline_profiles = {识别版本(record) for record in baselines if 可嵌入(record)}
    http3_templates = {}
    for path in PROJECT_DIR.glob(HTTP3_FIXTURE_GLOB):
        document = json.loads(path.read_text(encoding="utf-8"))
        fixture_records = document if isinstance(document, list) else document.get("records", [])
        for record in fixture_records:
            profile = record.get("profile")
            if record.get("schema_version") == 2 and record.get("http3"):
                http3_templates[profile] = record["http3"]
    baselines = [
        {**record, "schema_version": 2, "http3": http3_templates[profile]}
        if (profile := record.get("profile")) in http3_templates
        else record
        for record in baselines
    ]
    embedded = []
    counts = {}
    sources = baselines + [
        record for record in records
        if 识别版本(record) not in baseline_profiles
    ]
    grouped = {}
    for record in sources:
        if not 可嵌入(record):
            continue
        profile = 识别版本(record)
        if profile.startswith("chrome") and int(profile.removeprefix("chrome")) < 最低Chrome版本:
            continue

        grouped.setdefault(profile, []).append(record)

    for profile, profile_records in grouped.items():
        if 需要单火种(profile):
            seed = next(
                (
                    record for record in profile_records
                    if 41 not in record.get("tls", {}).get("extensions", [])
                ),
                profile_records[0],
            )
            profile_records = [seed]
        for record in profile_records:
            item = {
                "schema_version": record.get("schema_version", 1),
                "id": record["id"],
                "profile": profile,
                "tls": record["tls"],
                "http": record["http"],
            }
            if record.get("http3"):
                item["http3"] = record["http3"]
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
