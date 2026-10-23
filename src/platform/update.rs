//! Stable GitHub release checks and explicitly requested, verified Windows updates.

use std::ffi::c_void;
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptHash, BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE,
    BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
};

const API_URL: &str = "https://api.github.com/repos/ponslookspain/tile-rs/releases/latest";
const ASSET_PREFIX: &str = "https://github.com/ponslookspain/tile-rs/releases/download/";
// Set to Some("<64 hex characters>") before publishing signed updates; see docs/RELEASING.md.
// No production key has been provisioned: builds/tests work, installation fails closed.
const UPDATE_VERIFYING_KEY: Option<&str> = None;
const MAX_BODY: usize = 1024 * 1024;
const MAX_INSTALLER: usize = 256 * 1024 * 1024;
static CHECK_RUNNING: AtomicBool = AtomicBool::new(false);
static INSTALL_RUNNING: AtomicBool = AtomicBool::new(false);
static BACKGROUND_STARTED: AtomicBool = AtomicBool::new(false);

struct CheckGuard;
impl CheckGuard {
    fn acquire() -> Option<Self> {
        CHECK_RUNNING
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self)
    }
}
impl Drop for CheckGuard {
    fn drop(&mut self) {
        CHECK_RUNNING.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub url: String,
    installer_url: String,
    checksum_url: String,
    signature_url: String,
    size: u64,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<ApiAsset>,
}
#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct HttpHandle(*mut c_void);
impl Drop for HttpHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// Only HTTPS is used, with WinHTTP's certificate and redirect validation enabled.
fn download(
    url: &str,
    limit: usize,
    expected_size: u64,
    progress: impl Fn(u8),
) -> Result<Vec<u8>, String> {
    let (host, path) = url
        .strip_prefix("https://")
        .and_then(|s| s.split_once('/'))
        .ok_or("Invalid update URL")?;
    if !matches!(host, "github.com" | "api.github.com") {
        return Err("Untrusted update host".into());
    }
    unsafe {
        let agent = wide("tile-rs-update");
        let session = HttpHandle(WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err("Could not start WinHTTP".into());
        }
        WinHttpSetTimeouts(session.0, 10000, 10000, 30000, 30000)
            .map_err(|_| "Could not configure network timeouts")?;
        let host = wide(host);
        let connection = HttpHandle(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0));
        if connection.0.is_null() {
            return Err("Could not reach GitHub".into());
        }
        let verb = wide("GET");
        let path = wide(&format!("/{path}"));
        let request = HttpHandle(WinHttpOpenRequest(
            connection.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err("Could not create the update request".into());
        }
        WinHttpSendRequest(request.0, None, None, 0, 0, 0)
            .map_err(|_| "Cannot connect. Check your internet connection.")?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .map_err(|_| "No response from GitHub")?;
        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|_| "Could not read the server status")?;
        if status != 200 {
            return Err(format!(
                "GitHub returned HTTP {status}. Please try again later."
            ));
        }
        let mut body = Vec::new();
        let mut last_progress = None;
        loop {
            let mut buffer = [0u8; 65536];
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            )
            .map_err(|_| "The download was interrupted. Please try again.")?;
            if read == 0 {
                break;
            }
            if body.len() + read as usize > limit {
                return Err("Update response exceeded the size limit".into());
            }
            body.extend_from_slice(&buffer[..read as usize]);
            if let Some(percent) = (body.len() as u64 * 90).checked_div(expected_size) {
                let percent = percent.min(90) as u8;
                if last_progress != Some(percent) {
                    progress(percent);
                    last_progress = Some(percent);
                }
            }
        }
        Ok(body)
    }
}

