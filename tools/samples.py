"""Three small, neutral sample PDFs for the in-browser demo (no third-party documents).

  text.pdf     two pages of digital text
  scanned.pdf  the same two pages, rasterised (image only; needs OCR)
  mixed.pdf    page 1 digital, page 2 a scanned signature page (needs OCR on page 2)

usage: python tools/samples.py <outdir>
"""
import sys
from pathlib import Path

import fitz
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen import canvas

PARAS = [
    "Sample mutual confidentiality agreement.",
    "1. Each party may share information with the other for the purpose of evaluating a possible project.",
    "2. The receiving party will keep the information confidential and use it only for that purpose.",
    "3. Information that is public, already known, or independently developed is not confidential.",
    "4. On request, the receiving party will return or destroy the information it holds.",
    "5. These obligations continue for two years after the last disclosure.",
    "This is a made-up document for testing whether a PDF needs OCR. It is not a real agreement.",
]


def text_pdf(path):
    c = canvas.Canvas(str(path), pagesize=A4)
    for page in (1, 2):
        y = 780
        c.setFont("Helvetica-Bold", 14)
        c.drawString(60, y, f"Sample agreement, page {page}")
        c.setFont("Helvetica", 11)
        for p in PARAS * 2:
            y -= 26
            c.drawString(60, y, p[:95])
        if page == 2:
            c.drawString(60, 120, "Signed: ____________________      Date: ____________")
        c.showPage()
    c.save()


def raster(src_page):
    return src_page.get_pixmap(dpi=100, colorspace=fitz.csGRAY).tobytes("jpg")


def main():
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    text_pdf(out / "text.pdf")
    with fitz.open(out / "text.pdf") as src:
        scan = fitz.open()
        for page in src:
            p = scan.new_page(width=page.rect.width, height=page.rect.height)
            p.insert_image(p.rect, stream=raster(page))
        scan.save(out / "scanned.pdf", deflate=True, garbage=4)
        mixed = fitz.open()
        mixed.insert_pdf(src, from_page=0, to_page=0)
        p = mixed.new_page(width=src[1].rect.width, height=src[1].rect.height)
        p.insert_image(p.rect, stream=raster(src[1]))
        mixed.save(out / "mixed.pdf", deflate=True, garbage=4)
    for f in ("text.pdf", "scanned.pdf", "mixed.pdf"):
        print(f, (out / f).stat().st_size // 1024, "KB")


if __name__ == "__main__":
    main()
