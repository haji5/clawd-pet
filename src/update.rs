//! "Check for updates" (PLAN.md §7, "Updating"): read the latest GitHub
//! release, decide whether it's newer, and fetch and verify its exe.
//!
//! Pure: the HTTP call comes in as a closure (`win::http::get` in the pet, a
//! fake in tests), so the whole check and download run in unit tests. Only
//! the pet calls this, and only after the user's click; the hook never does.
//!
//! The hash catches a truncated or corrupted download, or the wrong file. It
//! doesn't prove who built the exe (it comes from the same release): TLS to
//! GitHub is what vouches for the source.

use serde::Deserialize;
use std::cmp::Ordering;
use std::fmt;

// Unused by a debug build outside tests: it never asks GitHub (`setup`).
#[cfg_attr(debug_assertions, allow(dead_code))]
pub const REPO: &str = "haji5/clawd-pet";
pub const ASSET: &str = "clawd-pet.exe";
/// `<64 hex> *clawd-pet.exe`, the fallback when the API gives no `digest`.
const SHA_ASSET: &str = "clawd-pet.exe.sha256";
/// The release JSON is about 5 KB.
const API_CAP: usize = 1 << 20;
const SHA_CAP: usize = 1024;
/// What an exe of Clawd can plausibly weigh.
const MIN_EXE: u64 = 64 * 1024;
const MAX_EXE: u64 = 16 << 20;

pub const FORM: &str = "The latest release on GitHub isn't in the form Clawd expects, so it wasn't installed.";
pub const MISMATCH: &str = "The download didn't match the release's checksum, so it wasn't installed. Try again.";
// Only debug builds use these two (`setup`).
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub const DEBUG_BUILD: &str = "This is a debug build, so it doesn't update itself.";

/// `major.minor.patch`, nothing more.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// `v0.1.2` or `0.1.2`. Anything else (`v1.2`, `1.2.3-rc1`, `latest`)
    /// is `None`, not a guess.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut numbers = [0u32; 3];
        let mut parts = s.split('.');
        for n in &mut numbers {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            *n = part.parse().ok()?;
        }
        parts.next().is_none().then_some(Version(numbers[0], numbers[1], numbers[2]))
    }

    /// This build's version, from `Cargo.toml`.
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version(0, 0, 0))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// Where to ask, and where downloads may come from.
#[derive(Clone, Debug)]
pub struct Source {
    pub api: String,
    pub download_prefix: String,
}

impl Source {
    #[cfg_attr(debug_assertions, allow(dead_code))]
    pub fn github() -> Source {
        Source {
            api: format!("https://api.github.com/repos/{REPO}/releases/latest"),
            download_prefix: format!("https://github.com/{REPO}/releases/download/"),
        }
    }

    /// A test server on this machine (`http://127.0.0.1:<port>/…`), which
    /// then serves the downloads too.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub fn local(api: &str) -> Option<Source> {
        let rest = api.strip_prefix("http://127.0.0.1:")?;
        let port = &rest[..rest.find('/').unwrap_or(rest.len())];
        port.parse::<u16>().ok()?;
        Some(Source { api: api.to_string(), download_prefix: format!("http://127.0.0.1:{port}/") })
    }
}

/// Where this Clawd looks, and the version it counts as. Debug builds don't
/// update themselves, unless a test points them at its own server
/// (`CLAWD_PET_UPDATE_URL`, and `CLAWD_PET_UPDATE_AS` for the version).
pub fn setup() -> Result<(Source, Version), String> {
    #[cfg(debug_assertions)]
    {
        let source = std::env::var("CLAWD_PET_UPDATE_URL").ok().and_then(|u| Source::local(&u)).ok_or(DEBUG_BUILD)?;
        let current = std::env::var("CLAWD_PET_UPDATE_AS").ok().and_then(|v| Version::parse(&v)).unwrap_or_else(Version::current);
        Ok((source, current))
    }
    #[cfg(not(debug_assertions))]
    Ok((Source::github(), Version::current()))
}

/// How the expected hash is known.
#[derive(Clone, Debug, PartialEq)]
pub enum Sha {
    /// From the asset's `digest`.
    Known([u8; 32]),
    /// The URL of the `.sha256` asset.
    InFile(String),
}

/// A release newer than this Clawd, checked as far as the JSON can be.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    pub url: String,
    pub size: u64,
    pub sha: Sha,
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    UpToDate,
    /// This Clawd is newer than the latest release (a local build): its version.
    Ahead(Version),
    Newer(Release),
}