fn release_from_json(body: &[u8], current: &str) -> Result<Option<Release>, String> {
    let info: ApiRelease =
        serde_json::from_slice(body).map_err(|_| "Unrecognised GitHub release information")?;
    if parse_version(&info.tag_name).is_none() {
        return Err("Invalid release version".into());
    }
    if info.draft || info.prerelease || !is_newer(&info.tag_name, current) {
        return Ok(None);
    }
    let version = info.tag_name.trim_start_matches(['v', 'V']);
    let filename = format!("tile-rs-{version}-setup.exe");
    let asset = info
        .assets
        .iter()
        .find(|a| a.name == filename)
        .ok_or("The latest release has no Windows installer yet")?;
    let checksum = info
        .assets
        .iter()
        .find(|a| a.name == format!("{filename}.sha256"))
        .ok_or("The release has no installer checksum. Please use the release page.")?;
    let signature = info
        .assets
        .iter()
        .find(|a| a.name == format!("{filename}.sha256.sig"))
        .ok_or("The release has no update signature. Nothing can be installed.")?;
    let prefix = format!("{ASSET_PREFIX}{}/", info.tag_name);
    if asset.browser_download_url != format!("{prefix}{filename}")
        || checksum.browser_download_url != format!("{prefix}{filename}.sha256")
        || signature.browser_download_url != format!("{prefix}{filename}.sha256.sig")
        || signature.size != 64
        || asset.size == 0
        || asset.size > MAX_INSTALLER as u64
    {
        return Err("The release contains invalid installer information".into());
    }
    Ok(Some(Release {
        version: info.tag_name.clone(),
        notes: info.body.unwrap_or_default(),
        url: format!(
            "https://github.com/ponslookspain/tile-rs/releases/tag/{}",
            info.tag_name
        ),
        installer_url: asset.browser_download_url.clone(),
        checksum_url: checksum.browser_download_url.clone(),
        signature_url: signature.browser_download_url.clone(),
        size: asset.size,
    }))
}

pub fn check_latest() -> Result<Option<Release>, String> {
    let _guard = CheckGuard::acquire()
        .ok_or("An update check is already running. Try again in a moment.")?;
    release_from_json(
        &download(API_URL, MAX_BODY, 0, |_| {})?,
        env!("CARGO_PKG_VERSION"),
    )
}

fn update_verifying_key(encoded: Option<&str>) -> Result<VerifyingKey, String> {
    let encoded = encoded.ok_or(
        "Update signature verification is not configured in this build. Please use the release page.",
    )?;
    if encoded.len() != 64 || !encoded.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("The update verification key is invalid. Nothing was installed.".into());
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&encoded[i * 2..i * 2 + 2], 16)
            .map_err(|_| "The update verification key is invalid")?;
    }
    VerifyingKey::from_bytes(&bytes).map_err(|_| "The update verification key is invalid".into())
}

/// The signature covers the exact checksum asset bytes, including its filename and newline.
fn verify_checksum_signature(
    checksum: &[u8],
    signature: &[u8],
    key: &VerifyingKey,
) -> Result<(), String> {
    let signature = Signature::from_slice(signature)
        .map_err(|_| "The update signature is missing or invalid. Nothing was installed.")?;
    key.verify_strict(checksum, &signature)
        .map_err(|_| "Update signature verification failed. Nothing was installed.".into())
}

