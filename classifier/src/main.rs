//! CLI: classify PDFs and time each one in-process (file reading excluded).
//! usage: scan-or-text [--repeat N] <file.pdf>...   prints one JSON object per line
use std::time::Instant;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut repeat = 5usize;
    if args.first().map(|a| a == "--repeat").unwrap_or(false) {
        repeat = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(5).max(1);
        args.drain(0..2);
    }
    for path in args {
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => { println!("{{\"file\":{:?},\"error\":{:?}}}", path, e.to_string()); continue; }
        };
        let mut times = Vec::with_capacity(repeat);
        let mut report = None;
        for _ in 0..repeat {
            let t = Instant::now();
            let r = scan_or_text::classify(&data);
            times.push(t.elapsed().as_secs_f64() * 1e6);
            report = Some(r);
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = times[times.len() / 2];
        let json = report.unwrap().to_json();
        println!("{{\"file\":{:?},\"bytes\":{},\"micros\":{:.1},{}", path, data.len(), median, &json[1..]);
    }
}