/// What an HTTP fetch can fail with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FetchError {
    /// The server answered, but not with 200.
    Status(u32),
    /// A WinHTTP or system error code.
    Os(u32),
    /// More bytes than the cap.
    TooBig,
    /// Not a URL Clawd fetches.
    BadUrl,
}

impl FetchError {
    /// For a message box: what failed, and what to do about it.
    pub fn text(self) -> String {
        match self {
            FetchError::Os(12007 | 12029) => "Could not reach GitHub. Check your internet connection, then try again.".into(),
            FetchError::Os(12002) => "GitHub took too long to answer. Try again in a moment.".into(),
            FetchError::Os(12175) => "The secure connection to GitHub failed, so nothing was downloaded.".into(),
            FetchError::Os(n) => format!("Could not reach GitHub (error {n})."),
            FetchError::Status(403 | 429) => "GitHub is limiting requests from this network right now. Try again in an hour.".into(),
            FetchError::Status(404) => "No release was found on GitHub.".into(),
            FetchError::Status(n) => format!("GitHub answered with an error (HTTP {n})."),
            FetchError::TooBig | FetchError::BadUrl => FORM.into(),
        }
    }
}

/// The HTTP GET: a URL and the most bytes to accept.
pub type Fetch<'a> = &'a dyn Fn(&str, usize) -> Result<Vec<u8>, FetchError>;

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: Option<String>,
    browser_download_url: String,
}

/// A download URL from the release: under the source's prefix, and nothing
/// that could lead the request anywhere else.
fn trusted(url: &str, source: &Source) -> bool {
    url.starts_with(&source.download_prefix) && !url.contains("..") && url.bytes().all(|b| b.is_ascii_graphic())
}

/// Exactly 64 hex digits.
fn hex32(s: &[u8]) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let digit = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = [0u8; 32];
    for (i, pair) in s.chunks(2).enumerate() {
        out[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(out)
}

/// The hash in a `.sha256` file: its first 64 characters, which must be hex
/// and end there.
fn sha_in_file(bytes: &[u8]) -> Option<[u8; 32]> {
    let hash = hex32(bytes.get(..64)?)?;
    bytes.get(64).is_none_or(u8::is_ascii_whitespace).then_some(hash)
}

/// Read the release JSON: the tag's version, then (only for a newer one,
/// so a release Clawd already has never fails the check) the exe asset.
fn read(json: &[u8], source: &Source, current: Version) -> Result<Verdict, String> {
    let release: ApiRelease = serde_json::from_slice(json).map_err(|_| FORM)?;
    let version = Version::parse(&release.tag_name).ok_or(FORM)?;
    match version.cmp(&current) {
        Ordering::Equal => return Ok(Verdict::UpToDate),
        Ordering::Less => return Ok(Verdict::Ahead(version)),
        Ordering::Greater => {}
    }
    let exe = release.assets.iter().find(|a| a.name == ASSET).ok_or(FORM)?;
    if !trusted(&exe.browser_download_url, source) || !(MIN_EXE..=MAX_EXE).contains(&exe.size) {
        return Err(FORM.into());
    }
    let digest = exe.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")).and_then(|h| hex32(h.as_bytes()));
    let sha = match digest {
        Some(hash) => Sha::Known(hash),
        None => {
            let file = release.assets.iter().find(|a| a.name == SHA_ASSET && trusted(&a.browser_download_url, source)).ok_or(FORM)?;
            Sha::InFile(file.browser_download_url.clone())
        }
    };
    Ok(Verdict::Newer(Release { version, url: exe.browser_download_url.clone(), size: exe.size, sha }))
}

/// Ask for the latest release and compare it with `current`.
pub fn check(fetch: Fetch, source: &Source, current: Version) -> Result<Verdict, String> {
    let json = fetch(&source.api, API_CAP).map_err(FetchError::text)?;
    read(&json, source, current)
}

/// Fetch the release's exe and check it: its length, `MZ`, its SHA-256.
pub fn download(fetch: Fetch, release: &Release) -> Result<Vec<u8>, String> {
    // The small file first: a bad one is found before 800 KB come down.
    let want = match &release.sha {
        Sha::Known(hash) => *hash,
        Sha::InFile(url) => sha_in_file(&fetch(url, SHA_CAP).map_err(FetchError::text)?).ok_or(FORM)?,
    };
    let bytes = fetch(&release.url, release.size as usize).map_err(|e| match e {
        FetchError::TooBig => MISMATCH.to_string(),
        e => e.text(),
    })?;
    if bytes.len() as u64 != release.size || !bytes.starts_with(b"MZ") || sha256(&bytes) != want {
        return Err(MISMATCH.into());
    }
    Ok(bytes)
}

// ---- what the boxes say ----

pub fn up_to_date_text(current: Version) -> String {
    format!("Clawd is up to date (version {current}).")
}

pub fn ahead_text(current: Version, latest: Version) -> String {
    format!("This Clawd ({current}) is newer than the latest release ({latest}).")
}

pub fn ask_text(current: Version, new: Version) -> String {
    format!("Clawd {new} is available. You have {current}.\n\nUpdate now? Clawd will restart.")
}

pub fn updated_text(old: &str, new: Version, hooks_updated: bool) -> String {
    let mut text = format!("Clawd was updated from {old} to {new}.");
    if hooks_updated {
        text.push_str("\n\nIts Claude hooks were updated too.");
    }
    text
}

// ---- URLs, for win/http.rs ----

#[derive(Debug, PartialEq)]
pub struct Url<'a> {
    pub secure: bool,
    pub host: &'a str,
    pub port: u16,
    /// With the query, if any; `/` at least.
    pub path: &'a str,
}