fn checksum_matches(bytes: &[u8], checksum: &[u8], filename: &str) -> Result<bool, String> {
    let text = std::str::from_utf8(checksum).map_err(|_| "The installer checksum is invalid")?;
    let mut parts = text.split_whitespace();
    let expected = parts
        .next()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("The installer checksum is invalid")?;
    if parts.next() != Some(filename) || parts.next().is_some() {
        return Err(
            "The signed checksum does not match this release's installer. Nothing was installed."
                .into(),
        );
    }
    let mut digest = [0u8; 32];
    unsafe {
        let mut algorithm = BCRYPT_ALG_HANDLE::default();
        BCryptOpenAlgorithmProvider(
            &mut algorithm,
            BCRYPT_SHA256_ALGORITHM,
            PCWSTR::null(),
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
        .ok()
        .map_err(|_| "Could not initialize SHA-256 verification")?;
        let result = BCryptHash(algorithm, None, bytes, &mut digest).ok();
        let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
        result.map_err(|_| "Could not verify the downloaded installer")?;
    }
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(actual.eq_ignore_ascii_case(expected))
}

/// Downloads and verifies first; a separate helper survives the launcher's event loop.
/// On success the caller must close the launcher so the installer can replace its executable.
pub fn install(release: &Release, progress: impl Fn(u8)) -> Result<(), String> {
    if INSTALL_RUNNING.swap(true, Ordering::AcqRel) {
        return Err("An update is already being installed".into());
    }
    let result = (|| {
        progress(0);
        let key = update_verifying_key(UPDATE_VERIFYING_KEY)?;
        let checksum = download(&release.checksum_url, 4096, 0, |_| {})?;
        let signature = download(&release.signature_url, 64, 0, |_| {})?;
        verify_checksum_signature(&checksum, &signature, &key)?;
        let bytes = download(
            &release.installer_url,
            MAX_INSTALLER,
            release.size,
            &progress,
        )?;
        if bytes.len() as u64 != release.size {
            return Err("The downloaded installer is incomplete".into());
        }
        let filename = format!(
            "tile-rs-{}-setup.exe",
            release.version.trim_start_matches(['v', 'V'])
        );
        if !checksum_matches(&bytes, &checksum, &filename)? {
            return Err("Installer verification failed. Nothing was installed.".into());
        }
        progress(95);
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let target = exe
            .parent()
            .ok_or("Cannot locate the installation folder")?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("tile-rs-update-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&directory).map_err(|e| format!("Cannot create update folder: {e}"))?;
        let installer = directory.join("setup.exe");
        let helper = directory.join("install.ps1");
        let ready = directory.join("ready");
        let stage: Result<(), String> = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&installer)
                .map_err(|e| e.to_string())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            std::fs::write(&helper, INSTALL_HELPER).map_err(|e| e.to_string())?;
            let mut child = Command::new("powershell.exe")
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(&helper)
                .env("TILE_UPDATE_PARENT", std::process::id().to_string())
                .env("TILE_UPDATE_EXE", &exe)
                .env("TILE_UPDATE_TARGET", target)
                .env("TILE_UPDATE_SETUP", &installer)
                .env("TILE_UPDATE_READY", &ready)
                .env(
                    "TILE_UPDATE_AUTOSTART",
                    if crate::platform::autostart::autostart_enabled() {
                        "1"
                    } else {
                        "0"
                    },
                )
                .creation_flags(0x08000000)
                .spawn()
                .map_err(|e| format!("Cannot start the update installer: {e}"))?;
            // A policy can block a script even after process creation succeeds.
            // Keep both applications running until the helper confirms execution.
            for _ in 0..100 {
                if ready.is_file() {
                    return Ok(());
                }
                if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                    return Err(
                        "The update helper could not run. Check your PowerShell policy.".into(),
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
            Err("The update helper did not start. Please try again.".into())
        })();
        if stage.is_err() {
            let _ = std::fs::remove_file(&installer);
            let _ = std::fs::remove_file(&helper);
            let _ = std::fs::remove_file(&ready);
            let _ = std::fs::remove_dir(&directory);
        }
        stage?;
        crate::ui::tray::request_manager_quit();
        progress(100);
        Ok(())
    })();
    if result.is_err() {
        INSTALL_RUNNING.store(false, Ordering::Release);
    }
    result
}

const INSTALL_HELPER: &str = r#"
$ErrorActionPreference = 'Stop'
try {
    [System.IO.File]::WriteAllText($env:TILE_UPDATE_READY, 'ready')
    $launcher = Get-Process -Id ([int]$env:TILE_UPDATE_PARENT) -ErrorAction SilentlyContinue
    if ($launcher -and -not $launcher.WaitForExit(60000)) { throw 'The launcher is still running. Close it and try again.' }
    $processes = Get-Process -Name 'tile-rs' -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $env:TILE_UPDATE_EXE }
    foreach ($manager in $processes) {
        if (-not $manager.WaitForExit(30000)) { throw 'tile-rs is still running. Quit it and try again.' }
    }
    $arguments = @('/SILENT', '/NORESTART', '/SUPPRESSMSGBOXES', '/NOCLOSEAPPLICATIONS', ('/DIR="' + $env:TILE_UPDATE_TARGET + '"'))
    if ($env:TILE_UPDATE_AUTOSTART -eq '1') { $arguments += '/MERGETASKS=autostart' }
    else { $arguments += '/MERGETASKS=!autostart' }
    $setup = Start-Process -FilePath $env:TILE_UPDATE_SETUP -ArgumentList $arguments -PassThru -Wait
    if ($setup.ExitCode -ne 0) { throw ('Installer exited with code ' + $setup.ExitCode + '. Your previous settings have been kept.') }
    Start-Process -FilePath $env:TILE_UPDATE_EXE
} catch {
    Add-Type -AssemblyName System.Windows.Forms
    [System.Windows.Forms.MessageBox]::Show($_.Exception.Message, 'tile-rs update failed') | Out-Null
    if (Test-Path -LiteralPath $env:TILE_UPDATE_EXE) { Start-Process -FilePath $env:TILE_UPDATE_EXE }
} finally {
    Remove-Item -LiteralPath $env:TILE_UPDATE_READY -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $env:TILE_UPDATE_SETUP -ErrorAction SilentlyContinue
    $scriptPath = $PSCommandPath
    Remove-Item -LiteralPath $scriptPath -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath (Split-Path -Parent $scriptPath) -ErrorAction SilentlyContinue
}
"#;

