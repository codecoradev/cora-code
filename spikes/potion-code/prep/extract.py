#!/usr/bin/env python3
"""Build code-search evaluation sets from source trees (offline, stdlib only).

For each function with a documentation comment, the documentation's first
paragraph is a query and the function (without its documentation) is the single
relevant document; every other function in the tree is a distractor. A second
query form has the function's own name masked, so a keyword match on the name
cannot answer it.

usage: extract.py rust|python <out_prefix> <max_queries> <src_dir>...
       extract.py hand <out_prefix> <questions.json> <src_dir>...
"""
import ast
import json
import os
import random
import re
import sys

FN_RE = re.compile(
    r'^(\s*)(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?(?:async\s+)?'
    r'(?:unsafe\s+)?(?:extern\s+"[^"]*"\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)'
)
MAX_CHARS = 4000
MIN_WORDS = 5


def walk(root, exts, skip=("target", ".git", "node_modules", "__pycache__", "test", "tests")):
    for dp, dn, fn in os.walk(root):
        dn[:] = sorted(d for d in dn if d not in skip or exts == (".py",) and d == "test" and False)
        for f in sorted(fn):
            if f.endswith(exts):
                yield os.path.join(dp, f)


def first_paragraph(lines):
    para, in_code = [], False
    for ln in lines:
        s = ln.strip()
        if s.startswith("```"):
            in_code = not in_code
            continue
        if in_code:
            continue
        if not s:
            if para:
                break
            continue
        if s.startswith("#") or s.startswith("|") or s.startswith("- ") and not para:
            if para:
                break
            continue
        para.append(s)
    text = " ".join(para).replace("`", "")
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"\s+", " ", text).strip()
    return text[:300]


def mask(text, name):
    return re.sub(r"\b%s\b" % re.escape(name), "", text).strip()


def rust_functions(path):
    try:
        lines = open(path, encoding="utf-8").read().split("\n")
    except Exception:
        return []
    out, in_tests = [], False
    for i, line in enumerate(lines):
        if re.match(r"^\s*mod\s+tests\b", line):
            in_tests = True
        m = FN_RE.match(line)
        if not m:
            continue
        indent, name = len(m.group(1)), m.group(2)
        # doc comment and attributes above
        j, attrs, doc = i - 1, [], []
        while j >= 0 and lines[j].strip().startswith("#["):
            attrs.append(lines[j].strip())
            j -= 1
        while j >= 0 and lines[j].strip().startswith("///"):
            doc.append(lines[j].strip()[3:].strip())
            j -= 1
        doc.reverse()
        # end of the function
        end = i
        if not line.rstrip().endswith(("}", ";")):
            for k in range(i + 1, min(i + 400, len(lines))):
                s = lines[k]
                if s.strip() and len(s) - len(s.lstrip()) <= indent and s.strip().startswith("}"):
                    end = k
                    break
            else:
                end = min(i + 60, len(lines) - 1)
        text = "\n".join(lines[i : end + 1])[:MAX_CHARS]
        sig_lines = []
        for k in range(i, min(i + 12, len(lines))):
            sig_lines.append(lines[k])
            if "{" in lines[k] or lines[k].rstrip().endswith(";"):
                break
        sig = re.sub(r"\s+", " ", " ".join(sig_lines).split("{")[0]).strip()[:300]
        is_test = in_tests or any("test" in a for a in attrs) or "/tests/" in path
        out.append(dict(path=path, line=i + 1, name=name, text=text, sig=sig, doc=doc, test=is_test))
    return out


def python_functions(path):
    try:
        src = open(path, encoding="utf-8").read()
        tree = ast.parse(src)
    except Exception:
        return []
    lines = src.split("\n")
    out = []
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        doc = ast.get_docstring(node) or ""
        start, end = node.lineno - 1, (node.end_lineno or node.lineno)
        body = list(lines[start:end])
        first = node.body[0]
        if doc and isinstance(first, ast.Expr) and isinstance(getattr(first, "value", None), ast.Constant):
            a, b = first.lineno - 1 - start, (first.end_lineno or first.lineno) - start
            body = body[:a] + body[b:]
        sig_lines = []
        for k in range(start, min(start + 12, len(lines))):
            sig_lines.append(lines[k].split("#")[0])
            if sig_lines[-1].rstrip().endswith(":"):
                break
        sig = re.sub(r"\s+", " ", " ".join(sig_lines)).strip()[:300]
        out.append(
            dict(path=path, line=node.lineno, name=node.name, text="\n".join(body)[:MAX_CHARS], sig=sig,
                 doc=doc.split("\n") if doc else [], test=False)
        )
    return out


def main():
    mode, prefix = sys.argv[1], sys.argv[2]
    if mode == "hand":
        qfile, dirs = sys.argv[3], sys.argv[4:]
        maxq = 0
    else:
        maxq, dirs = int(sys.argv[3]), sys.argv[4:]
    extract = python_functions if mode == "python" else rust_functions
    exts = (".py",) if mode == "python" else (".rs",)
    funcs = []
    for d in dirs:
        for p in walk(d, exts):
            funcs.extend(extract(p))
    for n, f in enumerate(funcs):
        f["id"] = n
    # drop empty/degenerate chunks from the corpus
    funcs = [f for f in funcs if len(f["text"]) >= 20]
    with open(prefix + ".corpus.jsonl", "w") as fh:
        for f in funcs:
            fh.write(json.dumps(dict(id=f["id"], path=f["path"], name=f["name"], text=f["text"], sig=f["sig"], test=f["test"])) + "\n")

    queries = []
    if mode == "hand":
        hand = json.load(open(qfile))
        index = {}
        for f in funcs:
            index.setdefault(f["name"], []).append(f)
        for item in hand:
            rel = []
            for spec in item["relevant"]:
                suffix, name = spec.split("::")
                hits = [f["id"] for f in index.get(name, []) if f["path"].endswith(suffix) and not f["test"]]
                if not hits:
                    sys.exit(f"no function {spec!r} for question {item['q']!r}")
                rel.extend(hits)
            queries.append(dict(id=len(queries), q=item["q"], qm=item["q"], rel=sorted(set(rel))))
    else:
        cand = []
        for f in funcs:
            if f["test"] or not f["doc"]:
                continue
            q = first_paragraph(f["doc"])
            if len(q.split()) < MIN_WORDS or len(f["text"]) < 80:
                continue
            qm = mask(q, f["name"])
            if len(qm.split()) < 3:
                continue
            cand.append(dict(q=q, qm=qm, rel=[f["id"]]))
        random.Random(7).shuffle(cand)
        for n, c in enumerate(cand[:maxq]):
            queries.append(dict(id=n, **c))
    with open(prefix + ".queries.jsonl", "w") as fh:
        for q in queries:
            fh.write(json.dumps(q) + "\n")
    tests = sum(1 for f in funcs if f["test"])
    print(f"{prefix}: {len(funcs)} functions ({tests} in tests), {len(queries)} queries")


main()
