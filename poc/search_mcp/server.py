"""Web Search MCP server — 架构 B 的 Source 契约实现。

三工具映射架构 B 的 Source 契约：
    search_candidates(query, k) -> 有界候选集（仅导航，非证据）
    open_source(candidate_id)   -> 抓取并固化网页快照，返回 source_ref + content_hash
    read_source(source_ref)     -> 读取快照正文

设计要点：
- 候选注册表按 candidate_id 记账；open 只接受本次会话产生的候选，拒绝集合外 URL。
- open 时抓取正文、写快照、算哈希、生成稳定 source_ref；快照落 snapshots/。
- 阻断内网地址、非 HTTP(S)、超大响应，网页内容视为不可信数据。
- 传输 stdio，既可挂 Claude Desktop / Cline 等客户端，也可被 run.py 进程内直接 import。

逐字取证与审计回放由 run.py 环路（validation-poc.md §5）承担：cheap 模型只在
固化快照内摘引文，程序校验 hash 匹配、引文逐字命中、Claim 有据。
"""
from __future__ import annotations

import hashlib
import ipaddress
import json
import os
import socket
import time
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlparse

import httpx
from ddgs import DDGS
from dotenv import load_dotenv
from mcp.server.fastmcp import FastMCP

load_dotenv(Path(__file__).parent.parent / ".env")

SNAP_DIR = Path(__file__).parent / "snapshots"
SNAP_DIR.mkdir(exist_ok=True)

MAX_BYTES = 4_000_000          # 单页正文上限，防超大响应
FETCH_TIMEOUT = 20.0
CRAWL_TIMEOUT = 120.0          # crawl4ai 带 JS 渲染，抓取较慢
UA = "research-poc/0 (+local validation)"

# 固化后端：crawl4ai 服务（服务器端抓取+渲染+正文抽取）。唯一后端，遇爬不了的资源再调整。
CRAWL4AI_BASE = os.environ.get("CRAWL4AI_BASE_URL", "").rstrip("/")
CRAWL4AI_TOKEN = os.environ.get("CRAWL4AI_TOKEN", "")

mcp = FastMCP("web-search-source")

# 会话内候选与快照注册表；PoC 单进程内存态即可。
_candidates: dict[str, dict] = {}   # candidate_id -> {url, title, snippet}
_snapshots: dict[str, dict] = {}    # source_ref   -> {url, title, text, hash, fetched_at}


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _is_public_http(url: str) -> tuple[bool, str]:
    """校验 URL 为公网 HTTP(S)，阻断内网/环回/非法主机。"""
    try:
        p = urlparse(url)
    except Exception as e:
        return False, f"URL 解析失败: {e}"
    if p.scheme not in ("http", "https"):
        return False, f"仅允许 http(s)，拒绝 scheme={p.scheme!r}"
    host = p.hostname
    if not host:
        return False, "缺少主机名"
    try:
        infos = socket.getaddrinfo(host, None)
    except Exception as e:
        return False, f"DNS 解析失败: {e}"
    for info in infos:
        ip = ipaddress.ip_address(info[4][0])
        if ip.is_private or ip.is_loopback or ip.is_link_local or ip.is_reserved or ip.is_multicast:
            return False, f"目标解析到非公网地址 {ip}，已阻断"
    return True, ""


@mcp.tool()
def search_candidates(query: str, k: int = 6) -> str:
    """搜索并返回有界候选网页集。结果仅供导航选择，标题与摘要不得作为事实证据。

    Args:
        query: 搜索词。
        k: 候选数量上限（1-10）。
    Returns:
        JSON 字符串，含 candidates 列表：candidate_id / title / url / snippet。
    """
    k = max(1, min(int(k), 10))
    query = (query or "").strip()
    if not query:
        return json.dumps({"error": "query 为空"}, ensure_ascii=False)
    out = []
    # 免费搜索后端对密集请求限流（429）；指数退避重试，末次失败才报错。
    results = None
    last_err = None
    for attempt in range(4):
        try:
            with DDGS() as ddgs:
                results = list(ddgs.text(query, max_results=k))
            break
        except Exception as e:
            last_err = e
            if "429" in str(e) or "ratelimit" in str(e).lower():
                time.sleep(2 ** attempt + 1)   # 1,3,5,9s 递增退避
                continue
            break
    if results is None:
        return json.dumps({"error": f"搜索失败: {last_err}", "query": query}, ensure_ascii=False)
    for r in results:
        url = r.get("href") or r.get("url") or ""
        if not url:
            continue
        cid = hashlib.sha1(f"{query}|{url}".encode()).hexdigest()[:12]
        _candidates[cid] = {"url": url, "title": r.get("title", ""), "snippet": r.get("body", "")}
        out.append({
            "candidate_id": cid,
            "title": r.get("title", ""),
            "url": url,
            "snippet": r.get("body", ""),
        })
    return json.dumps({"query": query, "count": len(out), "candidates": out},
                      ensure_ascii=False, indent=2)