/// `https://host[:port]/path`. Plain `http` only to `127.0.0.1`, for tests.
pub fn split_url(url: &str) -> Result<Url<'_>, FetchError> {
    let (secure, rest) = match url.split_once("://") {
        Some(("https", rest)) => (true, rest),
        Some(("http", rest)) => (false, rest),
        _ => return Err(FetchError::BadUrl),
    };
    let (authority, path) = rest.find('/').map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, port.parse().map_err(|_| FetchError::BadUrl)?),
        None => (authority, if secure { 443 } else { 80 }),
    };
    let host_ok = !host.is_empty() && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    if !host_ok || port == 0 || (!secure && host != "127.0.0.1") {
        return Err(FetchError::BadUrl);
    }
    Ok(Url { secure, host, port, path })
}

// ---- SHA-256 (FIPS 180-4) ----

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
    0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
    0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
    0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
    0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
    0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn compress(h: &mut [u32; 8], block: &[u8]) {
    let mut w = [0u32; 64];
    for (i, word) in block.chunks(4).enumerate() {
        w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        (hh, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
    }
    for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *x = x.wrapping_add(y);
    }
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let (blocks, rest) = bytes.as_chunks::<64>();
    for block in blocks {
        compress(&mut h, block);
    }
    // The rest, a 1 bit, zeros, and the length in bits: one or two blocks.
    let mut tail = [0u8; 128];
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let len = if rest.len() < 56 { 64 } else { 128 };
    tail[len - 8..len].copy_from_slice(&((bytes.len() as u64) * 8).to_be_bytes());
    for block in tail[..len].chunks(64) {
        compress(&mut h, block);
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    const LATEST: &[u8] = include_bytes!("../tests/fixtures/update/latest.json");
    const V011_SHA: &str = "9e23c9aec018708ae31019545d3ae986a1fe04c90126d066f7d3156982233cd6";
    const EXE_URL: &str = "https://github.com/haji5/clawd-pet/releases/download/v0.1.1/clawd-pet.exe";
    const SHA_URL: &str = "https://github.com/haji5/clawd-pet/releases/download/v0.1.1/clawd-pet.exe.sha256";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    /// The real v0.1.1 answer, changed by `edit`.
    fn latest(edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
        let mut json: serde_json::Value = serde_json::from_slice(LATEST).unwrap();
        edit(&mut json);
        serde_json::to_vec(&json).unwrap()
    }

    fn read_as_older(json: &[u8]) -> Result<Verdict, String> {
        read(json, &Source::github(), v("0.1.0"))
    }

    /// A fake server: URL → answer. Records what was asked for, with the cap.
    struct Fake {
        answers: HashMap<String, Result<Vec<u8>, FetchError>>,
        asked: RefCell<Vec<(String, usize)>>,
    }

    impl Fake {
        fn new(answers: &[(&str, Result<Vec<u8>, FetchError>)]) -> Fake {
            Fake { answers: answers.iter().map(|(u, a)| (u.to_string(), a.clone())).collect(), asked: RefCell::default() }
        }

        fn get(&self, url: &str, cap: usize) -> Result<Vec<u8>, FetchError> {
            self.asked.borrow_mut().push((url.to_string(), cap));
            let answer = self.answers.get(url).cloned().unwrap_or(Err(FetchError::Status(404)))?;
            if answer.len() > cap { Err(FetchError::TooBig) } else { Ok(answer) }
        }
    }

    #[test]
    fn versions() {
        assert_eq!(Version::parse("v0.1.2"), Some(Version(0, 1, 2)));
        assert_eq!(Version::parse("0.1.2"), Some(Version(0, 1, 2)));
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.1.10") > v("0.1.9"));
        for bad in ["v1.2", "v1.2.3-rc1", "latest", "", "v", "1.2.3.4", "1..3", "1.2.+3", " 1.2.3", "vv1.2.3"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(v("v3.20.1").to_string(), "3.20.1");
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn sha256_matches_the_published_vectors() {
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(&sha256(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu")),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
        assert_eq!(hex(&sha256(&vec![b'a'; 1_000_000])), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn the_real_release_reads_as_newer_than_an_older_clawd() {
        let Ok(Verdict::Newer(r)) = read_as_older(LATEST) else { panic!() };
        assert_eq!(r.version, v("0.1.1"));
        assert_eq!(r.url, EXE_URL);
        assert_eq!(r.size, 771_072);
        assert_eq!(r.sha, Sha::Known(hex32(V011_SHA.as_bytes()).unwrap()));
    }

    #[test]
    fn the_same_or_an_older_release_is_not_newer() {
        assert_eq!(read(LATEST, &Source::github(), v("0.1.1")), Ok(Verdict::UpToDate));
        assert_eq!(read(LATEST, &Source::github(), v("0.2.0")), Ok(Verdict::Ahead(v("0.1.1"))));
        // Not newer: the assets don't matter then.
        let no_assets = latest(|j| j["assets"] = serde_json::json!([]));
        assert_eq!(read(&no_assets, &Source::github(), v("0.1.1")), Ok(Verdict::UpToDate));
    }

    #[test]
    fn a_release_in_another_form_is_refused() {
        let form = Err(FORM.to_string());
        assert_eq!(read_as_older(b"not json"), form);
        assert_eq!(read_as_older(&latest(|j| j["tag_name"] = "v0.2".into())), form);
        assert_eq!(read_as_older(&latest(|j| j["tag_name"] = "v0.2.0-rc1".into())), form);
        assert_eq!(read_as_older(&latest(|j| j["assets"][0]["name"] = "clawd.exe".into())), form, "no exe asset");
        for url in [
            "https://evil.example/haji5/clawd-pet/releases/download/v0.1.1/clawd-pet.exe",
            "https://github.com/someone/clawd-pet/releases/download/v0.1.1/clawd-pet.exe",
            "http://github.com/haji5/clawd-pet/releases/download/v0.1.1/clawd-pet.exe",
            "https://github.com/haji5/clawd-pet/releases/download/../../../x/clawd-pet.exe",
            "https://github.com/haji5/clawd-pet/releases/download/v0.1.1/clawd pet.exe",
        ] {
            assert_eq!(read_as_older(&latest(|j| j["assets"][0]["browser_download_url"] = url.into())), form, "{url}");
        }
        for size in [0, 1000, 17 << 20] {
            assert_eq!(read_as_older(&latest(|j| j["assets"][0]["size"] = size.into())), form, "{size}");
        }
        let neither = latest(|j| {
            j["assets"][0]["digest"] = serde_json::Value::Null;
            j["assets"][1]["name"] = "notes.txt".into();
        });
        assert_eq!(read_as_older(&neither), form, "no hash at all");
        let foreign_sha = latest(|j| {
            j["assets"][0].as_object_mut().unwrap().remove("digest");
            j["assets"][1]["browser_download_url"] = "https://evil.example/clawd-pet.exe.sha256".into();
        });
        assert_eq!(read_as_older(&foreign_sha), form, "hash file elsewhere");
    }

    #[test]
    fn without_a_digest_the_hash_comes_from_the_sha256_asset() {
        for digest in [serde_json::Value::Null, "md5:abc".into(), "sha256:123".into()] {
            let json = latest(|j| j["assets"][0]["digest"] = digest.clone());
            let Ok(Verdict::Newer(r)) = read_as_older(&json) else { panic!("{digest}") };
            assert_eq!(r.sha, Sha::InFile(SHA_URL.into()));
        }
    }

    #[test]
    fn check_compares_versions_and_explains_failures() {
        let source = Source::github();
        let fake = Fake::new(&[(source.api.as_str(), Ok(LATEST.to_vec()))]);
        let get = |u: &str, c: usize| fake.get(u, c);
        assert!(matches!(check(&get, &source, v("0.1.0")), Ok(Verdict::Newer(_))));
        assert_eq!(check(&get, &source, v("0.1.1")), Ok(Verdict::UpToDate));
        assert_eq!(check(&get, &source, v("1.0.0")), Ok(Verdict::Ahead(v("0.1.1"))));
        assert_eq!(fake.asked.borrow()[0], (source.api.clone(), API_CAP));

        for (error, text) in [
            (FetchError::Os(12007), "Could not reach GitHub. Check your internet connection, then try again."),
            (FetchError::Os(12029), "Could not reach GitHub. Check your internet connection, then try again."),
            (FetchError::Os(12002), "GitHub took too long to answer. Try again in a moment."),
            (FetchError::Os(12175), "The secure connection to GitHub failed, so nothing was downloaded."),
            (FetchError::Os(5), "Could not reach GitHub (error 5)."),
            (FetchError::Status(403), "GitHub is limiting requests from this network right now. Try again in an hour."),
            (FetchError::Status(429), "GitHub is limiting requests from this network right now. Try again in an hour."),
            (FetchError::Status(404), "No release was found on GitHub."),
            (FetchError::Status(500), "GitHub answered with an error (HTTP 500)."),
            (FetchError::TooBig, FORM),
            (FetchError::BadUrl, FORM),
        ] {
            let failing = |_: &str, _: usize| Err(error);
            assert_eq!(check(&failing, &source, v("0.1.0")), Err(text.to_string()), "{error:?}");
        }
    }

    /// An exe-like file and the release that describes it.
    fn exe_and_release(sha: Option<Sha>) -> (Vec<u8>, Release) {
        let mut exe = b"MZ".to_vec();
        exe.extend((0..70_000u32).map(|i| (i * 7 % 251) as u8));
        let sha = sha.unwrap_or(Sha::Known(sha256(&exe)));
        let release = Release { version: v("0.2.0"), url: EXE_URL.into(), size: exe.len() as u64, sha };
        (exe, release)
    }

    #[test]
    fn download_accepts_only_the_described_bytes() {
        let (exe, release) = exe_and_release(None);
        let serve = |bytes: Vec<u8>| Fake::new(&[(EXE_URL, Ok(bytes))]);

        let good = serve(exe.clone());
        assert_eq!(download(&|u: &str, c: usize| good.get(u, c), &release), Ok(exe.clone()));
        assert_eq!(good.asked.borrow()[..], [(EXE_URL.to_string(), exe.len())], "capped at the release's size");

        let mismatch = Err(MISMATCH.to_string());
        let short = serve(exe[..exe.len() - 1].to_vec());
        assert_eq!(download(&|u: &str, c: usize| short.get(u, c), &release), mismatch, "one byte short");
        let mut long = exe.clone();
        long.push(0);
        let long = serve(long);
        assert_eq!(download(&|u: &str, c: usize| long.get(u, c), &release), mismatch, "one byte over");
        let mut flipped = exe.clone();
        flipped[1000] ^= 1;
        let flipped = serve(flipped);
        assert_eq!(download(&|u: &str, c: usize| flipped.get(u, c), &release), mismatch, "wrong hash");

        let mut not_exe = exe.clone();
        not_exe[0] = b'P';
        let (_, mut r) = exe_and_release(None);
        r.sha = Sha::Known(sha256(&not_exe));
        let not_exe = serve(not_exe);
        assert_eq!(download(&|u: &str, c: usize| not_exe.get(u, c), &r), mismatch, "no MZ");

        let offline = |_: &str, _: usize| Err(FetchError::Os(12029));
        assert_eq!(download(&offline, &release), Err(FetchError::Os(12029).text()));
    }

    #[test]
    fn download_can_take_the_hash_from_the_sha256_file() {
        let (exe, release) = exe_and_release(Some(Sha::InFile(SHA_URL.into())));
        // The real file's format: `<hex> *clawd-pet.exe\n`.
        let file = format!("{} *clawd-pet.exe\n", hex(&sha256(&exe)));
        let fake = Fake::new(&[(EXE_URL, Ok(exe.clone())), (SHA_URL, Ok(file.into_bytes()))]);
        assert_eq!(download(&|u: &str, c: usize| fake.get(u, c), &release), Ok(exe.clone()));
        assert_eq!(fake.asked.borrow()[0], (SHA_URL.to_string(), SHA_CAP), "the small file first");

        let wrong = format!("{} *clawd-pet.exe\n", hex(&sha256(b"other")));
        let fake = Fake::new(&[(EXE_URL, Ok(exe.clone())), (SHA_URL, Ok(wrong.into_bytes()))]);
        assert_eq!(download(&|u: &str, c: usize| fake.get(u, c), &release), Err(MISMATCH.to_string()));

        let junk = Fake::new(&[(EXE_URL, Ok(exe)), (SHA_URL, Ok(b"<html>not found</html>".to_vec()))]);
        assert_eq!(download(&|u: &str, c: usize| junk.get(u, c), &release), Err(FORM.to_string()));
        assert_eq!(junk.asked.borrow().len(), 1, "no exe download after a bad hash file");
    }

    #[test]
    fn hash_files() {
        let h = "9e23c9aec018708ae31019545d3ae986a1fe04c90126d066f7d3156982233cd6";
        assert!(sha_in_file(format!("{h} *clawd-pet.exe\n").as_bytes()).is_some());
        assert!(sha_in_file(h.as_bytes()).is_some());
        assert!(sha_in_file(h.to_uppercase().as_bytes()).is_some());
        assert!(sha_in_file(format!("{h}0").as_bytes()).is_none(), "65 hex digits");
        assert!(sha_in_file(&h.as_bytes()[..63]).is_none());
        assert!(sha_in_file(format!("{}g", &h[..63]).as_bytes()).is_none());
    }

    #[test]
    fn urls() {
        assert_eq!(
            split_url("https://api.github.com/repos/haji5/clawd-pet/releases/latest"),
            Ok(Url { secure: true, host: "api.github.com", port: 443, path: "/repos/haji5/clawd-pet/releases/latest" })
        );
        assert_eq!(split_url("https://example.com"), Ok(Url { secure: true, host: "example.com", port: 443, path: "/" }));
        assert_eq!(split_url("https://example.com:8443/a?b=c"), Ok(Url { secure: true, host: "example.com", port: 8443, path: "/a?b=c" }));
        assert_eq!(split_url("http://127.0.0.1:5000/x"), Ok(Url { secure: false, host: "127.0.0.1", port: 5000, path: "/x" }));
        for bad in ["http://github.com/x", "ftp://x/y", "github.com/x", "https://:443/x", "https://a@b/x", "https://h:99999/x", "https://h:0/x", ""] {
            assert_eq!(split_url(bad), Err(FetchError::BadUrl), "{bad}");
        }
    }

    #[test]
    fn local_sources() {
        let s = Source::local("http://127.0.0.1:5123/releases/latest").unwrap();
        assert_eq!(s.download_prefix, "http://127.0.0.1:5123/");
        assert!(Source::local("http://127.0.0.2:5123/x").is_none());
        assert!(Source::local("https://127.0.0.1:5123/x").is_none());
        assert!(Source::local("http://127.0.0.1:x/").is_none());
        let g = Source::github();
        assert!(split_url(&g.api).unwrap().secure);
        assert!(g.download_prefix.starts_with("https://github.com/"));
    }

    #[test]
    fn box_texts() {
        assert_eq!(up_to_date_text(v("0.1.1")), "Clawd is up to date (version 0.1.1).");
        assert_eq!(ahead_text(v("0.2.0"), v("0.1.1")), "This Clawd (0.2.0) is newer than the latest release (0.1.1).");
        assert_eq!(ask_text(v("0.1.1"), v("0.1.2")), "Clawd 0.1.2 is available. You have 0.1.1.\n\nUpdate now? Clawd will restart.");
        assert_eq!(updated_text("0.1.1", v("0.1.2"), false), "Clawd was updated from 0.1.1 to 0.1.2.");
        assert_eq!(updated_text("0.1.1", v("0.1.2"), true), "Clawd was updated from 0.1.1 to 0.1.2.\n\nIts Claude hooks were updated too.");
    }
}
