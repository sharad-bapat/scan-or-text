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
//! The only dependency is miniz_oxide (inflate). Page labels mirror the ground-truth labeller:
//!   TEXT       >= 40 characters of text, images cover < 80%
//!   SCAN_TEXT  >= 40 characters over an image covering >= 80%, or an invisible text layer
//!   IMAGE      < 40 characters, images cover >= 30%   -> needs OCR
//!   EMPTY      < 40 characters, little image
use std::collections::HashMap;

mod crypt;

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

// ---------- low-level byte helpers ----------

fn is_ws(b: u8) -> bool { matches!(b, b' ' | b'\n' | b'\r' | b'\t' | 0x0c | 0) }
fn is_delim(b: u8) -> bool { matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%') }
fn is_regular(b: u8) -> bool { !is_ws(b) && !is_delim(b) }

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() { return None; }
    let first = needle[0];
    let last = hay.len().checked_sub(needle.len())?;
    let mut i = from;
    while i <= last {
        match hay[i..=last].iter().position(|&b| b == first) {
            None => return None,
            Some(off) => {
                i += off;
                if &hay[i..i + needle.len()] == needle { return Some(i); }
                i += 1;
            }
        }
    }
    None
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() { return None; }
    (0..=hay.len() - needle.len()).rev().find(|&i| &hay[i..i + needle.len()] == needle)
}

fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() {
        if is_ws(s[i]) { i += 1; }
        else if s[i] == b'%' { while i < s.len() && s[i] != b'\n' && s[i] != b'\r' { i += 1; } }
        else { break; }
    }
    i
}

fn parse_uint(s: &[u8], i: usize) -> Option<(u64, usize)> {
    let mut j = i;
    let mut v: u64 = 0;
    while j < s.len() && s[j].is_ascii_digit() { v = v.saturating_mul(10).saturating_add((s[j] - b'0') as u64); j += 1; }
    if j == i { None } else { Some((v, j)) }
}

/// Index of the matching close for a balanced "<<...>>" or "[...]" starting at `i` (at the opener).
fn matching(s: &[u8], i: usize) -> usize {
    let mut depth = 0i32;
    let mut j = i;
    while j < s.len() {
        match s[j] {
            b'(' => { j = skip_string(s, j); continue; }
            b'<' if j + 1 < s.len() && s[j + 1] == b'<' => { depth += 1; j += 2; continue; }
            b'>' if j + 1 < s.len() && s[j + 1] == b'>' => { depth -= 1; j += 2; if depth == 0 { return j; } continue; }
            b'[' => depth += 1,
            b']' => { depth -= 1; if depth == 0 { return j + 1; } }
            _ => {}
        }
        j += 1;
    }
    s.len()
}

/// Skip a literal string "(...)" starting at `i`; returns the index after it.
fn skip_string(s: &[u8], i: usize) -> usize {
    let mut depth = 0i32;
    let mut j = i;
    while j < s.len() {
        match s[j] {
            b'\\' => { j += 2; continue; }
            b'(' => depth += 1,
            b')' => { depth -= 1; if depth == 0 { return j + 1; } }
            _ => {}
        }
        j += 1;
    }
    s.len()
}

// ---------- values inside dictionaries ----------

#[derive(Clone, Debug)]
enum Val { Ref(u32), Num(f64), Name(Vec<u8>), Dict(Vec<u8>), Array(Vec<u8>), Other }

fn parse_val(s: &[u8], i: usize) -> Val {
    let i = skip_ws(s, i);
    if i >= s.len() { return Val::Other; }
    match s[i] {
        b'/' => {
            let mut j = i + 1;
            while j < s.len() && is_regular(s[j]) { j += 1; }
            Val::Name(s[i + 1..j].to_vec())
        }
        b'<' if i + 1 < s.len() && s[i + 1] == b'<' => { let e = matching(s, i); Val::Dict(s[i..e.min(s.len())].to_vec()) }
        b'[' => { let e = matching(s, i); Val::Array(s[i + 1..e.saturating_sub(1).max(i + 1)].to_vec()) }
        b'0'..=b'9' => {
            if let Some((n, j)) = parse_uint(s, i) {
                let k = skip_ws(s, j);
                if let Some((_, k2)) = parse_uint(s, k) {
                    let k3 = skip_ws(s, k2);
                    if k3 < s.len() && s[k3] == b'R' && (k3 + 1 >= s.len() || !is_regular(s[k3 + 1])) { return Val::Ref(n as u32); }
                }
                return parse_num(s, i).map(Val::Num).unwrap_or(Val::Other);
            }
            Val::Other
        }
        b'-' | b'+' | b'.' => parse_num(s, i).map(Val::Num).unwrap_or(Val::Other),
        _ => Val::Other,
    }
}

