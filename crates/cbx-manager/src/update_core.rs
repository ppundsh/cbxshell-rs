//! Platform-independent pieces of the updater: release manifest parsing, version comparison,
//! base64 and minisign key / signature normalisation. Kept free of Win32 so it is unit-testable.
//!
//! Manifest format (`latest.json`, attached to every GitHub release; same shape as Tauri's):
//!
//! ```json
//! {
//!   "version": "2.0.2",
//!   "notes": "…",
//!   "pub_date": "2026-11-20T00:00:00Z",
//!   "platforms": {
//!     "windows-x86_64": {
//!       "url": "https://github.com/ppundsh/cbxshell-rs/releases/download/v2.0.2/CBXShell-Setup.exe",
//!       "signature": "<base64 of the minisign .sig file>"
//!     }
//!   }
//! }
//! ```

#![allow(dead_code)] // notes / pub_date and the generic JSON variants are kept for diagnostics

/// Every installer URL must start with this prefix (release assets of this repository only).
pub const ALLOWED_URL_PREFIX: &str = "https://github.com/ppundsh/cbxshell-rs/releases/download/";
/// Platform key inside `platforms`.
pub const PLATFORM_KEY: &str = "windows-x86_64";
const MAX_JSON_DEPTH: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    pub notes: String,
    pub pub_date: String,
    pub url: String,
    /// minisign signature text (already base64-decoded when the manifest carried it encoded).
    pub signature: String,
    /// Maintainer approval (armored SSH signature made with the YubiKey); empty when absent.
    pub approval: String,
}

// ---------------------------------------------------------------------------------------------
// Minimal JSON

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

struct JsonParser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> JsonParser<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("JSON: expected '{}' at {}", c as char, self.i))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_JSON_DEPTH {
            return Err("JSON: nested too deeply".into());
        }
        self.ws();
        match self.b.get(self.i).copied() {
            Some(b'{') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Obj(items));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.eat(b':')?;
                    let v = self.value(depth + 1)?;
                    items.push((k, v));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Obj(items));
                        }
                        _ => return Err("JSON: expected ',' or '}'".into()),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Arr(items));
                        }
                        _ => return Err("JSON: expected ',' or ']'".into()),
                    }
                }
            }
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') if self.b[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Json::Bool(true))
            }
            Some(b'f') if self.b[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Json::Bool(false))
            }
            Some(b'n') if self.b[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(Json::Null)
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => {
                let start = self.i;
                while self.i < self.b.len()
                    && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    self.i += 1;
                }
                let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
                s.parse::<f64>().map(Json::Num).map_err(|e| format!("JSON number: {e}"))
            }
            _ => Err(format!("JSON: unexpected input at {}", self.i)),
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.b.get(self.i..self.i + 4).ok_or("JSON: short \\u escape")?;
        let s = std::str::from_utf8(h).map_err(|e| e.to_string())?;
        self.i += 4;
        u32::from_str_radix(s, 16).map_err(|e| e.to_string())
    }

    fn string(&mut self) -> Result<String, String> {
        if self.b.get(self.i) != Some(&b'"') {
            return Err(format!("JSON: expected string at {}", self.i));
        }
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.i < self.b.len() && self.b[self.i] != b'"' && self.b[self.i] != b'\\' {
                if self.b[self.i] < 0x20 {
                    return Err("JSON: control character in string".into());
                }
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?);
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = *self.b.get(self.i).ok_or("JSON: dangling escape")?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.b[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                }
                            }
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                        }
                        _ => return Err("JSON: bad escape".into()),
                    }
                }
                _ => return Err("JSON: unterminated string".into()),
            }
        }
    }
}

pub fn parse_json(text: &str) -> Result<Json, String> {
    let mut p = JsonParser { b: text.trim_start_matches('\u{feff}').as_bytes(), i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err("JSON: trailing data".into());
    }
    Ok(v)
}

