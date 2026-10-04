//! The cookies Chrome stored, read so the embedded browser can start signed
//! in where the user already is — the way other agent apps offer to.
//!
//! Only ever on the user's say-so, from the window: the daemon never reads a
//! browser profile. Chrome encrypts each value with a key kept in the macOS
//! Keychain under "Chrome Safe Storage"; asking the Keychain for it is the
//! caller's job, and macOS asks the user before handing it over. What is read
//! goes straight into the embedded browser's own profile and is never logged
//! or sent anywhere.

use std::path::{Path, PathBuf};

/// One cookie, decrypted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeCookie {
    /// The domain as Chrome keys it: a leading dot for a domain cookie.
    pub host: String,
    /// Cookie name, as set by the site.
    pub name: String,
    /// Decrypted plaintext value. Sensitive: never log it.
    pub value: String,
    /// URL path prefix the cookie is scoped to.
    pub path: String,
    /// When it expires, in seconds since the Unix epoch; `None` for a
    /// session cookie.
    pub expires: Option<i64>,
    /// Sent only over HTTPS.
    pub secure: bool,
    /// Hidden from page scripts.
    pub http_only: bool,
    /// Cross-site sending policy.
    pub same_site: SameSite,
}

/// A cookie's `SameSite` policy, as Chrome stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    /// No policy recorded; the browser's default applies.
    Unspecified,
    /// `SameSite=None`: sent cross-site (requires `secure`).
    None,
    /// `SameSite=Lax`: sent on top-level cross-site navigations only.
    Lax,
    /// `SameSite=Strict`: never sent cross-site.
    Strict,
}

/// A Chrome profile: its directory and the name the user gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeProfile {
    /// Absolute path of the profile directory, e.g. `<Chrome>/Default`.
    pub dir: PathBuf,
    /// Display name from `Local State`, falling back to the directory name.
    pub name: String,
}

/// Why cookies could not be read.
#[derive(Debug, thiserror::Error)]
pub enum CookieError {
    /// The store could not be copied, opened, queried or decrypted; the message
    /// says which.
    #[error("could not read Chrome's cookie store: {0}")]
    Store(String),
}

/// The AES key Chrome derives from its Keychain password.
pub fn derive_key(password: &[u8]) -> [u8; 16] {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, b"saltysalt", 1003, &mut key);
    key
}

/// Decrypt one `encrypted_value`. `with_domain_hash` is set for stores whose
/// schema prefixes each plaintext with a SHA-256 of the host (version 24 on).
pub fn decrypt_value(encrypted: &[u8], key: &[u8; 16], with_domain_hash: bool) -> Option<String> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};

    let body = encrypted
        .strip_prefix(b"v10")
        .or_else(|| encrypted.strip_prefix(b"v11"))?;
    let iv = [b' '; 16];
    let plain = cbc::Decryptor::<aes::Aes128>::new(key.into(), &iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(body)
        .ok()?;
    let plain = if with_domain_hash {
        plain.get(32..)?.to_vec()
    } else {
        plain
    };
    String::from_utf8(plain).ok()
}

/// Chrome's timestamps — microseconds since 1601-01-01 — as Unix seconds;
/// zero means "no expiry".
pub fn chrome_time_to_unix(micros: i64) -> Option<i64> {
    /// Seconds from 1601-01-01 to 1970-01-01.
    const EPOCH_GAP: i64 = 11_644_473_600;
    (micros > 0).then(|| micros / 1_000_000 - EPOCH_GAP)
}

/// Every cookie in a profile's `Cookies` store.
///
/// The store is copied first: Chrome keeps it open and locked while it runs.
pub fn read_cookies(store: &Path, key: &[u8; 16]) -> Result<Vec<ChromeCookie>, CookieError> {
    let fail = |error: &dyn std::fmt::Display| CookieError::Store(error.to_string());
    let copy_dir = std::env::temp_dir().join(format!("ginka-cookies-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&copy_dir).map_err(|e| fail(&e))?;
    let result = (|| {
        let copy = copy_dir.join("Cookies");
        std::fs::copy(store, &copy).map_err(|e| fail(&e))?;
        // What Chrome has not checkpointed yet lives beside the store.
        for suffix in ["-wal", "-shm"] {
            let mut side = store.as_os_str().to_owned();
            side.push(suffix);
            let mut side_copy = copy.as_os_str().to_owned();
            side_copy.push(suffix);
            let _ = std::fs::copy(&side, &side_copy);
        }
        let db = rusqlite::Connection::open_with_flags(
            &copy,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| fail(&e))?;
        let version: i64 = db
            .query_row("SELECT value FROM meta WHERE key = 'version'", [], |row| {
                row.get::<_, String>(0)
            })
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let with_domain_hash = version >= 24;
        let mut statement = db
            .prepare(
                "SELECT host_key, name, value, encrypted_value, path, expires_utc,
                        is_secure, is_httponly, samesite FROM cookies",
            )
            .map_err(|e| fail(&e))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            })
            .map_err(|e| fail(&e))?;
        let mut cookies = Vec::new();
        for row in rows {
            let (host, name, plain, encrypted, path, expires, secure, http_only, same_site) =
                row.map_err(|e| fail(&e))?;
            let value = if !plain.is_empty() {
                plain
            } else {
                match decrypt_value(&encrypted, key, with_domain_hash) {
                    Some(value) => value,
                    // Left out rather than imported empty.
                    None => continue,
                }
            };
            cookies.push(ChromeCookie {
                host,
                name,
                value,
                path,
                expires: chrome_time_to_unix(expires),
                secure: secure != 0,
                http_only: http_only != 0,
                same_site: match same_site {
                    0 => SameSite::None,
                    1 => SameSite::Lax,
                    2 => SameSite::Strict,
                    _ => SameSite::Unspecified,
                },
            });
        }
        Ok(cookies)
    })();
    let _ = std::fs::remove_dir_all(&copy_dir);
    result
}