fn parse_num(s: &[u8], i: usize) -> Option<f64> {
    let mut j = i;
    while j < s.len() && (s[j].is_ascii_digit() || matches!(s[j], b'-' | b'+' | b'.')) { j += 1; }
    std::str::from_utf8(&s[i..j]).ok()?.parse().ok()
}

/// Value of a top-level key in a dictionary's bytes (nested dictionaries are skipped).
fn get(dict: &[u8], key: &[u8]) -> Option<Val> {
    let mut i = if dict.starts_with(b"<<") { 2 } else { 0 };
    while i < dict.len() {
        i = skip_ws(dict, i);
        if i >= dict.len() { break; }
        match dict[i] {
            b'/' => {
                let mut j = i + 1;
                while j < dict.len() && is_regular(dict[j]) { j += 1; }
                let k = &dict[i..j];
                let v = parse_val(dict, j);
                if k == key { return Some(v); }
                i = skip_val(dict, j);
            }
            b'>' => break,
            _ => i += 1,
        }
    }
    None
}

fn skip_val(s: &[u8], i: usize) -> usize {
    let i = skip_ws(s, i);
    if i >= s.len() { return i; }
    match s[i] {
        b'<' if i + 1 < s.len() && s[i + 1] == b'<' => matching(s, i),
        b'[' => matching(s, i),
        b'(' => skip_string(s, i),
        b'<' => find(s, b">", i).map(|e| e + 1).unwrap_or(s.len()),
        b'/' => { let mut j = i + 1; while j < s.len() && is_regular(s[j]) { j += 1; } j }
        _ => {
            // a number, or "N G R"
            let mut j = i;
            while j < s.len() && is_regular(s[j]) { j += 1; }
            let k = skip_ws(s, j);
            if let Some((_, k2)) = parse_uint(s, k) {
                let k3 = skip_ws(s, k2);
                if k3 < s.len() && s[k3] == b'R' { return k3 + 1; }
            }
            j
        }
    }
}

fn refs_in(s: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        i = skip_ws(s, i);
        if i >= s.len() { break; }
        if s[i].is_ascii_digit() {
            if let Val::Ref(n) = parse_val(s, i) { out.push(n); }
        }
        i = skip_val(s, i).max(i + 1);
    }
    out
}

fn nums_in(s: &[u8]) -> Vec<f64> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        i = skip_ws(s, i);
        if i < s.len() && (s[i].is_ascii_digit() || matches!(s[i], b'-' | b'+' | b'.')) {
            if let Some(v) = parse_num(s, i) { out.push(v); }
        }
        i = skip_val(s, i).max(i + 1);
    }
    out
}

/// ASCII85 (base-85) decoding, up to the "~>" end marker.
fn ascii85(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 4 / 5);
    let (mut acc, mut n) = (0u64, 0usize);
    let mut i = if s.starts_with(b"<~") { 2 } else { 0 };
    while i < s.len() {
        let c = s[i];
        i += 1;
        match c {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                acc = acc * 85 + (c - b'!') as u64;
                n += 1;
                if n == 5 { out.extend_from_slice(&(acc as u32).to_be_bytes()); acc = 0; n = 0; }
            }
            _ if is_ws(c) => {}
            _ => return None,
        }
    }
    if n > 1 {
        for _ in n..5 { acc = acc * 85 + 84; }
        out.extend_from_slice(&(acc as u32).to_be_bytes()[..n - 1]);
    }
    Some(out)
}

