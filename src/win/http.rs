//! One HTTPS GET through WinHTTP, for "Check for updates" only: the one
//! thing in Clawd that uses the network beyond `127.0.0.1` (PLAN.md §1), and
//! only after the user's click. `winhttp.dll` is delay-loaded (`build.rs`),
//! so the hook never maps it.
//!
//! Certificate checks and the redirect policy stay at WinHTTP's defaults:
//! redirects are followed (a release download goes from `github.com` to
//! another host), except from HTTPS down to HTTP.

use super::wide;
use crate::update::{self, FetchError};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle,
    WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts,
};

/// Closed on every path, the request first (they drop in reverse order).
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn last_error() -> FetchError {
    FetchError::Os(unsafe { GetLastError() })
}

fn handle(h: *mut c_void) -> Result<Handle, FetchError> {
    if h.is_null() { Err(last_error()) } else { Ok(Handle(h)) }
}

fn ok(done: i32) -> Result<(), FetchError> {
    if done != 0 { Ok(()) } else { Err(last_error()) }
}

/// GET `url`; the body if the answer is 200 and at most `cap` bytes.
/// Blocks for up to the timeouts below: call it from a worker thread.
pub fn get(url: &str, cap: usize) -> Result<Vec<u8>, FetchError> {
    let url = update::split_url(url)?;
    let agent = wide(concat!("clawd-pet/", env!("CARGO_PKG_VERSION")));
    let (host, verb, path) = (wide(url.host), wide("GET"), wide(url.path));
    let headers = wide("Accept: application/vnd.github+json");
    unsafe {
        let session = handle(WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, null(), null(), 0))?;
        // Resolve, connect, send, receive (ms).
        ok(WinHttpSetTimeouts(session.0, 5_000, 10_000, 10_000, 20_000))?;
        let connection = handle(WinHttpConnect(session.0, host.as_ptr(), url.port, 0))?;
        let flags = if url.secure { WINHTTP_FLAG_SECURE } else { 0 };
        let request = handle(WinHttpOpenRequest(connection.0, verb.as_ptr(), path.as_ptr(), null(), null(), null(), flags))?;
        // Length -1: NUL-terminated.
        ok(WinHttpSendRequest(request.0, headers.as_ptr(), u32::MAX, null(), 0, 0, 0))?;
        ok(WinHttpReceiveResponse(request.0, null_mut()))?;
        let (mut status, mut len) = (0u32, size_of::<u32>() as u32);
        ok(WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            null(),
            (&mut status as *mut u32).cast(),
            &mut len,
            null_mut(),
        ))?;
        if status != 200 {
            return Err(FetchError::Status(status));
        }
        let mut body = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            ok(WinHttpReadData(request.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut read))?;
            if read == 0 {
                return Ok(body);
            }
            if body.len() + read as usize > cap {
                return Err(FetchError::TooBig);
            }
            body.extend_from_slice(&buf[..read as usize]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A server on `127.0.0.1` that answers one request with `response`,
    /// and hands back the request it got.
    fn serve_once(response: Vec<u8>) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/releases/latest", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.ends_with(b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0, "the request ended early");
                request.extend_from_slice(&buf[..n]);
            }
            let _ = stream.write_all(&response);
            String::from_utf8_lossy(&request).into_owned()
        });
        (url, server)
    }

    fn answer(status: &str, body: &[u8]) -> Vec<u8> {
        let mut r = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
        r.extend_from_slice(body);
        r
    }

    #[test]
    fn a_200_gives_the_body_and_the_request_says_who_asks() {
        let body: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        let (url, server) = serve_once(answer("200 OK", &body));
        assert_eq!(get(&url, body.len()), Ok(body));
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /releases/latest HTTP/1.1\r\n"), "{request}");
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains(&format!("user-agent: clawd-pet/{}\r\n", env!("CARGO_PKG_VERSION"))), "{request}");
        assert!(lower.contains("accept: application/vnd.github+json\r\n"), "{request}");
        assert!(!lower.contains("cookie") && !lower.contains("authorization"), "{request}");
    }

    #[test]
    fn other_statuses_are_errors() {
        let (url, server) = serve_once(answer("404 Not Found", b"nope"));
        assert_eq!(get(&url, 1000), Err(FetchError::Status(404)));
        server.join().unwrap();
    }

    #[test]
    fn a_body_over_the_cap_is_refused() {
        let (url, server) = serve_once(answer("200 OK", &[b'x'; 5000]));
        assert_eq!(get(&url, 4999), Err(FetchError::TooBig));
        server.join().unwrap();
    }

    #[test]
    fn nobody_listening_is_an_os_error() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port(); // closed again at once
        assert_eq!(get(&format!("http://127.0.0.1:{port}/x"), 10), Err(FetchError::Os(12029)));
    }

    /// The real thing: the v0.1.1 exe from GitHub, through the 302 to its
    /// asset host, checked against the release's digest. Network, so only
    /// on request: `cargo test --bin clawd-pet real_release -- --ignored`.
    #[test]
    #[ignore = "downloads from GitHub"]
    fn the_real_release_downloads_through_its_redirect() {
        let url = "https://github.com/haji5/clawd-pet/releases/download/v0.1.1/clawd-pet.exe";
        let exe = get(url, 771_072).unwrap();
        assert_eq!(exe.len(), 771_072);
        let hex: String = update::sha256(&exe).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "9e23c9aec018708ae31019545d3ae986a1fe04c90126d066f7d3156982233cd6");
    }

    #[test]
    fn plain_http_elsewhere_is_never_tried() {
        assert_eq!(get("http://example.com/", 10), Err(FetchError::BadUrl));
    }
}
