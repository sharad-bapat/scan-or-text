//! scan_or_text: does this PDF need OCR?
//!
//! A minimal structural classifier. It never renders a page and never extracts text properly;
//! it reads just enough of the file to answer, per page: how much text is drawn (visible, or
//! invisible as in an OCR layer), and how much of the page is covered by images.
//!
//! One pass indexes every `N G obj` (and the objects packed inside compressed object streams),
//! the page tree is walked from the catalog, and each page's content stream runs through a tiny
//! interpreter that tracks the transformation matrix (q/Q/cm), text render mode (Tr), text-showing
//! operators (Tj TJ ' ") and image draws (Do, inline BI..EI, images inside form XObjects).
//!
//! The only dependency is pdf-core, the PDF reading shared with wordbox and where-are-the-regions. Page labels mirror the ground-truth labeller:
//!   TEXT       >= 40 characters of text, images cover < 80%
//!   SCAN_TEXT  >= 40 characters over an image covering >= 80%, or an invisible text layer
//!   IMAGE      < 40 characters, images cover >= 30%   -> needs OCR
//!   EMPTY      < 40 characters, little image


pub const MIN_TEXT: usize = 40;
const MAX_PAGES: usize = 500;
const MAX_FORM_DEPTH: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageClass { Text, ScanText, Image, Empty, Unknown }

#[derive(Debug, Default)]
pub struct Report {
    pub pages: usize,
    pub text: usize,
    pub scan_text: usize,
    pub image: usize,
    pub empty: usize,
    pub unknown: usize,
    pub needs_ocr: bool,
    pub category: &'static str,
    pub encrypted: bool,
    pub ocr_pages: Vec<usize>,
}

impl Report {
    pub fn to_json(&self) -> String {
        let pages: Vec<String> = self.ocr_pages.iter().map(|p| p.to_string()).collect();
        format!(
            "{{\"pages\":{},\"text\":{},\"scan_text\":{},\"image\":{},\"empty\":{},\"unknown\":{},\"needs_ocr\":{},\"category\":\"{}\",\"encrypted\":{},\"ocr_pages\":[{}]}}",
            self.pages, self.text, self.scan_text, self.image, self.empty, self.unknown,
            self.needs_ocr, self.category, self.encrypted, pages.join(",")
        )
    }
}

// The PDF reading itself (byte helpers, values, filters, the object index, decryption, the page
// tree) is pdf-core's, shared with wordbox and where-are-the-regions (D4). Its page walk goes to
// 2,000 pages; this tool still reads at most MAX_PAGES of them.
pub(crate) use pdf_core::*;

// ---------- the content-stream interpreter ----------

type M = [f64; 6];
const IDENT: M = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
fn mul(a: &M, b: &M) -> M {
    [a[0] * b[0] + a[1] * b[2], a[0] * b[1] + a[1] * b[3],
     a[2] * b[0] + a[3] * b[2], a[2] * b[1] + a[3] * b[3],
     a[4] * b[0] + a[5] * b[2] + b[4], a[4] * b[1] + a[5] * b[3] + b[5]]
}

#[derive(Default)]
struct Tally { visible: usize, invisible: usize, image_area: f64, undecodable: bool }