// ---------------------------------------------------------------------------------------------
// Base64 (standard alphabet; whitespace ignored, padding optional)

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let clean: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let body = {
        let mut end = clean.len();
        while end > 0 && clean[end - 1] == b'=' {
            end -= 1;
        }
        if clean.len() - end > 2 {
            return None;
        }
        &clean[..end]
    };
    if body.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    for chunk in body.chunks(4) {
        let mut acc = 0u32;
        for (k, &c) in chunk.iter().enumerate() {
            acc |= val(c)? << (18 - 6 * k as u32);
        }
        out.push((acc >> 16) as u8);
        if chunk.len() > 2 {
            out.push((acc >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(acc as u8);
        }
    }
    Some(out)
}

/// Accepts a minisign text blob either verbatim (`untrusted comment: …` lines) or wrapped in
/// base64 as Tauri does, and returns the plain text.
pub fn unwrap_minisign_text(s: &str) -> Option<String> {
    let t = s.trim();
    if t.starts_with("untrusted comment:") {
        return Some(t.to_string());
    }
    let decoded = String::from_utf8(base64_decode(t)?).ok()?;
    decoded.trim_start().starts_with("untrusted comment:").then(|| decoded.trim().to_string())
}

/// Extracts the base64 key line (`RW…`) from `assets/updater.pub`.
///
/// Accepted forms: a minisign `.pub` file, Tauri's base64-wrapped public key, or the bare key line.
/// Lines starting with `#` are comments; an empty result means "updates not configured".
pub fn normalize_public_key(file: &str) -> Option<String> {
    let body: String = file
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    if body.is_empty() {
        return None;
    }
    let text = unwrap_minisign_text(&body).unwrap_or(body);
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
        .filter(|l| l.len() >= 40 && base64_decode(l).map(|b| b.len() == 42).unwrap_or(false))
        .map(str::to_string)
}

// ---------------------------------------------------------------------------------------------
// Versions

/// Parses `v1.2.3`, `1.2.3`, `1.2.3-beta.1` → ((1, 2, 3), is_prerelease).
pub fn parse_version(v: &str) -> Option<((u64, u64, u64), bool)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let (core, pre) = match v.split_once(['-', '+']) {
        Some((c, rest)) => (c, !rest.is_empty() && v.as_bytes()[c.len()] == b'-'),
        None => (v, false),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(((major, minor, patch), pre))
}

/// True when `candidate` is strictly newer than `current`. A pre-release is older than the
/// release with the same number. Unparseable versions are never "newer".
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some((c, c_pre)), Some((cur, cur_pre))) = (parse_version(candidate), parse_version(current)) else {
        return false;
    };
    c > cur || (c == cur && cur_pre && !c_pre)
}

