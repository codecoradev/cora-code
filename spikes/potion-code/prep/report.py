#!/usr/bin/env python3
"""Aggregate eval results into Markdown tables with bootstrap confidence intervals.

usage: report.py <results_dir>
"""
import glob
import json
import os
import random
import sys

DATASETS = [
    ("cora_hand", "cora-code, 39 hand-written questions"),
    ("cora_doc", "cora-code, 400 doc-comment queries"),
    ("crates_doc", "8 Rust crates, 600 doc-comment queries"),
    ("py_doc", "Python stdlib, 600 docstring queries"),
    ("cora_doc_kw", "cora-code, 400 keyword-style queries (3 content words)"),
    ("crates_doc_kw", "8 Rust crates, keyword-style queries (3 content words)"),
    ("py_doc_kw", "Python stdlib, keyword-style queries (3 content words)"),
]
BACKENDS = ["bm25", "hashing", "nomic", "potion-code-v2", "potion-code-v2-int8", "potion-code-v1", "potion-base-8M", "potion-base-32M"]
rng = random.Random(11)


def boot_ci(x, n=1000):
    m = len(x)
    means = sorted(sum(x[rng.randrange(m)] for _ in range(m)) / m for _ in range(n))
    return means[int(0.025 * n)], means[int(0.975 * n)]


def paired(a, b, n=1000):
    """95% interval of mean(a - b) over paired queries."""
    d = [x - y for x, y in zip(a, b)]
    m = len(d)
    means = sorted(sum(d[rng.randrange(m)] for _ in range(m)) / m for _ in range(n))
    return sum(d) / m, means[int(0.025 * n)], means[int(0.975 * n)]


def load(res, ds, rep):
    rows, info = {}, {}
    for b in BACKENDS:
        p = f"{res}/{ds}.{rep}.{b}.jsonl"
        if not os.path.exists(p):
            continue
        for line in open(p):
            j = json.loads(line)
            if "method" in j:
                rows[(j["method"], j["form"])] = j
            else:
                info[b] = j
    return rows, info


def main():
    res = sys.argv[1]
    for ds, title in DATASETS:
        for rep in ("sig", "body"):
            rows, info = load(res, ds, rep)
            any_info = next(iter(info.values()))
            what = "name + signature only (what cora indexes today)" if rep == "sig" else "the whole function body"
            print(f"\n### {title}: documents are {what}")
            print(f"\n{any_info['docs']} functions searched, {any_info['queries']} queries\n")
            forms = ["q"] if ds == "cora_hand" or ds.endswith("_kw") else ["q", "qm"]
            for form in forms:
                label = "query as written" if form == "q" else "function name masked in the query"
                print(f"**{label}**\n")
                print("| Method | MRR@10 (95% CI) | R@1 | R@5 | R@10 | NDCG@10 |")
                print("|---|---|---|---|---|---|")
                for b in BACKENDS:
                    for m in (b, f"bm25+{b}@0.25", f"bm25+{b}@0.5", f"bm25+{b}@1", f"bm25+{b}@2"):
                        if (m, form) in rows and b != "bm25" or (m == "bm25" and (m, form) in rows):
                            j = rows[(m, form)]
                            lo, hi = boot_ci(j["rr"])
                            print(f"| {m} | {j['mrr10']:.3f} ({lo:.3f}-{hi:.3f}) | {j['r1']:.3f} | {j['r5']:.3f} | {j['r10']:.3f} | {j['ndcg10']:.3f} |")
                print()
            base = rows[("bm25", "q")]["rr"]
            print("Paired difference in MRR@10 against bm25 alone (query as written):\n")
            print("| Method | mean diff | 95% interval |")
            print("|---|---|---|")
            for m in ["potion-code-v2", "bm25+potion-code-v2@1", "bm25+potion-code-v2@0.5", "bm25+hashing@1", "bm25+hashing@0.5", "bm25+nomic@0.5"]:
                if (m, "q") in rows:
                    d, lo, hi = paired(rows[(m, "q")]["rr"], base)
                    print(f"| {m} | {d:+.3f} | {lo:+.3f} to {hi:+.3f} |")
            print()
    print("\n### Cost per backend (8 Rust crates, 5,028 functions, one process per run)\n")
    print("| Documents | Backend | dim | load ms | docs/s | query ms | RSS loaded MB | RSS after MB | peak MB | vectors MB |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for rep in ("sig", "body"):
        _, info = load(res, "crates_doc", rep)
        for b in BACKENDS[1:]:
            i = info.get(b)
            if i and "dim" in i:
                print(f"| {rep} | {b} | {i['dim']} | {i['load_ms']:.0f} | {i['docs_per_s']:.0f} | {i['query_ms']:.3f} | {i['rss_loaded_mb']:.0f} | {i['rss_after_mb']:.0f} | {i['peak_rss_mb']:.0f} | {i['index_vectors_mb']:.1f} |")


main()
