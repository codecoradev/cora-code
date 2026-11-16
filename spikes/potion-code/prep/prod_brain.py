#!/usr/bin/env python3
"""Run the real `cora brain` on an evaluation set and score it the same way.

usage: prod_brain.py <cora_binary> <data_prefix> <project_root>
The project must already be indexed (`cora index`). Targets are matched by
function name and file path.
"""
import json
import os
import subprocess
import sys

cora, prefix, root = sys.argv[1], sys.argv[2], sys.argv[3]
corpus = {}
for line in open(prefix + ".corpus.jsonl"):
    d = json.loads(line)
    corpus[d["id"]] = (d["name"], os.path.relpath(d["path"], root))
rr, r1, r5, r10 = [], 0, 0, 0
queries = [json.loads(l) for l in open(prefix + ".queries.jsonl")]
for q in queries:
    want = {corpus[i] for i in q["rel"] if i in corpus}
    out = subprocess.run([cora, "brain", q["q"], "--limit", "10", "--json"], cwd=root, capture_output=True, text=True)
    try:
        results = json.loads(out.stdout)
    except Exception:
        results = []
    rank = None
    for pos, r in enumerate(results, 1):
        if (r["name"], r["file"]) in want:
            rank = pos
            break
    rr.append(1 / rank if rank else 0.0)
    r1 += rank == 1
    r5 += bool(rank and rank <= 5)
    r10 += bool(rank)
n = len(queries)
print(json.dumps(dict(n=n, mrr10=sum(rr) / n, r1=r1 / n, r5=r5 / n, r10=r10 / n)))
