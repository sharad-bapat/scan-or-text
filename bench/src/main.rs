//! Head-to-head: scan_or_text vs pdf-inspector's classify_pdf_mem, same bytes, same process.
//! usage: bench <files.txt> <out-ours.jsonl> <out-inspector.jsonl> [repeat]
use std::io::Write;
use std::time::Instant;

use pdf_inspector::PdfType;

fn median(mut v: Vec<f64>) -> f64 { v.sort_by(|a, b| a.partial_cmp(b).unwrap()); v[v.len() / 2] }

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let list = std::fs::read_to_string(&a[1]).expect("file list");
    let mut ours = std::fs::File::create(&a[2]).unwrap();
    let mut theirs = std::fs::File::create(&a[3]).unwrap();
    let repeat: usize = a.get(4).and_then(|v| v.parse().ok()).unwrap_or(5);
    for path in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let data = match std::fs::read(path) { Ok(d) => d, Err(_) => continue };

        let mut t = Vec::new();
        let mut rep = None;
        for _ in 0..repeat { let s = Instant::now(); rep = Some(scan_or_text::classify(&data)); t.push(s.elapsed().as_secs_f64() * 1e6); }
        let r = rep.unwrap();
        writeln!(ours, "{{\"file\":{:?},\"micros\":{:.1},{}", path, median(t), &r.to_json()[1..]).unwrap();

        let mut t = Vec::new();
        let mut out = None;
        for _ in 0..repeat {
            let s = Instant::now();
            let res = std::panic::catch_unwind(|| pdf_inspector::classify_pdf_mem(&data));
            t.push(s.elapsed().as_secs_f64() * 1e6);
            out = Some(res);
        }
        let us = median(t);
        match out.unwrap() {
            Ok(Ok(c)) => {
                let category = match c.pdf_type { PdfType::TextBased => "text", PdfType::Scanned | PdfType::ImageBased => "scanned", PdfType::Mixed => "mixed" };
                let needs_ocr = !c.pages_needing_ocr.is_empty() || matches!(c.pdf_type, PdfType::Scanned | PdfType::ImageBased);
                writeln!(theirs, "{{\"file\":{:?},\"micros\":{:.1},\"needs_ocr\":{},\"category\":\"{}\",\"pages\":{},\"confidence\":{:.3}}}",
                    path, us, needs_ocr, category, c.page_count, c.confidence).unwrap();
            }
            Ok(Err(e)) => { writeln!(theirs, "{{\"file\":{:?},\"micros\":{:.1},\"error\":{:?}}}", path, us, e.to_string()).unwrap(); }
            Err(_) => { writeln!(theirs, "{{\"file\":{:?},\"micros\":{:.1},\"error\":\"panic\"}}", path, us).unwrap(); }
        }
    }
}
