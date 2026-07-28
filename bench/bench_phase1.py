#!/usr/bin/env python3
"""Quick benchmark: cora index --rebuild vs cbm index_repository."""
import subprocess, time, statistics, os

CORA_BIN = "/opt/data/repos/cora-code/target/release/cora"
CBM_BIN = "/opt/data/bin/cbm"

REPOS = [
    ("/opt/data/repos/cora-code", "Rust (cora-code, ~8K LOC)"),
    ("/opt/data/repos/glm-proxy-golang", "Go (glm-proxy, ~2K LOC)"),
]

WARMUP = 1
RUNS = 5
DB_DIR = "/opt/data/.codecora/cora-code"
CBM_DB = "/opt/data/.cbm"

def clean_cora():
    if not os.path.isdir(DB_DIR):
        os.makedirs(DB_DIR, exist_ok=True)
    for f in os.listdir(DB_DIR):
        p = os.path.join(DB_DIR, f)
        if os.path.isfile(p):
            os.remove(p)

def clean_cbm():
    if not os.path.isdir(CBM_DB):
        os.makedirs(CBM_DB, exist_ok=True)
    for f in os.listdir(CBM_DB):
        p = os.path.join(CBM_DB, f)
        if os.path.isfile(p):
            os.remove(p)

def bench_cora(repo):
    clean_cora()
    times = []
    for i in range(WARMUP + RUNS):
        t0 = time.perf_counter()
        r = subprocess.run(
            [CORA_BIN, "index", "--rebuild"],
            capture_output=True, text=True, timeout=120,
            cwd=repo,
        )
        elapsed = time.perf_counter() - t0
        if i >= WARMUP:
            times.append(elapsed)
        if r.returncode != 0 and i == WARMUP:
            print(f"  ⚠ Cora stderr: {r.stderr[:200]}")
    return times

def bench_cbm(repo):
    clean_cbm()
    times = []
    for i in range(WARMUP + RUNS):
        t0 = time.perf_counter()
        r = subprocess.run(
            [CBM_BIN, "cli", "index_repository", f"--repo-path={repo}"],
            capture_output=True, text=True, timeout=120,
        )
        elapsed = time.perf_counter() - t0
        if i >= WARMUP:
            times.append(elapsed)
        if r.returncode != 0 and i == WARMUP:
            print(f"  ⚠ CBM stderr: {r.stderr[:200]}")
    return times

print("=" * 65)
print("BENCHMARK: Phase 1 Quick Wins")
print("  - SQLite PRAGMA tuning (WAL, sync=NORMAL, cache=64MB)")
print("  - Single transaction per project (was per-file)")
print("  - FTS5 triggers disabled during bulk insert")
print("  - Tree-sitter parsed once per file (was 3x)")
print("  - Prepared statements for batch INSERT")
print("=" * 65)

for repo, label in REPOS:
    print(f"\n📁 {label}")
    print(f"   Path: {repo}")
    
    cora_times = bench_cora(repo)
    cbm_times = bench_cbm(repo)
    
    c_mean = statistics.mean(cora_times)
    c_std = statistics.stdev(cora_times) if len(cora_times) > 1 else 0
    cb_mean = statistics.mean(cbm_times)
    cb_std = statistics.stdev(cbm_times) if len(cbm_times) > 1 else 0
    ratio = c_mean / cb_mean if cb_mean > 0 else float('inf')
    
    print(f"   Cora:  {c_mean*1000:.0f}ms ± {c_std*1000:.0f}ms  ({[f'{t*1000:.0f}ms' for t in cora_times]})")
    print(f"   CBM:   {cb_mean*1000:.0f}ms ± {cb_std*1000:.0f}ms  ({[f'{t*1000:.0f}ms' for t in cbm_times]})")
    print(f"   Ratio: {ratio:.1f}x slower")