/// LZW decoding as PDF uses it (ISO 32000-1 7.4.4): 9 to 12 bit codes, 256 = clear, 257 = end.
/// With `early` (the default EarlyChange 1), the code width grows one code sooner.
fn lzw(data: &[u8], early: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 3);
    // each entry is (prefix entry, last byte, first byte, length); strings are rebuilt on output
    let mut table: Vec<(u32, u8, u8, u32)> = (0..256u32).map(|b| (u32::MAX, b as u8, b as u8, 1)).collect();
    table.push((0, 0, 0, 0));
    table.push((0, 0, 0, 0));
    let (mut width, mut buf, mut nbits, mut i) = (9u32, 0u32, 0u32, 0usize);
    let mut prev: Option<u32> = None;
    let mut scratch = Vec::new();
    loop {
        while nbits < width {
            if i >= data.len() { return out; }
            buf = (buf << 8) | data[i] as u32;
            i += 1;
            nbits += 8;
        }
        let code = (buf >> (nbits - width)) & ((1 << width) - 1);
        nbits -= width;
        buf &= (1 << nbits) - 1;
        if code == 256 { table.truncate(258); width = 9; prev = None; continue; }
        if code == 257 { break; }
        let known = (code as usize) < table.len();
        let (first, entry) = match (known, prev) {
            (true, _) => (table[code as usize].2, code),
            (false, Some(p)) if code as usize == table.len() => (table[p as usize].2, u32::MAX),
            _ => break,
        };
        if let Some(p) = prev {
            let pe = table[p as usize];
            if table.len() < 4096 { table.push((p, first, pe.2, pe.3 + 1)); }
        }
        let e = if entry == u32::MAX { (table.len() - 1) as u32 } else { entry };
        // walk the entry back to its root, then reverse
        scratch.clear();
        let mut k = e;
        while k != u32::MAX { let t = table[k as usize]; scratch.push(t.1); k = t.0; }
        out.extend(scratch.iter().rev());
        prev = Some(e);
        let size = table.len() as u32 + if early { 1 } else { 0 };
        if size >= (1 << width) && width < 12 { width += 1; }
    }
    out
}

/// RunLengthDecode (ISO 32000-1 7.4.5).
fn run_length(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 2);
    let mut i = 0;
    while i < data.len() {
        let n = data[i] as usize;
        i += 1;
        if n < 128 { let e = (i + n + 1).min(data.len()); out.extend_from_slice(&data[i..e]); i = e; }
        else if n > 128 { if i < data.len() { out.extend(std::iter::repeat(data[i]).take(257 - n)); } i += 1; }
        else { break; }
    }
    out
}

/// A string value (literal "(...)" with escapes, or hex "<...>") at position `i`.
fn string_at(s: &[u8], i: usize) -> Option<Vec<u8>> {
    if i >= s.len() { return None; }
    if s[i] == b'<' {
        let e = find(s, b">", i)?;
        let hex: Vec<u8> = s[i + 1..e].iter().copied().filter(|b| b.is_ascii_hexdigit()).collect();
        let val = |c: u8| (c as char).to_digit(16).unwrap() as u8;
        return Some(hex.chunks(2).map(|p| (val(p[0]) << 4) | if p.len() > 1 { val(p[1]) } else { 0 }).collect());
    }
    if s[i] != b'(' { return None; }
    let (mut out, mut depth, mut j) = (Vec::new(), 1i32, i + 1);
    while j < s.len() {
        let c = s[j];
        match c {
            b'\\' if j + 1 < s.len() => {
                j += 1;
                match s[j] {
                    b'n' => out.push(b'\n'), b'r' => out.push(b'\r'), b't' => out.push(b'\t'),
                    b'b' => out.push(8), b'f' => out.push(12),
                    b'\r' => { if j + 1 < s.len() && s[j + 1] == b'\n' { j += 1; } }
                    b'\n' => {}
                    d @ b'0'..=b'7' => {
                        let mut v = (d - b'0') as u32;
                        for _ in 0..2 { if j + 1 < s.len() && (b'0'..=b'7').contains(&s[j + 1]) { j += 1; v = v * 8 + (s[j] - b'0') as u32; } }
                        out.push(v as u8);
                    }
                    other => out.push(other),
                }
            }
            b'(' => { depth += 1; out.push(c); }
            b')' => { depth -= 1; if depth == 0 { return Some(out); } out.push(c); }
            _ => out.push(c),
        }
        j += 1;
    }
    Some(out)
}

/// A top-level string value of `key` in a dictionary.
fn get_string(dict: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    let mut i = if dict.starts_with(b"<<") { 2 } else { 0 };
    while i < dict.len() {
        i = skip_ws(dict, i);
        if i >= dict.len() || dict[i] != b'/' { i += 1; continue; }
        let mut j = i + 1;
        while j < dict.len() && is_regular(dict[j]) { j += 1; }
        if &dict[i..j] == key { return string_at(dict, skip_ws(dict, j)); }
        i = skip_val(dict, j);
    }
    None
}