/// Stable releases are checked on startup and every six hours; no automatic installation.
pub fn start_background_checks() {
    if BACKGROUND_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("tile-rs-updates".into())
        .spawn(|| {
            std::thread::sleep(Duration::from_secs(15));
            let mut notified = String::new();
            loop {
                match check_latest() {
                    Ok(Some(release)) if release.version != notified => {
                        crate::ui::tray::notify_update(&release.version);
                        notified = release.version;
                    }
                    Err(error) => {
                        crate::diagnostics::log(format!("background update check: {error}"))
                    }
                    _ => {}
                }
                std::thread::sleep(Duration::from_secs(6 * 60 * 60));
            }
        });
}

/// Compatibility entry point for tray menus; the launcher presents check/install controls.
pub fn check_and_notify() {
    crate::ui::launcher::show_updates();
}

fn identifiers_valid(value: &str, numeric_leading_zero: bool) -> bool {
    value.split('.').all(|part| {
        !part.is_empty()
            && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && (numeric_leading_zero
                || part.len() == 1
                || !part.starts_with('0')
                || !part.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Parse stable/prerelease versions, accepting the historical abbreviated "0.4" form.
fn parse_version(s: &str) -> Option<([u64; 3], Option<&str>)> {
    let s = s.trim();
    let s = s
        .strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s);
    let core = if let Some((version, metadata)) = s.split_once('+') {
        if !identifiers_valid(metadata, true) {
            return None;
        }
        version
    } else {
        s
    };
    let (core, pre) = if let Some((core, pre)) = core.split_once('-') {
        if !identifiers_valid(pre, false) {
            return None;
        }
        (core, Some(pre))
    } else {
        (core, None)
    };
    let mut numbers = [0; 3];
    for (i, part) in core.split('.').enumerate() {
        if i >= 3
            || part.is_empty()
            || !part.bytes().all(|b| b.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return None;
        }
        numbers[i] = part.parse().ok()?;
    }
    Some((numbers, pre))
}

fn compare_prerelease(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let (a, b) = match (a, b) {
        (None, None) => return Equal,
        (None, Some(_)) => return Greater,
        (Some(_), None) => return Less,
        (Some(a), Some(b)) => (a, b),
    };
    let (mut a, mut b) = (a.split('.'), b.split('.'));
    loop {
        let (x, y) = match (a.next(), b.next()) {
            (None, None) => return Equal,
            (None, Some(_)) => return Less,
            (Some(_), None) => return Greater,
            (Some(x), Some(y)) => (x, y),
        };
        let numeric = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        let order = match (numeric(x), numeric(y)) {
            (true, true) => x.len().cmp(&y.len()).then_with(|| x.cmp(y)),
            (true, false) => Less,
            (false, true) => Greater,
            (false, false) => x.cmp(y),
        };
        if order != Equal {
            return order;
        }
    }
}

/// Whether `latest` is a strictly newer version than `current`.
fn is_newer(latest: &str, current: &str) -> bool {
    let (Some((a, pre_a)), Some((b, pre_b))) = (parse_version(latest), parse_version(current))
    else {
        return false;
    };
    a.cmp(&b)
        .then_with(|| compare_prerelease(pre_a, pre_b))
        .is_gt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_json(version: &str) -> serde_json::Value {
        let prefix = format!("{ASSET_PREFIX}v{version}/tile-rs-{version}-setup.exe");
        serde_json::json!({
            "tag_name": format!("v{version}"), "body": "Release notes", "assets": [
                { "name": format!("tile-rs-{version}-setup.exe"), "browser_download_url": prefix, "size": 123 },
                { "name": format!("tile-rs-{version}-setup.exe.sha256"), "browser_download_url": format!("{prefix}.sha256"), "size": 90 },
                { "name": format!("tile-rs-{version}-setup.exe.sha256.sig"), "browser_download_url": format!("{prefix}.sha256.sig"), "size": 64 }
            ]
        })
    }

    #[test]
    fn stable_release_requires_matching_installer_and_checksum_assets() {
        let info = release_json("0.6.0");
        let body = serde_json::to_vec(&info).unwrap();
        let release = release_from_json(&body, "0.5.0").unwrap().unwrap();
        assert_eq!(release.version, "v0.6.0");
        assert_eq!(release.notes, "Release notes");
        assert!(release_from_json(&body, "0.6.0").unwrap().is_none());
        assert!(release_from_json(b"{}", "0.5.0").is_err());
        for field in ["draft", "prerelease"] {
            let mut info = info.clone();
            info[field] = true.into();
            assert!(
                release_from_json(&serde_json::to_vec(&info).unwrap(), "0.5.0")
                    .unwrap()
                    .is_none()
            );
        }
        let mut missing = info.clone();
        missing["assets"].as_array_mut().unwrap().pop();
        assert!(release_from_json(&serde_json::to_vec(&missing).unwrap(), "0.5.0").is_err());
        let mut untrusted = info;
        untrusted["assets"][0]["browser_download_url"] = "https://example.com/setup.exe".into();
        assert!(release_from_json(&serde_json::to_vec(&untrusted).unwrap(), "0.5.0").is_err());
    }

    #[test]
    fn checksum_signatures_reject_tampering_missing_data_and_another_key() {
        use ed25519_dalek::{Signer, SigningKey};
        // Deterministic test-only pair; never used by the production verifier.
        let signing = SigningKey::from_bytes(&[0x42; 32]);
        let encoded: String = signing
            .verifying_key()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let key = update_verifying_key(Some(&encoded)).unwrap();
        let checksum = b"checksum  tile-rs-test-setup.exe\n";
        let signature = signing.sign(checksum).to_bytes();
        assert!(verify_checksum_signature(checksum, &signature, &key).is_ok());
        assert!(verify_checksum_signature(b"tampered", &signature, &key).is_err());
        let mut tampered = signature;
        tampered[0] ^= 1;
        assert!(verify_checksum_signature(checksum, &tampered, &key).is_err());
        assert!(verify_checksum_signature(checksum, &[], &key).is_err());
        let other = SigningKey::from_bytes(&[0x24; 32]).verifying_key();
        assert!(verify_checksum_signature(checksum, &signature, &other).is_err());
        assert!(update_verifying_key(None).is_err());
        assert!(update_verifying_key(Some("not a key")).is_err());
    }

    #[test]
    fn release_signature_asset_must_have_trusted_url_and_exact_size() {
        for (field, value) in [
            (
                "browser_download_url",
                serde_json::json!("https://example.com/checksum.sig"),
            ),
            ("size", serde_json::json!(63)),
        ] {
            let mut info = release_json("0.6.0");
            info["assets"][2][field] = value;
            assert!(release_from_json(&serde_json::to_vec(&info).unwrap(), "0.5.0").is_err());
        }
    }

    #[test]
    fn sha256_is_verified_before_any_installer_is_started() {
        let valid = b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  setup.exe";
        assert!(checksum_matches(b"abc", valid, "setup.exe").unwrap());
        assert!(!checksum_matches(b"modified", valid, "setup.exe").unwrap());
        assert!(checksum_matches(b"abc", b"invalid", "setup.exe").is_err());
        // A valid hash/signature from an older installer must not authenticate a newer release.
        assert!(checksum_matches(b"abc", valid, "tile-rs-9.0.0-setup.exe").is_err());
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

    #[test]
    fn stable_versions_outrank_prereleases_and_build_metadata_is_ignored() {
        assert!(is_newer("0.5.0", "0.5.0-rc.1"));
        assert!(!is_newer("0.5.0-rc.1", "0.5.0"));
        assert!(is_newer("0.5.0-rc.10", "0.5.0-rc.2"));
        assert!(is_newer("0.5.0-beta", "0.5.0-2"));
        assert!(!is_newer("0.5.0+build.2", "0.5.0+build.1"));
        for invalid in [
            "",
            "v",
            "vv0.5.0",
            "0.5.0.1",
            "0.05.0",
            "0.5.0-rc.01",
            "0.5.0-",
            "0.5.0+",
        ] {
            assert!(parse_version(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn update_checks_are_single_flight_and_release_the_guard() {
        let guard = CheckGuard::acquire().unwrap();
        assert!(CheckGuard::acquire().is_none());
        drop(guard);
        assert!(CheckGuard::acquire().is_some());
    }
}