fn run(pdf: &Pdf, content: &[u8], resources: Option<&[u8]>, ctm0: M, t: &mut Tally, depth: usize) {
    let xobjects = resources.and_then(|r| get(r, b"/XObject")).and_then(|v| pdf.resolve(&v));
    let mut ctm = ctm0;
    let mut stack: Vec<M> = Vec::new();
    let mut nums: Vec<f64> = Vec::new();
    let mut strchars = 0usize;
    let mut name: Vec<u8> = Vec::new();
    let mut tr = 0i64;
    let s = content;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if is_ws(c) { i += 1; continue; }
        match c {
            b'%' => { while i < s.len() && s[i] != b'\n' && s[i] != b'\r' { i += 1; } }
            b'(' => {
                let e = skip_string(s, i);
                // characters, roughly: bytes inside, less escapes
                let inner = &s[i + 1..e.saturating_sub(1).max(i + 1)];
                strchars += inner.len() - inner.iter().filter(|&&b| b == b'\\').count();
                i = e;
            }
            b'<' if i + 1 < s.len() && s[i + 1] == b'<' => { i = matching(s, i); }
            b'<' => {
                let e = find(s, b">", i).unwrap_or(s.len());
                strchars += s[i + 1..e].iter().filter(|b| b.is_ascii_hexdigit()).count() / 2;
                i = e + 1;
            }
            b'[' | b']' => i += 1,
            b'/' => {
                let mut j = i + 1;
                while j < s.len() && is_regular(s[j]) { j += 1; }
                name = s[i + 1..j].to_vec();
                i = j;
            }
            b'0'..=b'9' | b'-' | b'+' | b'.' => {
                let mut j = i + 1;
                while j < s.len() && (s[j].is_ascii_digit() || s[j] == b'.') { j += 1; }
                if let Some(v) = std::str::from_utf8(&s[i..j]).ok().and_then(|x| x.parse::<f64>().ok()) {
                    nums.push(v);
                    if nums.len() > 6 { nums.remove(0); }
                }
                i = j;
            }
            b'\'' | b'"' => { show(&mut strchars, tr, t); nums.clear(); i += 1; }
            _ => {
                let mut j = i;
                while j < s.len() && is_regular(s[j]) { j += 1; }
                if j == i { i += 1; continue; }
                let op = &s[i..j];
                i = j;
                match op {
                    b"q" => stack.push(ctm),
                    b"Q" => { if let Some(m) = stack.pop() { ctm = m; } }
                    b"cm" if nums.len() >= 6 => {
                        let n = nums.len();
                        let m = [nums[n - 6], nums[n - 5], nums[n - 4], nums[n - 3], nums[n - 2], nums[n - 1]];
                        ctm = mul(&m, &ctm);
                    }
                    b"Tr" => { if let Some(v) = nums.last() { tr = *v as i64; } }
                    b"Tj" | b"TJ" => show(&mut strchars, tr, t),
                    b"Do" => draw(pdf, &xobjects, &name, &ctm, resources, t, depth),
                    b"BI" => {
                        // inline image: skip its data up to a whitespace-delimited EI
                        let id = find(s, b"ID", i).unwrap_or(s.len());
                        let mut k = id + 2;
                        // ASCII-encoded data can hold "EI" itself, so skip to its end marker first
                        if let Some(end) = ascii_data_end(&s[i..id.min(s.len())]) {
                            if let Some(e) = find(s, end, k) { k = e + end.len(); }
                        }
                        loop {
                            match find(s, b"EI", k) {
                                Some(e) if (e == 0 || is_ws(s[e - 1])) && (e + 2 >= s.len() || !is_regular(s[e + 2])) => { k = e + 2; break; }
                                Some(e) => k = e + 2,
                                None => { k = s.len(); break; }
                            }
                        }
                        t.image_area += (ctm[0] * ctm[3] - ctm[1] * ctm[2]).abs();
                        i = k;
                    }
                    _ => {}
                }
                nums.clear();
                strchars = 0;
            }
        }
    }
}

fn show(strchars: &mut usize, tr: i64, t: &mut Tally) {
    if tr == 3 { t.invisible += *strchars; } else { t.visible += *strchars; }
    *strchars = 0;
}

fn draw(pdf: &Pdf, xobjects: &Option<Vec<u8>>, name: &[u8], ctm: &M, resources: Option<&[u8]>, t: &mut Tally, depth: usize) {
    let mut key = Vec::with_capacity(name.len() + 1);
    key.push(b'/');
    key.extend_from_slice(name);
    let n = match xobjects.as_ref().and_then(|x| get(x, &key)) { Some(Val::Ref(n)) => n, _ => return };
    let d = match pdf.dict(n) { Some(d) => d, None => return };
    match get(&d, b"/Subtype") {
        Some(Val::Name(s)) if s == b"Image" => t.image_area += (ctm[0] * ctm[3] - ctm[1] * ctm[2]).abs(),
        Some(Val::Name(s)) if s == b"Form" && depth < MAX_FORM_DEPTH => {
            let m = match get(&d, b"/Matrix") { Some(Val::Array(a)) => { let v = nums_in(&a); if v.len() == 6 { [v[0], v[1], v[2], v[3], v[4], v[5]] } else { IDENT } } _ => IDENT };
            let res = get(&d, b"/Resources").and_then(|v| pdf.resolve(&v));
            match pdf.stream(n) {
                Some(body) => run(pdf, &body, res.as_deref().or(resources), mul(&m, ctm), t, depth + 1),
                None => t.undecodable = true,
            }
        }
        _ => {}
    }
}

