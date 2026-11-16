#!/usr/bin/env python3
"""Keyword-style variant of a doc-comment set: each query keeps its first three
content words, like someone typing a few terms instead of a sentence.

usage: make_kw.py <data_prefix_in> <data_prefix_out>
"""
import json
import shutil
import sys

STOP = set("a an and are as at be by can for from how if in into is it its of on or that the this to was what when where which who why with will each given returns return used using use uses new".split())
src, dst = sys.argv[1], sys.argv[2]
shutil.copy(src + ".corpus.jsonl", dst + ".corpus.jsonl")
out = open(dst + ".queries.jsonl", "w")
n = 0
for line in open(src + ".queries.jsonl"):
    q = json.loads(line)
    words = [w.strip(".,:;()'\"") for w in q["q"].split()]
    words = [w for w in words if len(w) >= 3 and w.lower() not in STOP][:3]
    if len(words) < 2:
        continue
    q["q"] = " ".join(words)
    q["qm"] = q["q"]
    out.write(json.dumps(q) + "\n")
    n += 1
print(dst, n, "queries")