/// Civil date (UTC) for a Unix timestamp — enough to show "last checked" without a time crate.
pub fn unix_to_ymd(secs: u64) -> (i64, u32, u32) {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Keeps a version string safe for use in a file name.
pub fn sanitize_version_for_file(v: &str) -> String {
    v.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        .take(32)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// SHA-2 (needed for SSHSIG and installer hashing; kept local to avoid another dependency)

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

const K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc, 0x3956c25bf348b538,
    0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118, 0xd807aa98a3030242, 0x12835b0145706fbe,
    0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2, 0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235,
    0xc19bf174cf692694, 0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5, 0x983e5152ee66dfab,
    0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4, 0xc6e00bf33da88fc2, 0xd5a79147930aa725,
    0x06ca6351e003826f, 0x142929670a0e6e70, 0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df, 0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30, 0xd192e819d6ef5218,
    0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8, 0x19a4c116b8d2d0c8, 0x1e376c085141ab53,
    0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8, 0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3, 0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b, 0xca273eceea26619c,
    0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178, 0x06f067aa72176fba, 0x0a637dc5a2c898a6,
    0x113f9804bef90dae, 0x1b710b35131c471b, 0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c, 0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

pub fn sha512(data: &[u8]) -> [u8; 64] {
    let mut h: [u64; 8] = [
        0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
        0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u128).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 128 != 112 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.chunks(128) {
        let mut w = [0u64; 80];
        for i in 0..16 {
            let mut b = [0u8; 8];
            b.copy_from_slice(&block[8 * i..8 * i + 8]);
            w[i] = u64::from_be_bytes(b);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K512[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 64];
    for (i, v) in h.iter().enumerate() {
        out[8 * i..8 * i + 8].copy_from_slice(&v.to_be_bytes());
    }
    out
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------------------------
// Release approval: an SSH signature (SSHSIG) made with the maintainer's YubiKey (FIDO2
// `sk-ssh-ed25519@openssh.com` key) over a statement binding the version to the installer hash.
// The CI-held minisign key alone can therefore never publish an update.

/// SSHSIG namespace used for release approvals (`ssh-keygen -Y sign -n cbxshell-release`).
pub const APPROVAL_NAMESPACE: &str = "cbxshell-release";
const SK_ED25519: &str = "sk-ssh-ed25519@openssh.com";
/// FIDO flags: user presence (touch) and user verification (PIN).
const SK_FLAG_UP: u8 = 0x01;
const SK_FLAG_UV: u8 = 0x04;

/// Exact bytes the maintainer signs for a release.
pub fn approval_statement(version: &str, installer_sha256_hex: &str) -> String {
    format!("{APPROVAL_NAMESPACE}\nversion={version}\nsha256={installer_sha256_hex}\n")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApproverKey {
    /// Wire-format public key blob (compared byte-for-byte with the one inside the signature).
    pub blob: Vec<u8>,
    pub ed25519: [u8; 32],
    pub application: String,
}

/// The Ed25519 verification that remains after all structural checks passed.
#[derive(Debug, PartialEq, Eq)]
pub struct Ed25519Check {
    pub public_key: [u8; 32],
    pub message: Vec<u8>,
    pub signature: [u8; 64],
}

struct Wire<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Wire<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.i.checked_add(n).filter(|e| *e <= self.b.len()).ok_or("SSH data truncated")?;
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn string(&mut self) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        self.bytes(n)
    }
    fn done(&self) -> bool {
        self.i == self.b.len()
    }
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s);
}

fn parse_sk_ed25519_blob(blob: &[u8]) -> Result<ApproverKey, String> {
    let mut w = Wire { b: blob, i: 0 };
    if w.string()? != SK_ED25519.as_bytes() {
        return Err("approver key is not a FIDO2 (sk-ssh-ed25519) key".into());
    }
    let pk = w.string()?;
    let app = w.string()?;
    if pk.len() != 32 || !w.done() {
        return Err("malformed sk-ssh-ed25519 key".into());
    }
    let mut ed25519 = [0u8; 32];
    ed25519.copy_from_slice(pk);
    Ok(ApproverKey {
        blob: blob.to_vec(),
        ed25519,
        application: String::from_utf8_lossy(app).into_owned(),
    })
}

/// Parses `assets/release-approvers.pub`: OpenSSH public-key lines (`sk-ssh-ed25519@openssh.com
/// AAAA… comment`), optionally prefixed like an allowed_signers line. Only hardware-backed
/// sk-ssh-ed25519 keys are accepted; `#` lines are comments.
pub fn parse_approver_keys(file: &str) -> Vec<ApproverKey> {
    let mut out = Vec::new();
    for line in file.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(pos) = tokens.iter().position(|t| *t == SK_ED25519) else {
            continue;
        };
        let Some(blob) = tokens.get(pos + 1).and_then(|b| base64_decode(b)) else {
            continue;
        };
        if let Ok(k) = parse_sk_ed25519_blob(&blob) {
            if !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out
}

/// Validates an armored SSH signature over `message` (namespace, approved key, FIDO touch + PIN
/// flags) and returns the raw Ed25519 check that must still succeed.
pub fn prepare_approval_check(armored: &str, message: &[u8], approvers: &[ApproverKey]) -> Result<Ed25519Check, String> {
    let body: String = armored
        .lines()
        .map(str::trim)
        .skip_while(|l| *l != "-----BEGIN SSH SIGNATURE-----")
        .skip(1)
        .take_while(|l| *l != "-----END SSH SIGNATURE-----")
        .collect();
    let blob = base64_decode(&body).ok_or("approval: not an SSH signature")?;
    let mut w = Wire { b: &blob, i: 0 };
    if w.bytes(6)? != b"SSHSIG" || w.u32()? != 1 {
        return Err("approval: unsupported SSH signature format".into());
    }
    let pk_blob = w.string()?;
    let namespace = w.string()?;
    let reserved = w.string()?;
    let hash_alg = w.string()?;
    let sig_blob = w.string()?;
    if !w.done() {
        return Err("approval: trailing data".into());
    }
    if namespace != APPROVAL_NAMESPACE.as_bytes() {
        return Err("approval: wrong signature namespace".into());
    }
    let key = approvers
        .iter()
        .find(|k| k.blob == pk_blob)
        .ok_or("approval: signed by a key that is not an approved release key")?;
    let digest: Vec<u8> = match hash_alg {
        b"sha512" => sha512(message).to_vec(),
        b"sha256" => sha256(message).to_vec(),
        _ => return Err("approval: unsupported hash algorithm".into()),
    };
    let mut signed = b"SSHSIG".to_vec();
    put_string(&mut signed, namespace);
    put_string(&mut signed, reserved);
    put_string(&mut signed, hash_alg);
    put_string(&mut signed, &digest);

    let mut s = Wire { b: sig_blob, i: 0 };
    if s.string()? != SK_ED25519.as_bytes() {
        return Err("approval: signature type mismatch".into());
    }
    let raw = s.string()?;
    let flags = s.bytes(1)?[0];
    let counter = s.u32()?;
    if raw.len() != 64 || !s.done() {
        return Err("approval: malformed signature".into());
    }
    if flags & SK_FLAG_UP == 0 || flags & SK_FLAG_UV == 0 {
        return Err("approval: signature was made without YubiKey touch and PIN".into());
    }
    // FIDO/U2F: the authenticator signs sha256(application) ‖ flags ‖ counter ‖ sha256(data).
    let mut msg = sha256(key.application.as_bytes()).to_vec();
    msg.push(flags);
    msg.extend_from_slice(&counter.to_be_bytes());
    msg.extend_from_slice(&sha256(&signed));
    let mut signature = [0u8; 64];
    signature.copy_from_slice(raw);
    Ok(Ed25519Check { public_key: key.ed25519, message: msg, signature })
}

// ---------------------------------------------------------------------------------------------
// OpenPGP approvals: the same statement signed with a GnuPG key kept on the YubiKey's OpenPGP
// applet (`gpg --detach-sign --armor`). Approver keys are listed as
// `openpgp ssh-ed25519 AAAA… openpgp:0x…`, i.e. the output of `gpg --export-ssh-key <KEYID>!`
// prefixed with `openpgp` — key material only, no user ID / e-mail.

const OPENPGP_PREFIX: &str = "openpgp";
const PGP_ALGO_EDDSA_LEGACY: u8 = 22;
const PGP_ALGO_ED25519: u8 = 27;
const PGP_HASH_SHA256: u8 = 8;
const PGP_HASH_SHA512: u8 = 10;

/// Ed25519 keys of OpenPGP approvers (`openpgp ssh-ed25519 <blob> …` lines).
pub fn parse_openpgp_approver_keys(file: &str) -> Vec<[u8; 32]> {
    let mut out = Vec::new();
    for line in file.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 3 || tokens[0] != OPENPGP_PREFIX || tokens[1] != "ssh-ed25519" {
            continue;
        }
        let Some(blob) = base64_decode(tokens[2]) else {
            continue;
        };
        let mut w = Wire { b: &blob, i: 0 };
        let parsed = (|| -> Result<[u8; 32], String> {
            if w.string()? != b"ssh-ed25519" {
                return Err("type".into());
            }
            let pk = w.string()?;
            if pk.len() != 32 || !w.done() {
                return Err("len".into());
            }
            let mut k = [0u8; 32];
            k.copy_from_slice(pk);
            Ok(k)
        })();
        if let Ok(k) = parsed {
            if !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out
}

fn pgp_dearmor(armored: &str, kind: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN PGP {kind}-----");
    let mut lines = armored.lines().map(str::trim).skip_while(|l| *l != begin).skip(1);
    // Armor headers (e.g. "Comment: …") end at the first blank line.
    for l in lines.by_ref() {
        if l.is_empty() {
            break;
        }
    }
    let body: String = lines
        .take_while(|l| !l.starts_with("-----END") && !l.starts_with('='))
        .collect();
    base64_decode(&body)
}

/// First packet of an OpenPGP stream: (tag, body).
fn pgp_first_packet(b: &[u8]) -> Result<(u8, &[u8]), String> {
    let ctb = *b.first().ok_or("OpenPGP: empty")?;
    if ctb & 0x80 == 0 {
        return Err("OpenPGP: bad packet header".into());
    }
    let (tag, start, len) = if ctb & 0x40 != 0 {
        let o1 = *b.get(1).ok_or("OpenPGP: truncated")? as usize;
        match o1 {
            0..=191 => (ctb & 0x3F, 2, o1),
            192..=223 => {
                let o2 = *b.get(2).ok_or("OpenPGP: truncated")? as usize;
                (ctb & 0x3F, 3, ((o1 - 192) << 8) + o2 + 192)
            }
            255 => {
                let l = b.get(2..6).ok_or("OpenPGP: truncated")?;
                (ctb & 0x3F, 6, u32::from_be_bytes([l[0], l[1], l[2], l[3]]) as usize)
            }
            _ => return Err("OpenPGP: partial lengths are not supported".into()),
        }
    } else {
        let n = match ctb & 3 {
            0 => 1,
            1 => 2,
            2 => 4,
            _ => return Err("OpenPGP: indeterminate length".into()),
        };
        let l = b.get(1..1 + n).ok_or("OpenPGP: truncated")?;
        let len = l.iter().fold(0usize, |acc, &x| (acc << 8) | x as usize);
        ((ctb >> 2) & 0x0F, 1 + n, len)
    };
    let body = b.get(start..start + len).ok_or("OpenPGP: truncated packet")?;
    Ok((tag, body))
}

/// Validates an armored OpenPGP detached signature over `message` and returns one Ed25519
/// check per approver key (the signature is accepted when any of them verifies).
pub fn prepare_openpgp_approval_checks(
    armored: &str,
    message: &[u8],
    keys: &[[u8; 32]],
) -> Result<Vec<Ed25519Check>, String> {
    if keys.is_empty() {
        return Err("approval: no OpenPGP approver keys".into());
    }
    let raw = pgp_dearmor(armored, "SIGNATURE").ok_or("approval: not an OpenPGP signature")?;
    let (tag, body) = pgp_first_packet(&raw)?;
    if tag != 2 || body.first() != Some(&4) {
        return Err("approval: unsupported OpenPGP signature packet".into());
    }
    let sig_type = body.get(1).copied().ok_or("approval: truncated")?;
    let algo = body.get(2).copied().ok_or("approval: truncated")?;
    let hash_alg = body.get(3).copied().ok_or("approval: truncated")?;
    if sig_type != 0x00 {
        return Err("approval: must be a binary-document signature (no --textmode / --clearsign)".into());
    }
    let mut w = Wire { b: body, i: 4 };
    let hl = {
        let b = w.bytes(2)?;
        u16::from_be_bytes([b[0], b[1]]) as usize
    };
    w.bytes(hl)?;
    let hashed_end = w.i;
    let ul = {
        let b = w.bytes(2)?;
        u16::from_be_bytes([b[0], b[1]]) as usize
    };
    w.bytes(ul)?;
    let left16 = w.bytes(2)?;
    let mut signature = [0u8; 64];
    match algo {
        PGP_ALGO_EDDSA_LEGACY => {
            for half in 0..2 {
                let bits = {
                    let b = w.bytes(2)?;
                    u16::from_be_bytes([b[0], b[1]]) as usize
                };
                let v = w.bytes(bits.div_ceil(8))?;
                if v.len() > 32 {
                    return Err("approval: malformed EdDSA value".into());
                }
                // MPIs drop leading zero bytes; restore the fixed 32-byte width.
                signature[half * 32 + 32 - v.len()..half * 32 + 32].copy_from_slice(v);
            }
        }
        PGP_ALGO_ED25519 => signature.copy_from_slice(w.bytes(64)?),
        _ => return Err("approval: OpenPGP key is not Ed25519".into()),
    }
    if !w.done() {
        return Err("approval: trailing data in signature".into());
    }
    let mut hashed = message.to_vec();
    hashed.extend_from_slice(&body[..hashed_end]);
    hashed.extend_from_slice(&[0x04, 0xFF]);
    hashed.extend_from_slice(&(hashed_end as u32).to_be_bytes());
    let digest: Vec<u8> = match hash_alg {
        PGP_HASH_SHA256 => sha256(&hashed).to_vec(),
        PGP_HASH_SHA512 => sha512(&hashed).to_vec(),
        _ => return Err("approval: unsupported OpenPGP hash algorithm".into()),
    };
    if digest[..2] != *left16 {
        return Err("approval: signature does not match this release".into());
    }
    Ok(keys
        .iter()
        .map(|k| Ed25519Check { public_key: *k, message: digest.clone(), signature })
        .collect())
}

/// True when the approval text is an OpenPGP signature (else an SSH signature).
pub fn is_openpgp_approval(armored: &str) -> bool {
    armored.contains("-----BEGIN PGP SIGNATURE-----")
}

// ---------------------------------------------------------------------------------------------
// Manifest

pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let root = parse_json(text)?;
    let s = |v: Option<&Json>| v.and_then(Json::as_str).unwrap_or("").to_string();
    let version = s(root.get("version"));
    if parse_version(&version).is_none() {
        return Err(format!("manifest: invalid version '{version}'"));
    }
    let platform = root
        .get("platforms")
        .and_then(|p| p.get(PLATFORM_KEY))
        .unwrap_or(&root);
    let url = s(platform.get("url"));
    let sig_raw = s(platform.get("signature"));
    if !url.starts_with(ALLOWED_URL_PREFIX)
        || url.len() > 512
        || url.contains(['\\', ' ', '?', '#', '@'])
        || url[ALLOWED_URL_PREFIX.len()..].contains("..")
    {
        return Err("manifest: installer URL is not a release asset of this repository".into());
    }
    let signature = unwrap_minisign_text(&sig_raw).ok_or("manifest: missing or malformed signature")?;
    Ok(Manifest {
        version,
        notes: s(root.get("notes")).chars().take(2000).collect(),
        pub_date: s(root.get("pub_date")),
        url,
        signature,
        approval: s(platform.get("approval")).chars().take(8192).collect(),
    })
}

/// Splits an allowed release URL into (host, path) for the HTTP client.
pub fn split_https_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let slash = rest.find('/')?;
    Some((&rest[..slash], &rest[slash..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIG_TEXT: &str = "untrusted comment: signature from tauri secret key\nRUTn1bKN7SRXXV8l2lVahGqU+X3XrMxOV6v5+hT8b1t8Ul6l3HUD4Y2Ra0y1qCpS+SSG+FjYHQcf0Es3kj1uYqgM6sl3Q5j7oA0=\ntrusted comment: timestamp:1700000000\tfile:CBXShell-Setup.exe\n0Y3n2LL2X2E7+H4b0N4y1nuE3sX9ZJmD1ZIlMt7S6uY0eJpS1f1u2u8Fv6J9b0yQYxqC9A3Z1dG2L+7rzV6tCw==";

    fn b64(s: &str) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let b = s.as_bytes();
        let mut out = String::new();
        for c in b.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            out.push(A[(n >> 18) as usize & 63] as char);
            out.push(A[(n >> 12) as usize & 63] as char);
            out.push(if c.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
            out.push(if c.len() > 2 { A[n as usize & 63] as char } else { '=' });
        }
        out
    }

    #[test]
    fn base64_round_trip() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar", SIG_TEXT] {
            assert_eq!(base64_decode(&b64(s)).unwrap(), s.as_bytes());
        }
        assert!(base64_decode("A").is_none());
        assert!(base64_decode("AB$=").is_none());
    }

    #[test]
    fn versions_compare() {
        assert!(is_newer("2.0.2", "2.0.1"));
        assert!(is_newer("v2.1.0", "2.0.9"));
        assert!(is_newer("2.0.1", "2.0.1-beta.1"));
        assert!(!is_newer("2.0.1-beta.2", "2.0.1"));
        assert!(!is_newer("2.0.1", "2.0.1"));
        assert!(!is_newer("1.9.9", "2.0.1"));
        assert!(!is_newer("garbage", "2.0.1"));
        assert!(!is_newer("2.0.1.4", "2.0.1"));
        assert_eq!(parse_version("3"), Some(((3, 0, 0), false)));
    }

    fn manifest_json(url: &str, sig: &str) -> String {
        format!(
            r#"{{"version":"2.0.2","notes":"Fixes \"RAR\" reads\n\u4e2d","pub_date":"2026-11-20T00:00:00Z",
               "platforms":{{"windows-x86_64":{{"signature":"{sig}","url":"{url}"}}}}}}"#
        )
    }

    #[test]
    fn manifest_parses_tauri_shape() {
        let url = "https://github.com/ppundsh/cbxshell-rs/releases/download/v2.0.2/CBXShell-Setup.exe";
        let m = parse_manifest(&manifest_json(url, &b64(SIG_TEXT))).unwrap();
        assert_eq!(m.version, "2.0.2");
        assert_eq!(m.url, url);
        assert_eq!(m.signature, SIG_TEXT);
        assert_eq!(m.notes, "Fixes \"RAR\" reads\n中");
        assert_eq!(split_https_url(&m.url), Some(("github.com", "/ppundsh/cbxshell-rs/releases/download/v2.0.2/CBXShell-Setup.exe")));
    }

    #[test]
    fn manifest_rejects_foreign_urls() {
        let sig = b64(SIG_TEXT);
        for url in [
            "https://evil.example/CBXShell-Setup.exe",
            "http://github.com/ppundsh/cbxshell-rs/releases/download/v2/CBXShell-Setup.exe",
            "https://github.com/ppundsh/cbxshell-rs/releases/download/../../other/x.exe",
            "https://github.com/ppundsh/cbxshell-rs/releases/download/v2/x.exe?redirect=evil",
            "https://github.com/someone/cbxshell-rs/releases/download/v2/CBXShell-Setup.exe",
        ] {
            assert!(parse_manifest(&manifest_json(url, &sig)).is_err(), "{url}");
        }
    }

    #[test]
    fn manifest_requires_signature() {
        let url = "https://github.com/ppundsh/cbxshell-rs/releases/download/v2.0.2/CBXShell-Setup.exe";
        assert!(parse_manifest(&manifest_json(url, "")).is_err());
        assert!(parse_manifest(&manifest_json(url, "bm90IGEgc2lnbmF0dXJl")).is_err());
    }

    #[test]
    fn public_key_forms() {
        let key = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let file = format!("untrusted comment: minisign public key: 1234\n{key}\n");
        assert_eq!(normalize_public_key(&file).as_deref(), Some(key));
        assert_eq!(normalize_public_key(&b64(&file)).as_deref(), Some(key));
        assert_eq!(normalize_public_key(key).as_deref(), Some(key));
        assert_eq!(normalize_public_key("# not configured\n\n"), None);
        assert_eq!(normalize_public_key("RWQshort"), None);
    }

    // Vector generated with Python `cryptography` following PROTOCOL.sshsig / PROTOCOL.u2f:
    // a software Ed25519 key standing in for the YubiKey, application "ssh:github", flags 0x05.
    const APPROVER: &str = "sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAIAOhB7/zzhC+HXDdGOdLwJln5NYwm6UNXx3chmQSVTG4AAAACnNzaDpnaXRodWI= me@example (YubiKey)";
    const ARMORED: &str = "-----BEGIN SSH SIGNATURE-----\nU1NIU0lHAAAAAQAAAFAAAAAac2stc3NoLWVkMjU1MTlAb3BlbnNzaC5jb20AAAAgA6EHv/\nPOEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbgAAAAKc3NoOmdpdGh1YgAAABBjYnhzaGVs\nbC1yZWxlYXNlAAAAAAAAAAZzaGE1MTIAAABnAAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2\nguY29tAAAAQP2uabnEmNCF+Of6ivMcvYzHSFlGzRiiiddWuVFbpuTcM/isah8yfpv+3yiv\nPrl2IJgWCp6FwcpLpuvLKw76kQ0FAAAABw==\n-----END SSH SIGNATURE-----\n";

    #[test]
    fn sha2_matches_reference() {
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hex(&sha512(b"abc")), "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f");
        let x: Vec<u8> = (0..5).flat_map(|_| 0u8..=255).collect();
        assert_eq!(hex(&sha256(&x)), "d414b085826eb06778483ba35564dc849e643359f69ed9747878ba6e54985bed");
        assert_eq!(hex(&sha512(&x)), "c93f55ccf2fa8c82699ff9b58afe3591242b135d908a6d865e17e38adb41c21d1d5359e51273036373d54d20b5659cc87e6e7b381ff027d33f971416cc590f90");
    }

    #[test]
    fn approval_signature_is_prepared_per_spec() {
        let keys = parse_approver_keys(&format!("# approvers\n{APPROVER}\n"));
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].application, "ssh:github");
        let stmt = approval_statement("2.0.2", &hex(&sha256(b"installer")));
        let check = prepare_approval_check(ARMORED, stmt.as_bytes(), &keys).unwrap();
        assert_eq!(hex(&check.public_key), "03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8");
        assert_eq!(hex(&check.message), "3e861c3042e923d26120ac735d2aa1a9bd94473fb1f583747ab76e0b63e1cff305000000071edd07bd6bef0020d2d2f20f2e3ee3c863ac46f056bbbe73d72a65cf6ea4ab4e");
        assert_eq!(hex(&check.signature), "fdae69b9c498d085f8e7fa8af31cbd8cc7485946cd18a289d756b9515ba6e4dc33f8ac6a1f327e9bfedf28af3eb9762098160a9e85c1ca4ba6ebcb2b0efa910d");
    }

    #[test]
    fn approval_rejects_other_keys_and_statements() {
        let keys = parse_approver_keys(APPROVER);
        // A different installer hash changes the message, so the Ed25519 check would fail.
        let other = approval_statement("2.0.2", &hex(&sha256(b"evil")));
        let c = prepare_approval_check(ARMORED, other.as_bytes(), &keys).unwrap();
        assert_ne!(hex(&c.message), "3e861c3042e923d26120ac735d2aa1a9bd94473fb1f583747ab76e0b63e1cff305000000071edd07bd6bef0020d2d2f20f2e3ee3c863ac46f056bbbe73d72a65cf6ea4ab4e");
        // Not an approved key.
        assert!(prepare_approval_check(ARMORED, b"x", &[]).is_err());
        // Software (non-FIDO) keys are never accepted as approvers.
        assert!(parse_approver_keys("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl x").is_empty());
        assert!(prepare_approval_check("garbage", b"x", &keys).is_err());
    }

    // Real GnuPG 2.4 output: ed25519 primary key (SHA-512 and SHA-256 digests) and an ed25519
    // signing subkey, signing approval_statement("2.0.2", sha256("installer")).
    const PGP_APPROVERS: &str = "openpgp ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB/6e3P+4dWrvWYmeWJUbWZt2u6i6runPzdqJTeDphQx openpgp:0x19993E22\nopenpgp ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINfO1Qo/pIi8oYb1aFyysoq6BbtrtmZGRFJ8PRY8zxeP openpgp:0x2B32EC6D\n";
    const PGP_PRIMARY_SHA512: &str = "-----BEGIN PGP SIGNATURE-----\n\niHUEABYKAB0WIQTkAlC9xSm/B5J4fLGn81zbGZk+IgUCav2U1QAKCRCn81zbGZk+\nIq1tAP9f4ieJcp4uwEUfVjb7RZR335eZcJ2s5vRPhNud9/DW5wEA414j/trm6aww\nnpAjlBLUzNOq9bCZtxEV8YNk5RXaFwM=\n=VMGX\n-----END PGP SIGNATURE-----\n";
    const PGP_SUBKEY: &str = "-----BEGIN PGP SIGNATURE-----\n\niHUEABYKAB0WIQTyZ/amCRh3t1J/Lix0yNJKKzLsbQUCav2U1QAKCRB0yNJKKzLs\nbXubAQD45kbPfosPulis1eXgFjZghKXjpNf8USVV/roPIBSD6gEA3HPcB+nWtp2r\nEvutJTy42eRyQBsAGgc35tLi/Rcu8gI=\n=5Hac\n-----END PGP SIGNATURE-----\n";
    const PGP_PRIMARY_SHA256: &str = "-----BEGIN PGP SIGNATURE-----\n\niHUEABYIAB0WIQTkAlC9xSm/B5J4fLGn81zbGZk+IgUCav2U1QAKCRCn81zbGZk+\nIuxBAQCul0QYLIHcBrhGH/TEPZWBkm22z1XDm7yBhO3+tXhLwgD8Cp6ecMCTbTN0\nmTQuQCZgyoU9OXZrzMusCrnGxByzqgA=\n=A74z\n-----END PGP SIGNATURE-----\n";

    #[test]
    fn openpgp_approval_matches_gnupg() {
        let keys = parse_openpgp_approver_keys(PGP_APPROVERS);
        assert_eq!(keys.len(), 2);
        assert_eq!(hex(&keys[0]), "1ffa7b73fee1d5abbd66267962546d666ddaeea2eabba73f376a253783a61431");
        // Software SSH keys without the `openpgp` prefix are not OpenPGP approvers.
        assert!(parse_openpgp_approver_keys("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB/6e3P+4dWrvWYmeWJUbWZt2u6i6runPzdqJTeDphQx x").is_empty());
        let stmt = approval_statement("2.0.2", &hex(&sha256(b"installer")));
        for (armored, digest, sig) in [
            (PGP_PRIMARY_SHA512, "ad6d605a5e7ab4440317bd5e5d6f32f2272bd96684e5d47353d9cb41c988d85dca18ab23573669e86b9dd3f0bbeb9dde244cfd172c2feab05307a905b2a28462", "5fe22789729e2ec0451f5636fb459477df9799709dace6f44f84db9df7f0d6e7e35e23fedae6e9ac309e90239412d4ccd3aaf5b099b71115f18364e515da1703"),
            (PGP_SUBKEY, "7b9b22e1b66082539ba8e009421490cbda0a1a43568b65ecd02c7ea6a4184e0135a92a90437b7ad5f6044eec0bfe9ba82a2556a3a21be11b1c91f2d8036ddf20", "f8e646cf7e8b0fba58acd5e5e016366084a5e3a4d7fc512555feba0f201483eadc73dc07e9d6b69dab12fbad253cb8d9e472401b001a0737e6d2e2fd172ef202"),
            (PGP_PRIMARY_SHA256, "ec417505556c5565292e656421f78eac274b32520da69903a75dd6c6d5b739bf", "ae9744182c81dc06b8461ff4c43d9581926db6cf55c39bbc8184edfeb5784bc20a9e9e70c0936d337499342e402660ca853d39766bcccbac0ab9c6c41cb3aa00"),
        ] {
            assert!(is_openpgp_approval(armored));
            let checks = prepare_openpgp_approval_checks(armored, stmt.as_bytes(), &keys).unwrap();
            assert_eq!(checks.len(), 2);
            assert_eq!(hex(&checks[0].message), digest);
            assert_eq!(hex(&checks[0].signature), sig);
        }
        // Another installer hash no longer matches the digest prefix stored in the signature.
        let other = approval_statement("2.0.2", &hex(&sha256(b"evil")));
        assert!(prepare_openpgp_approval_checks(PGP_PRIMARY_SHA512, other.as_bytes(), &keys).is_err());
        assert!(prepare_openpgp_approval_checks(PGP_PRIMARY_SHA512, stmt.as_bytes(), &[]).is_err());
    }

    #[test]
    fn dates() {
        assert_eq!(unix_to_ymd(0), (1970, 1, 1));
        assert_eq!(unix_to_ymd(951_782_400), (2000, 2, 29));
        assert_eq!(unix_to_ymd(1_795_392_000), (2026, 11, 23));
    }

    #[test]
    fn json_rejects_malformed() {
        for bad in ["{", "{\"a\":}", "[1,]", "\"\\x\"", "{} x", "[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]]"] {
            assert!(parse_json(bad).is_err(), "{bad}");
        }
    }
}