fn classify_page(pdf: &Pdf, page: u32) -> PageClass {
    let d = match pdf.dict(page) { Some(d) => d, None => return PageClass::Unknown };
    let (w, h) = match pdf.inherited(page, b"/MediaBox") {
        Some(Val::Array(a)) => { let v = nums_in(&a); if v.len() == 4 { ((v[2] - v[0]).abs(), (v[3] - v[1]).abs()) } else { (612.0, 792.0) } }
        _ => (612.0, 792.0),
    };
    let area = (w * h).max(1.0);
    let resources = pdf.inherited(page, b"/Resources").and_then(|v| pdf.resolve(&v));
    let refs = match get(&d, b"/Contents") {
        // a stream, an array of streams, or a reference to an array object holding them
        Some(Val::Ref(n)) => match pdf.dict(n).map(|x| parse_val(&x, 0)) { Some(Val::Array(a)) => refs_in(&a), _ => vec![n] },
        Some(Val::Array(a)) => refs_in(&a),
        _ => Vec::new(),
    };
    let mut content = Vec::new();
    let mut t = Tally::default();
    for r in refs {
        match pdf.stream(r) {
            Some(b) => { content.extend_from_slice(&b); content.push(b'\n'); }
            None => t.undecodable = true,
        }
    }
    run(pdf, &content, resources.as_deref(), IDENT, &mut t, 0);
    if t.undecodable && t.visible + t.invisible == 0 && t.image_area == 0.0 { return PageClass::Unknown; }
    let chars = t.visible + t.invisible;
    let cover = (t.image_area / area).min(1.0);
    if chars >= MIN_TEXT {
        if cover >= 0.8 || t.invisible >= MIN_TEXT { PageClass::ScanText } else { PageClass::Text }
    } else if cover >= 0.3 {
        PageClass::Image
    } else {
        PageClass::Empty
    }
}

/// Classify a whole PDF from its bytes.
/// The end marker of an inline image's data when its outer filter is ASCII85 (`~>`) or ASCIIHex (`>`).
fn ascii_data_end(dict: &[u8]) -> Option<&'static [u8]> {
    let has = |k: &[u8]| { let mut f = 0; while let Some(p) = find(dict, k, f) { let e = p + k.len(); if e >= dict.len() || !is_regular(dict[e]) { return true; } f = e; } false };
    if has(b"/A85") || has(b"/ASCII85Decode") { Some(b"~>") } else if has(b"/AHx") || has(b"/ASCIIHexDecode") { Some(b">") } else { None }
}

pub fn classify(data: &[u8]) -> Report {
    let mut r = Report::default();
    if !data.starts_with(b"%PDF") && find(&data[..data.len().min(1024)], b"%PDF", 0).is_none() {
        r.category = "not_pdf";
        return r;
    }
    let pdf = Pdf::index(data);
    r.encrypted = find(data, b"/Encrypt", 0).is_some();
    let mut pages = pdf.pages();
    pages.truncate(MAX_PAGES);
    r.pages = pages.len();
    for (i, p) in pages.iter().enumerate() {
        let c = if r.encrypted && pdf.crypt.is_none() { PageClass::Unknown } else { classify_page(&pdf, *p) };
        match c {
            PageClass::Text => r.text += 1,
            PageClass::ScanText => r.scan_text += 1,
            PageClass::Image => { r.image += 1; r.ocr_pages.push(i + 1); }
            PageClass::Empty => r.empty += 1,
            PageClass::Unknown => r.unknown += 1,
        }
    }
    r.needs_ocr = r.image > 0;
    r.category = if r.pages == 0 || r.unknown == r.pages { "unknown" }
        else if r.needs_ocr && (r.text + r.scan_text) > 0 { "mixed" }
        else if r.needs_ocr { "scanned" }
        else if r.scan_text > r.text { "scanned_ocr" }
        else { "text" };
    r
}

