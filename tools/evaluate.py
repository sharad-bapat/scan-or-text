"""Score a classifier's JSON-lines output against ground truth.

usage: python tools/evaluate.py <name> <results.jsonl> <labels-real.json> <synthetic/manifest.json> [out.json]
Each results line needs: file, micros, needs_ocr, category.
Truth: real files from the PyMuPDF labeller; constructed files from how they were built.
"""
import json
import statistics
import sys
from pathlib import Path


def norm(p):
    return str(Path(p)).lower().replace("\\", "/")


def pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(q * (len(xs) - 1))))]


def main():
    name, results, real, manifest, *out = sys.argv[1:]
    truth = {}
    for r in json.loads(Path(real).read_text(encoding="utf-8")):
        if "error" not in r:
            truth[norm(r["file"])] = ("real", r["needs_ocr"], r["category"])
    for r in json.loads(Path(manifest).read_text(encoding="utf-8")):
        truth[norm(r["file"])] = ("constructed:" + r["truth"], r["needs_ocr"], r["truth"])
    rows = [json.loads(l) for l in Path(results).read_text(encoding="utf-8").splitlines() if l.strip()]
    report = {"name": name, "sets": {}, "errors": [], "misses": []}
    by_set = {}
    for row in rows:
        t = truth.get(norm(row["file"]))
        if t is None:
            continue
        if "error" in row:
            report["errors"].append(row["file"])
            continue
        by_set.setdefault(t[0], []).append((row, t))
    total = {"n": 0, "route_ok": 0, "cat_ok": 0, "fp": 0, "fn": 0, "undecided": 0, "micros": []}
    for s, items in sorted(by_set.items()):
        n = len(items)
        # "unknown" (e.g. encrypted, not decrypted) is undecided: never counted as correct
        undecided = sum(1 for row, _ in items if row["category"] == "unknown")
        items_decided = [(row, t) for row, t in items if row["category"] != "unknown"]
        route_ok = sum(1 for row, t in items_decided if bool(row["needs_ocr"]) == t[1])
        cat_ok = sum(1 for row, t in items if row["category"] == t[2])
        fp = sum(1 for row, t in items_decided if row["needs_ocr"] and not t[1])
        fn = sum(1 for row, t in items_decided if not row["needs_ocr"] and t[1])
        micros = [row["micros"] for row, _ in items]
        report["sets"][s] = {"n": n, "routing_correct": route_ok, "category_correct": cat_ok,
                             "false_ocr": fp, "missed_ocr": fn, "undecided": undecided,
                             "ms_median": round(statistics.median(micros) / 1000, 3),
                             "ms_p95": round(pct(micros, 0.95) / 1000, 3), "ms_max": round(max(micros) / 1000, 3)}
        for row, t in items:
            if bool(row["needs_ocr"]) != t[1] or row["category"] != t[2]:
                report["misses"].append({"set": s, "file": row["file"], "truth": [t[1], t[2]],
                                         "got": [row["needs_ocr"], row["category"]]})
        total["n"] += n; total["route_ok"] += route_ok; total["cat_ok"] += cat_ok
        total["fp"] += fp; total["fn"] += fn; total["undecided"] += undecided; total["micros"] += micros
    m = total.pop("micros")
    report["all"] = {**total, "ms_median": round(statistics.median(m) / 1000, 3),
                     "ms_p95": round(pct(m, 0.95) / 1000, 3), "ms_max": round(max(m) / 1000, 3)}
    print(f"== {name}: routing {total['route_ok']}/{total['n']}, category {total['cat_ok']}/{total['n']}, "
          f"false OCR {total['fp']}, missed OCR {total['fn']}, undecided {total['undecided']}, errors {len(report['errors'])}; "
          f"median {report['all']['ms_median']} ms, p95 {report['all']['ms_p95']} ms, max {report['all']['ms_max']} ms")
    for s, v in report["sets"].items():
        print(f"   {s:26s} n={v['n']:3d} routing {v['routing_correct']}/{v['n']} category {v['category_correct']}/{v['n']}"
              f"  median {v['ms_median']} ms p95 {v['ms_p95']} ms")
    for miss in report["misses"][:15]:
        print("   miss:", miss["set"], Path(miss["file"]).name[:50], "truth", miss["truth"], "got", miss["got"])
    if out:
        Path(out[0]).write_text(json.dumps(report, indent=1), encoding="utf-8")


if __name__ == "__main__":
    main()
