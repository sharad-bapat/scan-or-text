# scan-or-text

Does this PDF need OCR? A minimal structural classifier in Rust (also compiled to WebAssembly). It reads just enough of the file to say, page by page, whether there's text or only an image. It never renders a page.

## Layout

- `classifier/`: the library and CLI.
  - `src/lib.rs`: content-stream interpreter and the page labels.
  - The PDF reading (object index, page tree, filters, empty-password decryption with RC4 and AES-128) is [pdf-core](https://github.com/sharad-bapat/pdf-core), shared with wordbox and where-are-the-regions. Clone it next to this repo: classifier/Cargo.toml gives it by path.
- `bench/`: head-to-head with [pdf-inspector](https://github.com/firecrawl/pdf-inspector) (`classify_pdf_mem`, default `Sample(8)`), with both run in the same process on the same bytes.
- `tools/label.py`: ground truth, a full PyMuPDF parse of every page (text length plus image coverage).
- `tools/synth.py`: constructed test PDFs, with labels known by construction:
  - scanned: half JPEG, half lossless;
  - mixed: the last page scanned;
  - Tesseract-OCR'd scans.
- `tools/evaluate.py`: scoring. An "unknown" answer counts as undecided, never as correct.
- `tools/wasm-bench.mjs`: WebAssembly timing in Node (V8).
- `tools/samples.py`: three neutral sample PDFs for the demo.

## Data

The real files are ContractNLI's 375 source PDFs (Koreeda & Manning 2021, CC BY 4.0), read from their download folder and not committed. 370 are text, 2 are scans with an OCR layer, and 3 are mixed (scanned signature pages). 18 are encrypted with an empty user password.

The 150 constructed files come from 60 randomly chosen real NDAs (seed 20260925, first 3 pages of each): 60 scanned, 60 mixed and 30 OCR'd scans. The independent labeller agrees with the construction on all 150.

## Results

Rerun on 27 September 2026, after the parser fixes in version 4, on the same Windows laptop. Absolute times vary about two times between runs, so compare within a run.

| | Routing correct (needs OCR or not) | Wrong | Median | p95 | Max |
|---|---|---|---|---|---|
| scan-or-text, native | 525 / 525 | 0 | 0.68 ms | 1.84 ms | 7.1 ms |
| pdf-inspector 1.24.0 (same run) | 491 / 525 | 34 (all false "needs OCR") | 1.40 ms | 4.45 ms | 34.5 ms |

Run back to back twice, native was 0.67 and 0.66 ms median and 1.9 ms p95, and WebAssembly 0.87 ms median and 2.3 to 2.4 ms p95. All four runs got 525 / 525. The WebAssembly file is 115 KB. In the first run, on 25 September, native was 1.34 ms median against pdf-inspector's 3.06 ms, and 0.90 ms back to back against 1.16 ms for WebAssembly. PyMuPDF's full parse (the ground truth, in Python) took 14.4 ms median, 40 ms p95 and 703 ms max in that run; I didn't rerun it.

Of pdf-inspector's 34 misses, 32 are the 30 constructed OCR'd scans plus 2 real ones, which it treats as scanned, so they'd get OCR'd again. That's a difference in definition. The other 2 are digital documents it flagged as scanned, which are real errors.

## Versions

1. No decryption (miniz_oxide only). Routing was 507 correct and 18 undecided: exactly the 18 encrypted files.
2. RC4 and AES-128 decryption added, for an empty user password, revisions 2 to 4. The first AES attempt failed because the stream slice kept the end-of-line before `endstream`, so the data wasn't a multiple of 16 bytes. Decrypting whole blocks only fixed it.
3. ASCII85 and ASCIIHex filters added, which one encrypted file needed. That gave 525 / 525.
4. Four parser fixes ported from wordbox: /Contents given as a reference to an array object, the LZW and RunLength filters, a stream search that ran to the end of the file for every object, and object streams read in HashMap order, so which copy of a duplicated object won could change between runs (now the copy later in the file wins). Routing stayed at 525 / 525. The first fix changed 5 pages in 4 real files from empty to text, which is what the labeller says they are.
5. Inline images with ASCII85 or ASCIIHex data are now skipped up to the data's own end marker, ported from where-are-the-regions. The data can contain "EI" itself: on govdocs1 003961 a line of it starts "EI(", so the rest of the page, 8 more image strips among it, was read as one string. None of the 525 files changes. 003961, not in the set, moves from text to scanned_ocr, as the labeller says: its map labels are real text over images that cover 97% of the page. The WebAssembly file is now 116 KB.

6. The parser moved to the shared pdf-core crate (6 October 2026). The labels are the same before and after on all 1,791 files checked: the 525 routing files, the 153 constructed and sample files, and the 1,113 test files of where-are-the-regions. pdf-core walks up to 2,000 pages, so this tool now cuts its own list at 500. The WebAssembly file is 124,032 bytes, and its labels match the native ones on all 525 routing files.

## Limits

AES-256 (revisions 5 and 6) and non-empty passwords aren't supported; they return `unknown`. Character counts are rough: a two-byte CID font counts double, and the thresholds (40 characters; image coverage of 30% and 80%) mirror the labeller's. The constructed scans are clean, with no skew, noise, tiled strips or JBIG2/CCITT images, and only 3 real documents need OCR.

## Commands

```
cargo build --release --manifest-path classifier/Cargo.toml
python tools/label.py data/labels-real.json <contract-nli raw dir>
python tools/synth.py data/labels-real.json data/synthetic 60 30
cargo build --release --manifest-path bench/Cargo.toml
bench/target/release/bench data/files.txt results/bench-ours.jsonl results/bench-inspector.jsonl 9
python tools/evaluate.py <name> <results.jsonl> data/labels-real.json data/synthetic/manifest.json
(cd classifier && wasm-pack build --release --target web --out-dir ../wasm-pkg -- --features wasm)
node tools/wasm-bench.mjs data/files.txt results/wasm.jsonl 9
```