/// The profiles listed in Chrome's `Local State`, the default first.
pub fn profiles(chrome_dir: &Path) -> Vec<ChromeProfile> {
    let Ok(text) = std::fs::read_to_string(chrome_dir.join("Local State")) else {
        return Vec::new();
    };
    let Ok(state) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(cache) = state
        .get("profile")
        .and_then(|profile| profile.get("info_cache"))
        .and_then(|cache| cache.as_object())
    else {
        return Vec::new();
    };
    let mut found: Vec<ChromeProfile> = cache
        .iter()
        .map(|(dir, info)| ChromeProfile {
            dir: chrome_dir.join(dir),
            name: info
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or(dir)
                .to_string(),
        })
        .collect();
    found.sort_by_key(|profile| {
        let dir = profile
            .dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        (dir.as_deref() != Some("Default"), dir)
    });
    found
}

/// A profile's cookie store: `Network/Cookies` since Chrome 96, `Cookies`
/// before it.
pub fn store_path(profile_dir: &Path) -> Option<PathBuf> {
    [
        profile_dir.join("Network/Cookies"),
        profile_dir.join("Cookies"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

/// The URL a cookie is set against: its host without the leading dot, over
/// HTTPS when it is secure.
pub fn cookie_url(cookie: &ChromeCookie) -> String {
    let scheme = if cookie.secure { "https" } else { "http" };
    let host = cookie.host.trim_start_matches('.');
    let path = if cookie.path.starts_with('/') {
        cookie.path.as_str()
    } else {
        "/"
    };
    format!("{scheme}://{host}{path}")
}

/// The domain a cookie is set with: a domain cookie keeps its leading dot,
/// and a host-only cookie has none, which the store reads as host-only.
pub fn cookie_domain(cookie: &ChromeCookie) -> String {
    if cookie.host.starts_with('.') {
        cookie.host.clone()
    } else {
        String::new()
    }
}

/// Where Chrome keeps its profiles on this machine.
pub fn chrome_dir(home: &Path) -> PathBuf {
    home.join("Library/Application Support/Google/Chrome")
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};

    fn encrypt(plain: &[u8], key: &[u8; 16]) -> Vec<u8> {
        let iv = [b' '; 16];
        let cipher = cbc::Encryptor::<aes::Aes128>::new(key.into(), &iv.into());
        let mut out = b"v10".to_vec();
        out.extend(cipher.encrypt_padded_vec_mut::<Pkcs7>(plain));
        out
    }

    #[test]
    fn a_value_encrypted_the_way_chrome_does_reads_back() {
        let key = derive_key(b"keychain-password");
        assert_ne!(key, [0; 16]);
        // The same password always gives the same key.
        assert_eq!(key, derive_key(b"keychain-password"));

        let old = encrypt(b"session=abc123", &key);
        assert_eq!(
            decrypt_value(&old, &key, false).as_deref(),
            Some("session=abc123")
        );

        // Newer stores put a 32-byte hash of the host in front.
        let mut hashed = vec![7u8; 32];
        hashed.extend_from_slice(b"token");
        let new = encrypt(&hashed, &key);
        assert_eq!(decrypt_value(&new, &key, true).as_deref(), Some("token"));

        // The wrong key, or something that is not Chrome's format, is no value.
        assert_eq!(decrypt_value(&old, &derive_key(b"other"), false), None);
        assert_eq!(decrypt_value(b"plain", &key, false), None);
    }

    #[test]
    fn chrome_times_count_from_1601() {
        // 2026-01-01T00:00:00Z.
        assert_eq!(
            chrome_time_to_unix(13_411_699_200_000_000),
            Some(1_767_225_600)
        );
        assert_eq!(chrome_time_to_unix(0), None);
    }

    /// Host, name, plain value, encrypted value, expiry, secure, HTTP-only
    /// and SameSite, as the store's columns hold them.
    type Row<'a> = (&'a str, &'a str, &'a str, Vec<u8>, i64, i64, i64, i64);

    fn store(dir: &Path, version: i64, rows: &[Row<'_>]) -> PathBuf {
        let path = dir.join("Cookies");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE meta(key TEXT, value TEXT);
             CREATE TABLE cookies(host_key TEXT, name TEXT, value TEXT, encrypted_value BLOB,
               path TEXT, expires_utc INTEGER, is_secure INTEGER, is_httponly INTEGER,
               samesite INTEGER);",
        )
        .unwrap();
        db.execute(
            "INSERT INTO meta VALUES ('version', ?1)",
            [version.to_string()],
        )
        .unwrap();
        for (host, name, value, encrypted, expires, secure, http_only, same_site) in rows {
            db.execute(
                "INSERT INTO cookies VALUES (?1, ?2, ?3, ?4, '/', ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    host, name, value, encrypted, expires, secure, http_only, same_site
                ],
            )
            .unwrap();
        }
        path
    }

    #[test]
    fn a_profile_store_reads_as_decrypted_cookies() {
        let dir = tempfile::tempdir().unwrap();
        let key = derive_key(b"pw");
        let mut hashed = vec![0u8; 32];
        hashed.extend_from_slice(b"s3cret");
        let path = store(
            dir.path(),
            24,
            &[
                (
                    ".github.com",
                    "user_session",
                    "",
                    encrypt(&hashed, &key),
                    13_411_699_200_000_000,
                    1,
                    1,
                    1,
                ),
                ("example.com", "plain", "visible", Vec::new(), 0, 0, 0, -1),
                // Undecryptable rows are left out rather than imported empty.
                (
                    "bad.example",
                    "broken",
                    "",
                    b"v10garbage".to_vec(),
                    0,
                    0,
                    0,
                    0,
                ),
            ],
        );
        let cookies = read_cookies(&path, &key).unwrap();
        assert_eq!(
            cookies,
            vec![
                ChromeCookie {
                    host: ".github.com".into(),
                    name: "user_session".into(),
                    value: "s3cret".into(),
                    path: "/".into(),
                    expires: Some(1_767_225_600),
                    secure: true,
                    http_only: true,
                    same_site: SameSite::Lax,
                },
                ChromeCookie {
                    host: "example.com".into(),
                    name: "plain".into(),
                    value: "visible".into(),
                    path: "/".into(),
                    expires: None,
                    secure: false,
                    http_only: false,
                    same_site: SameSite::Unspecified,
                },
            ]
        );
    }

    #[test]
    fn a_profile_store_is_found_where_chrome_keeps_it_now_or_kept_it_before() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(store_path(dir.path()), None);
        std::fs::write(dir.path().join("Cookies"), b"").unwrap();
        assert_eq!(store_path(dir.path()), Some(dir.path().join("Cookies")));
        std::fs::create_dir(dir.path().join("Network")).unwrap();
        std::fs::write(dir.path().join("Network/Cookies"), b"").unwrap();
        assert_eq!(
            store_path(dir.path()),
            Some(dir.path().join("Network/Cookies"))
        );
    }

    #[test]
    fn a_cookie_is_set_against_its_own_host() {
        let cookie = |host: &str, path: &str, secure: bool| ChromeCookie {
            host: host.into(),
            name: "n".into(),
            value: "v".into(),
            path: path.into(),
            expires: None,
            secure,
            http_only: false,
            same_site: SameSite::Unspecified,
        };
        let domain = cookie(".github.com", "/", true);
        assert_eq!(cookie_url(&domain), "https://github.com/");
        assert_eq!(cookie_domain(&domain), ".github.com");
        let host_only = cookie("example.com", "/app", false);
        assert_eq!(cookie_url(&host_only), "http://example.com/app");
        assert_eq!(cookie_domain(&host_only), "");
    }

    #[test]
    fn profiles_come_from_local_state_with_the_default_first() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Local State"),
            r#"{"profile":{"info_cache":{
                "Profile 1":{"name":"Work"},
                "Default":{"name":"Personal"}
            }}}"#,
        )
        .unwrap();
        assert_eq!(
            profiles(dir.path()),
            vec![
                ChromeProfile {
                    dir: dir.path().join("Default"),
                    name: "Personal".into()
                },
                ChromeProfile {
                    dir: dir.path().join("Profile 1"),
                    name: "Work".into()
                },
            ]
        );
        assert!(profiles(&dir.path().join("missing")).is_empty());
    }
}