// ---------- the object index ----------

enum Loc {
    Top { gen: u16, dict: (usize, usize), stream: Option<(usize, usize)> },
    Packed { buf: usize, start: usize, end: usize },
}

struct Pdf<'a> {
    data: &'a [u8],
    bufs: Vec<Vec<u8>>,
    objs: HashMap<u32, Loc>,
    crypt: Option<crypt::Crypt>,
}

impl<'a> Pdf<'a> {
    fn index(data: &'a [u8]) -> Pdf<'a> {
        let mut objs = HashMap::new();
        let mut i = 0;
        while let Some(p) = find(data, b"obj", i) {
            i = p + 3;
            if p == 0 || !is_ws(data[p - 1]) { continue; }
            if p + 3 < data.len() && is_regular(data[p + 3]) { continue; }
            // walk back over "N G "
            let mut j = p - 1;
            while j > 0 && is_ws(data[j]) { j -= 1; }
            let gen_end = j + 1;
            while j > 0 && data[j].is_ascii_digit() { j -= 1; }
            if j + 1 == gen_end || !is_ws(data[j]) { continue; }
            let gen = parse_uint(data, j + 1).map(|(g, _)| g as u16).unwrap_or(0);
            while j > 0 && is_ws(data[j]) { j -= 1; }
            let num_end = j + 1;
            while j > 0 && data[j].is_ascii_digit() { j -= 1; }
            let num_start = if data[j].is_ascii_digit() { j } else { j + 1 };
            if num_start == num_end { continue; }
            let num = match parse_uint(data, num_start) { Some((n, _)) => n as u32, None => continue };
            let start = p + 3;
            let end = find(data, b"endobj", start).unwrap_or(data.len());
            // search only inside this object: an unbounded search runs to the next stream in the file,
            // which is quadratic in files with many small objects
            let stream = find(&data[..end], b"stream", start);
            let loc = match stream {
                Some(k) => {
                    let mut s = k + 6;
                    if s < data.len() && data[s] == b'\r' { s += 1; }
                    if s < data.len() && data[s] == b'\n' { s += 1; }
                    let e = rfind(&data[s..end], b"endstream").map(|x| s + x).unwrap_or(end);
                    Loc::Top { gen, dict: (start, k), stream: Some((s, e)) }
                }
                None => Loc::Top { gen, dict: (start, end), stream: None },
            };
            objs.insert(num, loc);
            i = end.max(i);
        }
        let mut pdf = Pdf { data, bufs: Vec::new(), objs, crypt: None };
        pdf.crypt = pdf.security_handler();
        pdf.unpack_object_streams();
        pdf
    }

    /// The standard security handler, when the file opens with an empty user password.
    fn security_handler(&self) -> Option<crypt::Crypt> {
        let p = rfind(self.data, b"/Encrypt")?;
        let enc = match parse_val(self.data, p + 8) { Val::Ref(n) => self.dict(n)?, Val::Dict(d) => d, _ => return None };
        if !matches!(get(&enc, b"/Filter"), Some(Val::Name(f)) if f == b"Standard") { return None; }
        let r = match get(&enc, b"/R") { Some(Val::Num(v)) => v as u32, _ => return None };
        // Key length in bits: the top-level /Length if present; revision 4 files usually give it only
        // inside the crypt filter (/CF /StdCF /Length, often in bytes) and AESV2 is always 128.
        let length = match get(&enc, b"/Length") {
            Some(Val::Num(v)) => v as u32,
            _ if r >= 4 => {
                let inner = find(&enc, b"/StdCF", 0).and_then(|k| find(&enc, b"/Length", k)).map(|k| parse_val(&enc, k + 7));
                match inner { Some(Val::Num(v)) if v <= 32.0 => v as u32 * 8, Some(Val::Num(v)) => v as u32, _ => 128 }
            }
            _ => 40,
        };
        let perms = match get(&enc, b"/P") { Some(Val::Num(v)) => v as i64 as i32, _ => return None };
        let o = get_string(&enc, b"/O")?;
        let aes = find(&enc, b"/AESV2", 0).is_some();
        let encrypt_metadata = !matches!(find(&enc, b"/EncryptMetadata", 0), Some(k) if enc[k..].starts_with(b"/EncryptMetadata false"));
        let idp = rfind(self.data, b"/ID")?;
        let id0 = match parse_val(self.data, idp + 3) { Val::Array(a) => string_at(&a, skip_ws(&a, 0))?, _ => return None };
        crypt::Crypt::new(r, if r == 2 { 40 } else { length }, &o, perms, &id0, aes, encrypt_metadata)
    }

    /// Objects packed in object streams. Deterministic: a packed copy replaces a top-level object, or
    /// a copy in another object stream, only when its stream sits later in the file, as an incremental
    /// update would. Every stream is read before any entry changes, so the HashMap's order can't
    /// matter (taking the first copy seen made the winner depend on its seed).
    fn unpack_object_streams(&mut self) {
        let stms: Vec<(usize, u32)> = self.objs.iter()
            .filter_map(|(n, l)| match l { Loc::Top { dict, stream: Some(_), .. } => Some((dict.0, *n)), _ => None })
            .filter(|(_, n)| matches!(self.dict(*n).and_then(|d| get(&d, b"/Type")), Some(Val::Name(t)) if t == b"ObjStm"))
            .collect();
        let mut packed = Vec::new();
        for (pos, n) in stms {
            let dict = match self.dict(n) { Some(d) => d, None => continue };
            let count = match get(&dict, b"/N") { Some(Val::Num(v)) => v as usize, _ => continue };
            let first = match get(&dict, b"/First") { Some(Val::Num(v)) => v as usize, _ => continue };
            let buf = match self.stream(n) { Some(b) => b, None => continue };
            if first > buf.len() { continue; }
            let header = nums_in(&buf[..first]);
            let idx = self.bufs.len();
            for k in 0..count.min(header.len() / 2) {
                let num = header[2 * k] as u32;
                let off = first + header[2 * k + 1] as usize;
                let next = if k + 1 < header.len() / 2 { first + header[2 * k + 3] as usize } else { buf.len() };
                if off <= next && next <= buf.len() { packed.push((pos, num, idx, off, next)); }
            }
            self.bufs.push(buf);
        }
        // file position of the stream each packed object came from, for the "later wins" rule
        let mut packed_at: HashMap<u32, usize> = HashMap::new();
        for (pos, num, buf, start, end) in packed {
            let later = match self.objs.get(&num) {
                None => true,
                Some(Loc::Packed { .. }) => packed_at.get(&num).map(|&p| p < pos).unwrap_or(true),
                Some(Loc::Top { dict, .. }) => dict.0 < pos,
            };
            if later {
                self.objs.insert(num, Loc::Packed { buf, start, end });
                packed_at.insert(num, pos);
            }
        }
    }

    fn dict(&self, n: u32) -> Option<Vec<u8>> {
        let raw: &[u8] = match self.objs.get(&n)? {
            Loc::Top { dict, .. } => &self.data[dict.0..dict.1],
            Loc::Packed { buf, start, end } => &self.bufs[*buf][*start..*end],
        };
        let s = skip_ws(raw, 0);
        if raw[s..].starts_with(b"<<") { let e = matching(raw, s); Some(raw[s..e].to_vec()) } else { Some(raw[s..].to_vec()) }
    }

    fn resolve(&self, v: &Val) -> Option<Vec<u8>> {
        match v { Val::Dict(d) => Some(d.clone()), Val::Ref(n) => self.dict(*n), _ => None }
    }

    /// Decoded stream data (Flate, LZW, RunLength, ASCII85, ASCIIHex or unfiltered). None for other filters.
    fn stream(&self, n: u32) -> Option<Vec<u8>> {
        let (gen, s, e) = match self.objs.get(&n)? { Loc::Top { gen, stream: Some(r), .. } => (*gen, r.0, r.1), _ => return None };
        let dict = self.dict(n)?;
        let is_xref = matches!(get(&dict, b"/Type"), Some(Val::Name(t)) if t == b"XRef");
        let decrypted;
        let raw: &[u8] = match &self.crypt {
            Some(c) if !is_xref => { decrypted = c.decrypt(n, gen, &self.data[s..e.max(s)])?; &decrypted }
            _ => &self.data[s..e.max(s)],
        };
        let filters: Vec<Vec<u8>> = match get(&dict, b"/Filter") {
            None => Vec::new(),
            Some(Val::Name(f)) => vec![f],
            Some(Val::Array(a)) => {
                let mut v = Vec::new();
                let mut i = 0;
                while i < a.len() { if let Val::Name(f) = parse_val(&a, i) { v.push(f); } i = skip_val(&a, i).max(i + 1); }
                v
            }
            Some(Val::Ref(r)) => match self.dict(r) { Some(d) if d.starts_with(b"/") => vec![d[1..].to_vec()], _ => return None },
            _ => return None,
        };
        let mut out = raw.to_vec();
        for f in filters {
            if f == b"ASCII85Decode" || f == b"A85" {
                out = ascii85(&out)?;
            } else if f == b"ASCIIHexDecode" || f == b"AHx" {
                let mut v = Vec::with_capacity(out.len() / 2);
                let digits: Vec<u8> = out.iter().copied().take_while(|&b| b != b'>').filter(|b| b.is_ascii_hexdigit()).collect();
                for p in digits.chunks(2) {
                    let h = |c: u8| (c as char).to_digit(16).unwrap() as u8;
                    v.push((h(p[0]) << 4) | if p.len() > 1 { h(p[1]) } else { 0 });
                }
                out = v;
            } else if f == b"LZWDecode" || f == b"LZW" {
                let early = find(&dict, b"/EarlyChange 0", 0).is_none();
                out = lzw(&out, early);
            } else if f == b"RunLengthDecode" || f == b"RL" {
                out = run_length(&out);
            } else if f == b"FlateDecode" || f == b"Fl" {
                out = match miniz_oxide::inflate::decompress_to_vec_zlib(&out) {
                    Ok(v) => v,
                    Err(e) if !e.output.is_empty() => e.output,
                    Err(_) => miniz_oxide::inflate::decompress_to_vec(&out).ok()?,
                };
            } else {
                return None;
            }
        }
        Some(out)
    }

    fn pages(&self) -> Vec<u32> {
        let mut out = Vec::new();
        if let Some(root) = self.root() {
            if let Some(cat) = self.dict(root) {
                if let Some(Val::Ref(p)) = get(&cat, b"/Pages") {
                    let mut seen = std::collections::HashSet::new();
                    self.walk(p, &mut out, &mut seen, 0);
                }
            }
        }
        if out.is_empty() {
            let mut v: Vec<u32> = self.objs.keys().copied()
                .filter(|n| matches!(self.dict(*n).and_then(|d| get(&d, b"/Type")), Some(Val::Name(t)) if t == b"Page"))
                .collect();
            v.sort_unstable();
            out = v;
        }
        out.truncate(MAX_PAGES);
        out
    }

    fn walk(&self, n: u32, out: &mut Vec<u32>, seen: &mut std::collections::HashSet<u32>, depth: usize) {
        if depth > 32 || !seen.insert(n) || out.len() >= MAX_PAGES { return; }
        let d = match self.dict(n) { Some(d) => d, None => return };
        match get(&d, b"/Type") {
            Some(Val::Name(t)) if t == b"Pages" => {
                if let Some(Val::Array(kids)) = get(&d, b"/Kids") { for k in refs_in(&kids) { self.walk(k, out, seen, depth + 1); } }
            }
            _ => {
                if let Some(Val::Array(kids)) = get(&d, b"/Kids") { for k in refs_in(&kids) { self.walk(k, out, seen, depth + 1); } }
                else { out.push(n); }
            }
        }
    }

    fn root(&self) -> Option<u32> {
        // the last /Root wins (trailers of incremental updates, or an xref stream's dictionary)
        let p = rfind(self.data, b"/Root")?;
        match parse_val(self.data, p + 5) { Val::Ref(n) => Some(n), _ => None }
    }

    /// A page attribute, inherited through /Parent when missing.
    fn inherited(&self, page: u32, key: &[u8]) -> Option<Val> {
        let mut n = page;
        for _ in 0..16 {
            let d = self.dict(n)?;
            if let Some(v) = get(&d, key) { return Some(v); }
            match get(&d, b"/Parent") { Some(Val::Ref(p)) => n = p, _ => return None }
        }
        None
    }
}

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
pub fn classify(data: &[u8]) -> Report {
    let mut r = Report::default();
    if !data.starts_with(b"%PDF") && find(&data[..data.len().min(1024)], b"%PDF", 0).is_none() {
        r.category = "not_pdf";
        return r;
    }
    let pdf = Pdf::index(data);
    r.encrypted = find(data, b"/Encrypt", 0).is_some();
    let pages = pdf.pages();
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
