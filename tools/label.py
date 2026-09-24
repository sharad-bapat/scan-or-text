"""Ground truth for scan-or-text: label every PDF page by page with PyMuPDF (full parse, slow, reliable).

Per page:
  text_chars  characters of extractable text
  image_frac  fraction of the page area covered by drawn images (clipped, capped at 1)
Page label:
  TEXT         >= 40 characters of text and not under a full-page image
  SCAN_TEXT    >= 40 characters of text over an image covering >= 80% (a scan with an OCR layer)
  IMAGE        < 40 characters and images cover >= 30% (needs OCR)
  EMPTY        < 40 characters and little image (blank, cover or signature page)
Document:
  needs_ocr    any IMAGE page (the routing decision the fast classifiers must make)
  category     text | scanned | scanned_ocr | mixed

usage: python tools/label.py <out.json> <dir-or-pdf> [<dir-or-pdf> ...]
"""
import json
import sys
from pathlib import Path

import fitz  # PyMuPDF

MIN_TEXT = 40


def page_info(page):
    text_chars = len(page.get_text("text").strip())
    area = page.rect.width * page.rect.height or 1.0
    covered = 0.0
    for info in page.get_image_info():
        r = fitz.Rect(info["bbox"]) & page.rect
        covered += max(0.0, r.width) * max(0.0, r.height)
    image_frac = min(1.0, covered / area)
    if text_chars >= MIN_TEXT:
        label = "SCAN_TEXT" if image_frac >= 0.8 else "TEXT"
    else:
        label = "IMAGE" if image_frac >= 0.3 else "EMPTY"
    return {"text_chars": text_chars, "image_frac": round(image_frac, 3), "label": label}


def label_pdf(path):
    try:
        doc = fitz.open(path)
    except Exception as exc:  # unreadable files are reported, not silently dropped
        return {"file": str(path), "error": f"open: {exc}"}
    if doc.needs_pass:
        return {"file": str(path), "error": "encrypted"}
    pages = [page_info(p) for p in doc]
    labels = [p["label"] for p in pages]
    counts = {k: labels.count(k) for k in ("TEXT", "SCAN_TEXT", "IMAGE", "EMPTY")}
    needs_ocr = counts["IMAGE"] > 0
    if needs_ocr and (counts["TEXT"] or counts["SCAN_TEXT"]):
        category = "mixed"
    elif needs_ocr:
        category = "scanned"
    elif counts["SCAN_TEXT"] > counts["TEXT"]:
        category = "scanned_ocr"
    else:
        category = "text"
    return {"file": str(path), "pages": len(pages), "counts": counts, "needs_ocr": needs_ocr,
            "category": category, "per_page": pages}


def main():
    out, *inputs = sys.argv[1:]
    files = []
    for arg in inputs:
        p = Path(arg)
        files += sorted(p.rglob("*.pdf")) if p.is_dir() else [p]
    rows = []
    for i, f in enumerate(files, 1):
        rows.append(label_pdf(f))
        if i % 50 == 0:
            print(f"  labelled {i}/{len(files)}", file=sys.stderr)
    Path(out).write_text(json.dumps(rows, indent=1), encoding="utf-8")
    ok = [r for r in rows if "error" not in r]
    cats = {}
    for r in ok:
        cats[r["category"]] = cats.get(r["category"], 0) + 1
    print(f"{len(rows)} files, {len(rows) - len(ok)} unreadable; categories {cats}; "
          f"needs_ocr {sum(r['needs_ocr'] for r in ok)}")


if __name__ == "__main__":
    main()
