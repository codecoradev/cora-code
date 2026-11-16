#!/usr/bin/env python3
"""Turn source trees into "projects" for the vector-layout spike.

One project per crate directory: its functions (name + signature, what cora
embeds) and doc-comment queries whose answer is one function of the same project.

usage: make_projects.py <out_dir> <min_functions> <crate_src_dir>...
"""
import json
import os
import re
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "potion-code", "prep"))

# extract.py runs main() at import, so load its functions without executing it
src = open(os.path.join(os.path.dirname(__file__), "..", "..", "potion-code", "prep", "extract.py")).read()
src = src.rsplit("\nmain()", 1)[0]
ns = {}
exec(compile(src, "extract.py", "exec"), ns)
rust_functions, first_paragraph = ns["rust_functions"], ns["first_paragraph"]

out, min_fn, dirs = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
os.makedirs(out, exist_ok=True)
kept = total = 0
for d in dirs:
    name = os.path.basename(os.path.dirname(d.rstrip("/"))) if d.rstrip("/").endswith("src") else os.path.basename(d.rstrip("/"))
    funcs = []
    for dp, dn, fn in os.walk(d):
        dn[:] = sorted(x for x in dn if x not in ("target", "tests", "benches", "examples"))
        for f in sorted(fn):
            if f.endswith(".rs"):
                funcs.extend(rust_functions(os.path.join(dp, f)))
    funcs = [f for f in funcs if len(f["text"]) >= 20]
    if len(funcs) < min_fn:
        continue
    docs, queries = [], []
    for f in funcs:
        docs.append(f"{f['name']} {f['sig']}" if f["sig"] and f["sig"] != f["name"] else f["name"])
    for i, f in enumerate(funcs):
        if f["test"] or not f["doc"]:
            continue
        q = first_paragraph(f["doc"])
        qm = re.sub(r"\b%s\b" % re.escape(f["name"]), "", q).strip()
        if len(q.split()) >= 5 and len(qm.split()) >= 3:
            queries.append({"q": q, "rel": i})
    if len(queries) < 10:
        continue
    json.dump({"name": name, "docs": docs, "queries": queries[:60]}, open(f"{out}/{name}.json", "w"))
    kept += 1
    total += len(docs)
print(f"{kept} projects, {total} functions")