#[cfg(feature = "wasm")]
mod wasm {
    use wasm_bindgen::prelude::*;
    /// JSON report for the bytes of one PDF.
    #[wasm_bindgen]
    pub fn classify_json(bytes: &[u8]) -> String { super::classify(bytes).to_json() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small PDF from numbered object bodies, with a catalog as object 1.
    fn pdf(objs: &[(u32, &[u8])]) -> Vec<u8> {
        let mut out = b"%PDF-1.4
".to_vec();
        for (n, body) in objs {
            out.extend_from_slice(format!("{} 0 obj
", n).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"
endobj
");
        }
        out.extend_from_slice(b"trailer
<< /Root 1 0 R >>
%%EOF
");
        out
    }

    fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
        let mut v = format!("<< {} /Length {} >>
stream
", dict, data.len()).into_bytes();
        v.extend_from_slice(data);
        v.extend_from_slice(b"
endstream");
        v
    }

    const TEXT: &[u8] = b"BT /F1 12 Tf 72 700 Td (The quick brown fox jumps over the lazy dog, twice over.) Tj ET";

    fn one_page(contents: &str, extra: &[(u32, &[u8])]) -> Vec<u8> {
        let page = format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} >>", contents);
        let mut objs: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, page.as_bytes()),
        ];
        objs.extend_from_slice(extra);
        pdf(&objs)
    }

    #[test]
    fn contents_as_reference_to_array() {
        let s = stream("", TEXT);
        let data = one_page("4 0 R", &[(4, b"[5 0 R]"), (5, &s)]);
        let r = classify(&data);
        assert_eq!((r.pages, r.text, r.unknown), (1, 1, 0), "{:?}", r);
    }

    #[test]
    fn lzw_and_run_length_content() {
        // "-----A---B" in LZW (the ISO 32000-1 7.4.4.2 example), and a RunLength literal run
        let lzw_data = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        assert_eq!(lzw(&lzw_data, true), b"-----A---B");
        assert_eq!(run_length(&[2, b'a', b'b', b'c', 253, b'x', 128]), b"abcxxxx");

        let mut rl = vec![(TEXT.len() - 1) as u8];
        rl.extend_from_slice(TEXT);
        rl.push(128);
        let s = stream("/Filter /RunLengthDecode", &rl);
        let r = classify(&one_page("4 0 R", &[(4, &s)]));
        assert_eq!((r.text, r.unknown), (1, 0), "{:?}", r);
    }
    #[test]
    fn ascii85_inline_data_holding_ei() {
        // the A85 data has a line starting "EI(": stopping there opens a string that swallows the text after it
        for img in [&b"BI /W 2 /H 1 /CS /G /BPC 8 /F /A85 ID ab\nEI(cd~> EI\n"[..], b"BI /W 2 /H 1 /CS /G /BPC 8 /F [/AHx /Fl] ID 0E\nEI(> EI\n"] {
            let s = stream("", &[img, TEXT].concat());
            let r = classify(&one_page("4 0 R", &[(4, &s)]));
            assert_eq!((r.text, r.unknown), (1, 0), "{:?}", r);
        }
    }

    #[test]
    fn many_small_objects_index_in_linear_time() {
        // 20000 stream-less objects before the only stream: each one used to search to the end
        let bodies: Vec<(u32, Vec<u8>)> = (10..20010).map(|n| (n, format!("<< /K {} >>", n).into_bytes())).collect();
        let s = stream("", TEXT);
        let mut extra: Vec<(u32, &[u8])> = bodies.iter().map(|(n, b)| (*n, b.as_slice())).collect();
        extra.push((4, &s));
        let data = one_page("4 0 R", &extra);
        let t = std::time::Instant::now();
        let r = classify(&data);
        assert_eq!(r.text, 1, "{:?}", r);
        assert!(t.elapsed().as_secs_f64() < 0.3, "took {:?}", t.elapsed());
    }

    fn objstm(num: u32, body: &str) -> Vec<u8> {
        let header = format!("{} 0 ", num);
        stream(&format!("/Type /ObjStm /N 1 /First {}", header.len()), format!("{}{}", header, body).as_bytes())
    }

    #[test]
    fn later_copy_of_an_object_wins() {
        let text = stream("", TEXT);
        let blank = stream("", b"");
        let page = |c: u32| format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R >>", c);
        let (to_text, to_blank) = (objstm(3, &page(4)), objstm(3, &page(5)));
        let head: [(u32, &[u8]); 4] = [(1, b"<< /Type /Catalog /Pages 2 0 R >>"), (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"), (4, &text), (5, &blank)];
        let classes = |objs: &[(u32, &[u8])]| {
            let mut all = head.to_vec();
            all.extend_from_slice(objs);
            let r = classify(&pdf(&all));
            (r.text, r.empty)
        };
        // two object streams hold page 3: the one later in the file wins, whichever it is
        assert_eq!(classes(&[(6, &to_blank), (7, &to_text)]), (1, 0));
        assert_eq!(classes(&[(6, &to_text), (7, &to_blank)]), (0, 1));
        // a top-level copy wins over an earlier object stream, and loses to a later one
        let top_blank = page(5);
        assert_eq!(classes(&[(6, &to_text), (3, top_blank.as_bytes())]), (0, 1));
        assert_eq!(classes(&[(3, top_blank.as_bytes()), (6, &to_text)]), (1, 0));
    }
}
