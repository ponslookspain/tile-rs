//! On-demand "check for updates": asks the GitHub releases API for the latest tag. There is no
//! background network access - this only runs when the user picks the menu item / button.

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
};

const HOST: &str = "api.github.com";
const PATH: &str = "/repos/ponslookspain/tile-rs/releases/latest";
const AGENT: &str = "tile-rs";
const RELEASES_URL: &str = "https://github.com/ponslookspain/tile-rs/releases/latest";
/// Give up rather than read an unexpectedly large response.
const MAX_BODY: usize = 64 * 1024;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The latest release tag (`"v0.4.1"`), or a human-readable reason it could not be fetched.
fn latest_tag() -> Result<String, String> {
    unsafe {
        let agent = wide(AGENT);
        let session = WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err("could not start WinHTTP".into());
        }
        let _ = WinHttpSetTimeouts(session, 5000, 5000, 5000, 5000);
        let result = fetch(session);
        let _ = WinHttpCloseHandle(session);
        result
    }
}

unsafe fn fetch(session: *mut c_void) -> Result<String, String> {
    let host = wide(HOST);
    let connect = WinHttpConnect(session, PCWSTR(host.as_ptr()), 443, 0);
    if connect.is_null() {
        return Err("could not reach github.com".into());
    }
    let verb = wide("GET");
    let path = wide(PATH);
    let req = WinHttpOpenRequest(
        connect,
        PCWSTR(verb.as_ptr()),
        PCWSTR(path.as_ptr()),
        PCWSTR::null(),
        PCWSTR::null(),
        std::ptr::null(),
        WINHTTP_FLAG_SECURE,
    );
    if req.is_null() {
        let _ = WinHttpCloseHandle(connect);
        return Err("could not create the request".into());
    }
    let out = if WinHttpSendRequest(req, None, None, 0, 0, 0).is_err() {
        Err("the request could not be sent (are you online?)".into())
    } else if WinHttpReceiveResponse(req, std::ptr::null_mut()).is_err() {
        Err("no response from the server".into())
    } else {
        let mut body = Vec::new();
        loop {
            let mut buf = [0u8; 4096];
            let mut read = 0u32;
            if WinHttpReadData(
                req,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                &mut read,
            )
            .is_err()
            {
                break;
            }
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buf[..read as usize]);
            if body.len() >= MAX_BODY {
                break;
            }
        }
        let text = String::from_utf8_lossy(&body);
        match tag_from_json(&text) {
            Some(t) if !t.is_empty() => Ok(t),
            _ => Err("the release information was not recognised".into()),
        }
    };
    let _ = WinHttpCloseHandle(req);
    let _ = WinHttpCloseHandle(connect);
    out
}

/// Pulls the value of `"tag_name"` out of the GitHub JSON without a JSON dependency.
fn tag_from_json(body: &str) -> Option<String> {
    let key = "\"tag_name\"";
    let rest = &body[body.find(key)? + key.len()..];
    let rest = &rest[rest.find(':')? + 1..];
    let rest = &rest[rest.find('"')? + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// "v0.4.1" / "0.4.1" -> [0, 4, 1] (missing / non-numeric parts count as 0).
fn version_parts(s: &str) -> Vec<u32> {
    s.trim()
        .trim_start_matches(['v', 'V'])
        .split('.')
        .map(|p| p.trim().parse().unwrap_or(0))
        .collect()
}

/// Whether `latest` is a strictly newer version than `current`.
fn is_newer(latest: &str, current: &str) -> bool {
    let a = version_parts(latest);
    let b = version_parts(current);
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

/// Checks GitHub for a newer release and tells the user the result. Runs only when asked.
pub fn check_and_notify() {
    crate::log("checking for updates");
    let current = env!("CARGO_PKG_VERSION");
    match latest_tag() {
        Ok(tag) if is_newer(&tag, current) => crate::notify(
            "tile-rs: update available",
            &format!("A newer version {tag} is available (you have v{current}).\n\n{RELEASES_URL}"),
        ),
        Ok(_) => crate::notify("tile-rs", &format!("You are up to date (v{current}).")),
        Err(e) => crate::notify("tile-rs: update check failed", &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_is_read_from_the_github_json() {
        let body = r#"{"tag_name":"v0.5.0","name":"0.5.0"}"#;
        assert_eq!(tag_from_json(body).as_deref(), Some("v0.5.0"));
        assert!(tag_from_json("{}").is_none());
    }

    #[test]
    fn versions_compare_by_numbers() {
        assert!(is_newer("v0.4.1", "0.4.0"));
        assert!(is_newer("0.5.0", "0.4.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("v0.4.0", "0.4.0"));
        assert!(!is_newer("0.3.9", "0.4.0"));
        // a missing part counts as zero, so 0.4 == 0.4.0
        assert!(!is_newer("0.4", "0.4.0"));
        assert!(is_newer("0.4.1", "0.4"));
    }
}