@mcp.tool()
def open_source(candidate_id: str) -> str:
    """打开候选网页，抓取正文并固化快照，返回稳定 source_ref。

    只接受 search_candidates 本次会话返回过的 candidate_id；固化后方可作为可引用来源。

    Args:
        candidate_id: search_candidates 返回的候选 ID。
    Returns:
        JSON：source_ref / source_uri / title / content_hash / char_len / fetched_at。
    """
    cand = _candidates.get(candidate_id)
    if not cand:
        return json.dumps({"error": f"未知 candidate_id={candidate_id!r}，须先 search_candidates"},
                          ensure_ascii=False)
    url = cand["url"]
    ok, why = _is_public_http(url)
    if not ok:
        return json.dumps({"error": f"URL 校验失败: {why}", "url": url}, ensure_ascii=False)
    if not CRAWL4AI_BASE or not CRAWL4AI_TOKEN:
        return json.dumps({"error": "未配置 CRAWL4AI_BASE_URL / CRAWL4AI_TOKEN"}, ensure_ascii=False)
    # 固化经 crawl4ai：服务器端抓取+JS 渲染+正文抽取，直接取 markdown。
    try:
        with httpx.Client(timeout=CRAWL_TIMEOUT) as c:
            resp = c.post(f"{CRAWL4AI_BASE}/crawl",
                          headers={"Authorization": f"Bearer {CRAWL4AI_TOKEN}"},
                          json={"urls": [url]})
        if resp.status_code != 200:
            return json.dumps({"error": f"crawl4ai HTTP {resp.status_code}", "url": url},
                              ensure_ascii=False)
        results = resp.json().get("results") or []
        res = results[0] if results else {}
    except Exception as e:
        return json.dumps({"error": f"抓取失败: {e}", "url": url}, ensure_ascii=False)

    if not res.get("success"):
        return json.dumps({"error": f"crawl4ai 抓取未成功 (status={res.get('status_code')})，"
                                    f"资源可能反爬/失效，按原则弃取", "url": url}, ensure_ascii=False)
    final = res.get("url") or url
    ok, why = _is_public_http(final)      # 重定向落点复检，防越界到内网
    if not ok:
        return json.dumps({"error": f"重定向越界: {why}", "final_url": final}, ensure_ascii=False)
    md = res.get("markdown")
    if isinstance(md, dict):
        md = md.get("raw_markdown", "")
    text = (md or "")[:MAX_BYTES]
    if not text.strip():
        return json.dumps({"error": "正文抽取为空（可能为动态渲染或登录墙）", "url": final},
                          ensure_ascii=False)

    content_hash = "sha256:" + hashlib.sha256(text.encode()).hexdigest()
    snap_id = hashlib.sha1(f"{final}|{content_hash}".encode()).hexdigest()[:16]
    source_ref = f"source:web/{snap_id}"
    fetched_at = _now()
    record = {"url": final, "title": cand["title"], "text": text,
              "hash": content_hash, "fetched_at": fetched_at}
    _snapshots[source_ref] = record
    (SNAP_DIR / f"{snap_id}.json").write_text(
        json.dumps({"source_ref": source_ref, **record}, ensure_ascii=False, indent=2),
        encoding="utf-8")
    return json.dumps({
        "source_ref": source_ref,
        "source_uri": final,
        "title": cand["title"],
        "content_hash": content_hash,
        "char_len": len(text),
        "fetched_at": fetched_at,
    }, ensure_ascii=False, indent=2)


@mcp.tool()
def read_source(source_ref: str, max_chars: int = 12000) -> str:
    """读取已固化快照的正文。用于在授权来源内取证。

    Args:
        source_ref: open_source 返回的 source_ref。
        max_chars: 返回正文的字符上限。
    Returns:
        JSON：source_ref / source_uri / content_hash / text（含 truncated 标记）。
    """
    rec = _snapshots.get(source_ref)
    if not rec:
        snap_id = source_ref.split("/")[-1]      # 进程重启后从磁盘回补
        f = SNAP_DIR / f"{snap_id}.json"
        if f.exists():
            rec = json.loads(f.read_text(encoding="utf-8"))
            _snapshots[source_ref] = rec
        else:
            return json.dumps({"error": f"未知 source_ref={source_ref!r}，须先 open_source"},
                              ensure_ascii=False)
    text = rec["text"]
    truncated = len(text) > max_chars
    return json.dumps({
        "source_ref": source_ref,
        "source_uri": rec["url"],
        "content_hash": rec["hash"],
        "truncated": truncated,
        "text": text[:max_chars],
    }, ensure_ascii=False, indent=2)


if __name__ == "__main__":
    mcp.run()
