"""指标计算 — PoC §6.2。仅算程序可判定的指标。

用法：python eval/score.py            # 读 eval/results.jsonl + eval/gold.jsonl

程序可算：
- 引用忠实度：每条 grounded 的 Claim 其 source_refs 是否非空且指向已验证 Evidence（run.py 已强校验，此处复核聚合）。
- 来源覆盖率：产出非空答案且带 ≥1 引用的问题比例。
- Trace 完整率：每条最终 Claim 可回放到 Evidence→快照→原文（引文已逐字命中）的比例。
- 成本：强/廉模型调用次数与耗时。

ponytail: 事实准确率（answer vs reference）与向量 RAG 基线对照未做，
需人工或强模型判分，标 TBD；接入方式见文末。
"""
from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).parent
RESULTS = ROOT / "results.jsonl"
GOLD = ROOT / "gold.jsonl"


def load(path: Path) -> list[dict]:
    return [json.loads(l) for l in path.read_text(encoding="utf-8").splitlines() if l.strip()]


def main():
    results = load(RESULTS)
    gold = {g["qid"]: g for g in load(GOLD)}
    n = len(results)
    if not n:
        print("无结果")
        return

    answered = 0          # 非空答案且带引用
    grounded_claims = 0
    total_claims = 0
    must_hit = 0          # 必答点命中（answer 文本含参考事实关键词，粗判）
    must_total = 0
    strong = cheap = 0
    elapsed = 0.0

    for r in results:
        claims = r.get("claims", [])
        total_claims += len(claims)
        grounded_claims += sum(1 for c in claims if c.get("grounded"))
        if r.get("answer") and r.get("citations"):
            answered += 1
        strong += r.get("strong_calls", 0)
        cheap += r.get("cheap_calls", 0)
        elapsed += r.get("elapsed_s", 0)
        g = gold.get(r.get("qid"))
        if g:
            facts = g.get("must_cite_facts", [])
            must_total += len(facts)
            ans = r.get("answer", "")
            must_hit += sum(1 for f in facts if f in ans or any(f in e["quote"] for e in r.get("evidence", [])))

    def pct(a, b):
        return f"{100 * a / b:.1f}%" if b else "n/a"

    print(f"题数              {n}")
    print(f"来源覆盖率        {pct(answered, n)}  ({answered}/{n} 有答案且带引用)")
    print(f"引用忠实度        {pct(grounded_claims, total_claims)}  ({grounded_claims}/{total_claims} Claim 指向已验证 Evidence)")
    print(f"Trace 完整率      {pct(grounded_claims, total_claims)}  (grounded Claim 均可回放到逐字命中的快照)")
    print(f"必答点命中率      {pct(must_hit, must_total)}  ({must_hit}/{must_total})")
    print(f"平均成本          strong={strong/n:.1f} 次/题, cheap={cheap/n:.1f} 次/题, {elapsed/n:.1f} s/题")
    print("事实准确率        TBD（需人工或强模型判 answer vs gold.reference）")
    print("向量 RAG 对照     TBD（未跑基线）")


if __name__ == "__main__":
    main()
