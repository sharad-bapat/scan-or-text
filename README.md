# scan-or-text

Does this PDF need OCR? A minimal structural classifier in Rust (also compiled to WebAssembly). It reads just enough of the file to say, page by page, whether there's text or only an image. It never renders a page.

## Layout

- `classifier/`: the library and CLI.
  - `src/lib.rs`: object index, page tree, content-stream interpreter.
  - `src/crypt.rs`: empty-password decryption, RC4 and AES-128.
  - Dependencies: `miniz_oxide`, `md-5`, `aes`.
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

- **375 real NDAs:** ContractNLI's source PDFs (Koreeda & Manning 2021, CC BY 4.0). They're read from their download folder, not committed.
  - 370 are text, 2 are scans with an OCR layer, and 3 are mixed (scanned signature pages). 18 are encrypted with an empty user password.
- **150 constructed files** from 60 randomly chosen real NDAs (seed 20260925, first 3 pages of each):
  - 60 scanned
  - 60 mixed
  - 30 OCR'd scans

  The independent labeller agrees with the construction on all 150.

## Results (2026-09-25, a Windows laptop; absolute times vary about 2× between runs, so compare within a run)

| | Routing correct (needs OCR or not) | Wrong | Median | p95 | Max |
|---|---|---|---|---|---|
| scan-or-text, native | 525 / 525 | 0 | 1.34 ms | 4.09 ms | 14.0 ms |
| pdf-inspector 1.24.0 (same run) | 491 / 525 | 34 (all false "needs OCR") | 3.06 ms | 9.49 ms | 66.7 ms |

Native against WebAssembly, run back to back twice:
- Native: 0.90 ms median, 2.6 ms p95.
- WebAssembly: 1.16 ms median, 3.6 ms p95.
- Both are 525 / 525.

PyMuPDF's full parse (the ground truth, in Python) takes 14.4 ms median, 40 ms p95 and 703 ms max.

pdf-inspector's 34 misses break down as:
- the 30 constructed OCR'd scans plus 2 real ones, which it treats as scanned, so they get OCR'd again (a difference in definition);
- 2 digital documents flagged as scanned (genuine errors).

## Versions

1. **No decryption (miniz_oxide only).** Routing was 507 correct and 18 undecided: exactly the 18 encrypted files.
2. **Adding RC4 and AES-128 decryption,** empty user password, revisions 2 to 4. The first AES attempt failed because the stream slice kept the end-of-line before `endstream`, so the data wasn't a multiple of 16 bytes. The fix was to decrypt whole blocks only.
3. **Adding ASCII85 and ASCIIHex filters,** needed by one encrypted file. That gave 525 / 525.

## Limits

- **Unsupported:** AES-256 (revisions 5 and 6), the LZW and RunLength filters, and non-empty passwords. These all return `unknown`.
- **Rough character counts:** a two-byte CID font counts double, and the thresholds (40 characters; image coverage of 30% and 80%) mirror the labeller's.
- **Constructed scans are clean:** no skew, noise, tiled strips, or JBIG2/CCITT images. Only 3 real documents need OCR.

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
