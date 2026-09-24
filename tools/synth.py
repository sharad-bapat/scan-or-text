"""Constructed test PDFs with labels known by construction, from real digital NDAs.

  scan/   every page rasterised, rebuilt as an image-only PDF (needs OCR)
          even-numbered sources store JPEG (like most scanners), odd store lossless images
  ocr/    the scan run through Tesseract: page image + invisible text layer (does NOT need OCR)
  mixed/  the original digital pages with the last page swapped for its scan (needs OCR)

usage: python tools/synth.py <labels-real.json> <outdir> [count=60] [ocr_count=30]
"""
import io
import json
import random
import subprocess
import sys
import tempfile
from pathlib import Path

import fitz

DPI = 110
MAX_PAGES = 3


def raster_page(src_page, jpeg):
    pix = src_page.get_pixmap(dpi=DPI, colorspace=fitz.csGRAY)
    return pix.tobytes("jpg" if jpeg else "png"), pix


def scan_doc(src, jpeg):
    out = fitz.open()
    for page in list(src)[:MAX_PAGES]:
        img, _ = raster_page(page, jpeg)
        p = out.new_page(width=page.rect.width, height=page.rect.height)
        p.insert_image(p.rect, stream=img)
    return out


def ocr_doc(src):
    out = fitz.open()
    with tempfile.TemporaryDirectory() as tmp:
        for i, page in enumerate(list(src)[:MAX_PAGES]):
            _, pix = raster_page(page, jpeg=False)
            png = Path(tmp) / f"p{i}.png"
            pix.save(png)
            base = Path(tmp) / f"p{i}"
            subprocess.run(["tesseract", str(png), str(base), "-l", "eng", "pdf"], check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            with fitz.open(str(base) + ".pdf") as one:
                out.insert_pdf(one)
    return out


def mixed_doc(src, jpeg):
    n = min(len(src), MAX_PAGES)
    out = fitz.open()
    out.insert_pdf(src, from_page=0, to_page=n - 2) if n > 1 else None
    last = src[n - 1]
    img, _ = raster_page(last, jpeg)
    p = out.new_page(width=last.rect.width, height=last.rect.height)
    p.insert_image(p.rect, stream=img)
    return out


def main():
    labels_path, outdir, *rest = sys.argv[1:]
    count = int(rest[0]) if rest else 60
    ocr_count = int(rest[1]) if len(rest) > 1 else 30
    rows = json.loads(Path(labels_path).read_text(encoding="utf-8"))
    pool = sorted(r["file"] for r in rows if r.get("category") == "text" and r["pages"] >= 2)
    rng = random.Random(20260925)
    chosen = rng.sample(pool, count)
    out = Path(outdir)
    for sub in ("scan", "ocr", "mixed"):
        (out / sub).mkdir(parents=True, exist_ok=True)
    manifest = []
    for i, f in enumerate(chosen):
        name = f"s{i:03d}.pdf"
        jpeg = i % 2 == 0
        with fitz.open(f) as src:
            scan_doc(src, jpeg).save(out / "scan" / name, deflate=True)
            manifest.append({"file": str(out / "scan" / name), "source": f, "truth": "scanned", "needs_ocr": True, "jpeg": jpeg})
            mixed_doc(src, jpeg).save(out / "mixed" / name, deflate=True)
            manifest.append({"file": str(out / "mixed" / name), "source": f, "truth": "mixed", "needs_ocr": True, "jpeg": jpeg})
            if i < ocr_count:
                ocr_doc(src).save(out / "ocr" / name, deflate=True)
                manifest.append({"file": str(out / "ocr" / name), "source": f, "truth": "scanned_ocr", "needs_ocr": False, "jpeg": False})
        print(f"  {i + 1}/{count}", file=sys.stderr) if (i + 1) % 10 == 0 else None
    (out / "manifest.json").write_text(json.dumps(manifest, indent=1), encoding="utf-8")
    print(f"{len(manifest)} constructed PDFs")


if __name__ == "__main__":
    main()
