"""单进程受控环路 — PoC 阶段 1（validation-poc.md §5）。

10 步环路：strong 生成搜索词→选候选→open 固化→cheap 逐字取证→程序强校验→
strong 生成 Claim+带引用答案→校验 Claim 均指向已验证 Evidence→输出+trace。

关键：所有校验由程序执行（candidate_id 属本 run、hash 匹配、引文逐字命中、
Claim 指向已验证 Evidence）；模型只产出结构化候选。三工具进程内直接 import，不走 stdio。

用法：
    python run.py --question "问题"
    python run.py --gold eval/gold.jsonl        # 批量，结果写 eval/results.jsonl

ponytail: reasoner 不支持 json mode/temperature，故统一 prompt+鲁棒解析；
未做向量 RAG 基线对照与 strong 判分事实准确率（score.py 里标注）。
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sqlite3
import sys
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

import openai
from dotenv import load_dotenv

sys.path.insert(0, str(Path(__file__).parent / "search_mcp"))
import server  # noqa: E402  三工具纯函数

ROOT = Path(__file__).parent
load_dotenv(ROOT / ".env")

TRACE_DIR = ROOT / "trace"
TRACE_DIR.mkdir(exist_ok=True)
DB_PATH = ROOT / "store.sqlite"

STRONG = os.environ["MODEL_STRONG"]
CHEAP = os.environ["MODEL_CHEAP"]
_client = openai.OpenAI(base_url=os.environ["DEEPSEEK_BASE_URL"],
                        api_key=os.environ["DEEPSEEK_API_KEY"])

MAX_QUERIES = 3        # 有界搜索词
MAX_CANDIDATES = 5     # 每词候选上限
MAX_OPEN = 4           # 全 run 最多固化快照数，控成本


# ---------- 基础设施 ----------
def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _db() -> sqlite3.Connection:
    con = sqlite3.connect(DB_PATH)
    con.executescript("""
    CREATE TABLE IF NOT EXISTS run(
        run_id TEXT PRIMARY KEY, question TEXT, answer TEXT,
        created_at TEXT, strong_calls INT, cheap_calls INT);
    CREATE TABLE IF NOT EXISTS evidence(
        run_id TEXT, source_ref TEXT, content_hash TEXT, quote TEXT, verified INT);
    CREATE TABLE IF NOT EXISTS claim(
        run_id TEXT, text TEXT, source_refs TEXT, grounded INT);
    """)
    return con


class Loop:
    """一次 run 的状态与 trace。模型无状态，所有输入由持久对象重建。"""

    def __init__(self, question: str):
        self.run_id = uuid.uuid4().hex[:12]
        self.question = question
        self.trace_fp = (TRACE_DIR / f"{self.run_id}.jsonl").open("w", encoding="utf-8")
        self.strong_calls = 0
        self.cheap_calls = 0

    def log(self, step: str, **data):
        rec = {"ts": _now(), "run_id": self.run_id, "step": step, **data}
        self.trace_fp.write(json.dumps(rec, ensure_ascii=False) + "\n")
        self.trace_fp.flush()

    def chat(self, model: str, system: str, user: str) -> str:
        kw = {"model": model,
              "messages": [{"role": "system", "content": system},
                           {"role": "user", "content": user}]}
        if model == CHEAP:
            kw["temperature"] = 0          # reasoner 不接受 temperature
        r = _client.chat.completions.create(**kw)
        if model == STRONG:
            self.strong_calls += 1
        else:
            self.cheap_calls += 1
        return r.choices[0].message.content or ""

    def close(self):
        self.trace_fp.close()


def _extract_json(text: str):
    """鲁棒提取模型输出中的 JSON（兼容 ```json 包裹与裸对象/数组）。"""
    m = re.search(r"```(?:json)?\s*(.+?)```", text, re.S)
    blob = m.group(1) if m else text
    m = re.search(r"[\[{].*[\]}]", blob, re.S)
    if not m:
        raise ValueError(f"未找到 JSON: {text[:200]}")
    return json.loads(m.group(0))


def _tool(fn, **kw) -> dict:
    """调用 server 工具（返回 JSON 字符串）并解析为 dict。"""
    return json.loads(fn(**kw))


# ---------- 10 步环路 ----------
def run_once(question: str) -> dict:
    lp = Loop(question)
    lp.log("question", text=question)

    # 2. strong 生成有界搜索词
    raw = lp.chat(STRONG,
                  "你是检索规划器。只输出 JSON 数组，元素为搜索词字符串，"
                  f"最多 {MAX_QUERIES} 个，覆盖问题的关键事实点。不要解释。",
                  question)
    queries = _extract_json(raw)[:MAX_QUERIES]
    lp.log("queries", queries=queries)

    # 3-4. 每词搜索 → strong 选候选
    seen_cid: dict[str, dict] = {}
    for q in queries:
        res = _tool(server.search_candidates, query=q, k=MAX_CANDIDATES)
        for c in res.get("candidates", []):
            seen_cid[c["candidate_id"]] = c
    lp.log("candidates", count=len(seen_cid), items=list(seen_cid.values()))
    if not seen_cid:
        lp.log("abort", reason="无候选（搜索为空或被反爬）")
        lp.close()
        return {"run_id": lp.run_id, "question": question,
                "answer": "检索未返回任何候选来源，无法作答。", "citations": [],
                "evidence": [], "strong_calls": lp.strong_calls, "cheap_calls": lp.cheap_calls}

    cand_menu = [{"candidate_id": k, "title": v["title"], "snippet": v["snippet"]}
                 for k, v in seen_cid.items()]
    raw = lp.chat(STRONG,
                  "从候选中选出最可能含权威事实依据的网页。只输出 JSON 数组，"
                  f"元素为 candidate_id 字符串，最多 {MAX_OPEN} 个。只能从给定 id 中选，不得自造。",
                  json.dumps(cand_menu, ensure_ascii=False))
    picked = [cid for cid in _extract_json(raw) if cid in seen_cid][:MAX_OPEN]
    lp.log("picked", picked=picked)

    # 5. open 固化快照
    opened: list[dict] = []
    for cid in picked:
        res = _tool(server.open_source, candidate_id=cid)
        if "error" in res:
            lp.log("open_skip", candidate_id=cid, reason=res["error"])  # 遇反爬/登录墙即弃
            continue
        opened.append(res)
        lp.log("opened", source_ref=res["source_ref"], uri=res["source_uri"],
               content_hash=res["content_hash"])
    if not opened:
        lp.log("abort", reason="所有候选固化失败")
        lp.close()
        return {"run_id": lp.run_id, "question": question,
                "answer": "候选来源均无法固化快照（动态渲染/登录墙/反爬），无法作答。",
                "citations": [], "evidence": [],
                "strong_calls": lp.strong_calls, "cheap_calls": lp.cheap_calls}

    # 6. cheap 只读快照取证 → 7. 程序强校验
    verified: list[dict] = []
    for o in opened:
        sref = o["source_ref"]
        snap = _tool(server.read_source, source_ref=sref)
        text = snap["text"]
        raw = lp.chat(CHEAP,
                      "你是取证助手。只依据给定网页正文，摘出能回答问题的逐字引文片段。"
                      "只输出 JSON 数组，每元素 {\"quote\": \"原文逐字片段\"}；"
                      "quote 必须是正文中连续出现的原文，不得改写、拼接或翻译。无相关内容则输出 []。",
                      f"问题：{question}\n\n网页正文：\n{text}")
        try:
            cands = _extract_json(raw)
        except ValueError:
            cands = []
        for e in cands:
            quote = (e.get("quote") or "").strip()
            # 强校验：source_ref 存在 + hash 匹配 + 引文逐字命中原文
            hit = bool(quote) and quote in text and snap["content_hash"] == o["content_hash"]
            if hit and quote not in [v["quote"] for v in verified]:  # 去重
                verified.append({"source_ref": sref, "content_hash": o["content_hash"],
                                 "quote": quote})
            lp.log("evidence_check", source_ref=sref, verified=hit,
                   quote=quote[:120])
    lp.log("verified_evidence", count=len(verified))

    if not verified:
        lp.log("abort", reason="无逐字命中的证据")
        lp.close()
        return {"run_id": lp.run_id, "question": question,
                "answer": "未能从来源中取得逐字可验证的证据，不作答以免臆测。",
                "citations": [], "evidence": [],
                "strong_calls": lp.strong_calls, "cheap_calls": lp.cheap_calls}

    # 8. strong 读已验证证据 → Claim + 带引用答案
    ev_menu = [{"id": i, "source_ref": v["source_ref"], "quote": v["quote"]}
               for i, v in enumerate(verified)]
    raw = lp.chat(STRONG,
                  "仅依据给定证据回答问题，不得引入证据外的事实。输出 JSON 对象："
                  '{"answer": "综合回答", "claims": [{"text": "事实句", "evidence_ids": [证据id]}]}。'
                  "每条事实句必须由至少一个 evidence_id 支撑；不要编造 id。",
                  f"问题：{question}\n\n证据：\n{json.dumps(ev_menu, ensure_ascii=False, indent=2)}")
    out = _extract_json(raw)
    answer = out.get("answer", "")
    claims = out.get("claims", [])

    # 9. 校验每条 Claim 指向已验证 Evidence
    valid_ids = set(range(len(verified)))
    checked_claims = []
    for c in claims:
        ids = [i for i in c.get("evidence_ids", []) if i in valid_ids]
        grounded = len(ids) > 0
        checked_claims.append({"text": c.get("text", ""),
                               "source_refs": [verified[i]["source_ref"] for i in ids],
                               "grounded": grounded})
        lp.log("claim_check", text=c.get("text", "")[:120], grounded=grounded)

    citations = sorted({v["source_ref"] for v in verified})
    # 10. 输出 + 落库
    lp.log("answer", answer=answer, citations=citations)
    with _db() as con:
        con.execute("INSERT INTO run VALUES(?,?,?,?,?,?)",
                    (lp.run_id, question, answer, _now(), lp.strong_calls, lp.cheap_calls))
        con.executemany("INSERT INTO evidence VALUES(?,?,?,?,?)",
                        [(lp.run_id, v["source_ref"], v["content_hash"], v["quote"], 1)
                         for v in verified])
        con.executemany("INSERT INTO claim VALUES(?,?,?,?)",
                        [(lp.run_id, c["text"], json.dumps(c["source_refs"], ensure_ascii=False),
                          int(c["grounded"])) for c in checked_claims])
    lp.close()
    return {"run_id": lp.run_id, "question": question, "answer": answer,
            "citations": citations, "claims": checked_claims,
            "evidence": verified, "strong_calls": lp.strong_calls,
            "cheap_calls": lp.cheap_calls}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--question")
    ap.add_argument("--gold", help="gold.jsonl 路径，批量跑")
    args = ap.parse_args()

    if args.gold:
        gp = Path(args.gold)
        out_fp = (ROOT / "eval" / "results.jsonl").open("w", encoding="utf-8")
        for line in gp.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line:
                continue
            item = json.loads(line)
            t0 = time.time()
            r = run_once(item["question"])
            r["qid"] = item.get("qid")
            r["elapsed_s"] = round(time.time() - t0, 1)
            out_fp.write(json.dumps(r, ensure_ascii=False) + "\n")
            out_fp.flush()
            print(f"[{r['qid']}] {r['run_id']} claims={len(r.get('claims', []))} "
                  f"cites={len(r['citations'])} {r['elapsed_s']}s")
        out_fp.close()
        return

    q = args.question or "劳动合同被用人单位违法解除，劳动者可以主张哪些救济？"
    r = run_once(q)
    print(json.dumps(r, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
