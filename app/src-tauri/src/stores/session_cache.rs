//! Device-local cookie persistence. Only DPAPI ciphertext is written to the auth profile.
use super::StoreProvider;
use anyhow::{anyhow, Result};
use std::path::Path;
use tauri::webview::Cookie;

#[cfg(windows)]
mod platform {
    use super::*;
    use anyhow::ensure;
    use serde::{Deserialize, Serialize};
    use std::{
        fs,
        io::{Read, Write},
        os::windows::{ffi::OsStrExt, fs::MetadataExt},
    };
    use tauri::webview::cookie::{time::OffsetDateTime, SameSite};
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{LocalFree, HLOCAL},
            Security::Cryptography::{
                CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
            },
        },
    };

    const MAX_COOKIES: usize = 512;
    const MAX_PLAINTEXT: usize = 2 * 1024 * 1024;
    const MAX_CIPHERTEXT: usize = MAX_PLAINTEXT + 64 * 1024;

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StoredCookie {
        name: String,
        value: String,
        domain: Option<String>,
        path: Option<String>,
        secure: Option<bool>,
        http_only: Option<bool>,
        same_site: Option<String>,
        expires: Option<i64>,
    }

    fn domain_allowed(provider: StoreProvider, domain: &str) -> bool {
        let domain = domain.trim_start_matches('.').to_ascii_lowercase();
        match provider {
            StoreProvider::Booth => domain == "booth.pm" || domain.ends_with(".booth.pm"),
            StoreProvider::Gumroad => domain == "gumroad.com" || domain.ends_with(".gumroad.com"),
            StoreProvider::Jinxxy => matches!(
                domain.as_str(),
                "jinxxy.com"
                    | "www.jinxxy.com"
                    | "api.jinxxy.com"
                    | "ja.jinxxy.com"
                    | "de.jinxxy.com"
                    | "es.jinxxy.com"
                    | "fr.jinxxy.com"
                    | "zh.jinxxy.com"
                    | "ko.jinxxy.com"
            ),
        }
    }

    impl StoredCookie {
        fn capture(cookie: Cookie<'static>, now: i64) -> Self {
            let mut expires = cookie.expires_datetime().map(|time| time.unix_timestamp());
            // A declared Max-Age becomes a fixed deadline, never a renewed TTL on load.
            if let Some(age) = cookie.max_age() {
                let deadline = now.saturating_add(age.whole_seconds());
                expires = Some(expires.map_or(deadline, |expiry| expiry.min(deadline)));
            }
            Self {
                name: cookie.name().into(),
                value: cookie.value().into(),
                domain: cookie.domain().map(str::to_owned),
                path: cookie.path().map(str::to_owned),
                secure: cookie.secure(),
                http_only: cookie.http_only(),
                same_site: cookie.same_site().map(|value| {
                    match value {
                        SameSite::Strict => "Strict",
                        SameSite::Lax => "Lax",
                        SameSite::None => "None",
                    }
                    .into()
                }),
                expires,
            }
        }

        fn validate(&self, provider: StoreProvider) -> Result<()> {
            ensure!(
                !self.name.is_empty()
                    && self.name.len() <= 256
                    && self.value.len() <= 16 * 1024
                    && self.domain.as_ref().is_none_or(
                        |domain| domain.len() <= 253 && domain_allowed(provider, domain)
                    )
                    && self
                        .path
                        .as_ref()
                        .is_none_or(|path| path.len() <= 4096 && path.starts_with('/'))
                    && self
                        .same_site
                        .as_deref()
                        .is_none_or(|site| matches!(site, "Strict" | "Lax" | "None"))
                    && !self
                        .name
                        .chars()
                        .any(|c| c.is_control() || "()<>@,;:\\\"/[]?={} \t".contains(c))
                    && !self.value.chars().any(|c| c.is_control()),
                "SESSION_CACHE_INVALID"
            );
            Ok(())
        }

        fn restore(self) -> Result<Cookie<'static>> {
            let mut cookie = Cookie::new(self.name, self.value);
            if let Some(domain) = self.domain {
                cookie.set_domain(domain);
            }
            if let Some(path) = self.path {
                cookie.set_path(path);
            }
            cookie.set_secure(self.secure);
            cookie.set_http_only(self.http_only);
            cookie.set_same_site(self.same_site.map(|site| match site.as_str() {
                "Strict" => SameSite::Strict,
                "None" => SameSite::None,
                _ => SameSite::Lax,
            }));
            if let Some(expires) = self.expires {
                cookie.set_expires(
                    OffsetDateTime::from_unix_timestamp(expires)
                        .map_err(|_| anyhow!("SESSION_CACHE_INVALID"))?,
                );
            }
            Ok(cookie)
        }
    }

    fn entropy(provider: StoreProvider, account: &str) -> Result<Vec<u8>> {
        let account =
            uuid::Uuid::parse_str(account).map_err(|_| anyhow!("SESSION_CACHE_INVALID"))?;
        Ok(format!(
            "NekoNekoAsset/native-session/v1\0{}\0{account}",
            provider.key()
        )
        .into_bytes())
    }

    // DPAPI owns this allocation. Never expose its bytes in errors or Debug output.
    struct ProtectedBuffer(CRYPT_INTEGER_BLOB);
    impl Drop for ProtectedBuffer {
        fn drop(&mut self) {
            if !self.0.pbData.is_null() {
                unsafe {
                    std::ptr::write_bytes(self.0.pbData, 0, self.0.cbData as usize);
                    let _ = LocalFree(Some(HLOCAL(self.0.pbData.cast())));
                }
            }
        }
    }

    fn crypt(bytes: &mut [u8], entropy: &mut [u8], encrypt: bool) -> Result<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_mut_ptr(),
        };
        let entropy = CRYPT_INTEGER_BLOB {
            cbData: entropy.len() as u32,
            pbData: entropy.as_mut_ptr(),
        };
        let mut output = ProtectedBuffer(CRYPT_INTEGER_BLOB::default());
        unsafe {
            let result = if encrypt {
                CryptProtectData(
                    &input,
                    PCWSTR::null(),
                    Some(&entropy),
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output.0,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    None,
                    Some(&entropy),
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output.0,
                )
            };
            result.map_err(|_| anyhow!("SESSION_CACHE_UNAVAILABLE"))?;
            ensure!(
                !output.0.pbData.is_null()
                    && output.0.cbData > 0
                    && output.0.cbData as usize <= MAX_CIPHERTEXT,
                "SESSION_CACHE_INVALID"
            );
            Ok(std::slice::from_raw_parts(output.0.pbData, output.0.cbData as usize).to_vec())
        }
    }

    fn reject_links(path: &Path) -> Result<()> {
        for ancestor in path.ancestors() {
            match fs::symlink_metadata(ancestor) {
                Ok(metadata) => ensure!(
                    metadata.file_attributes() & 0x400 == 0,
                    "SESSION_CACHE_INVALID"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(anyhow!("SESSION_CACHE_UNAVAILABLE")),
            }
        }
        Ok(())
    }

    fn install(source: &Path, target: &Path) -> Result<()> {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(source: *const u16, target: *const u16, flags: u32) -> i32;
        }
        let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let target: Vec<_> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        ensure!(
            unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 1 | 8) } != 0,
            "SESSION_CACHE_UNAVAILABLE"
        );
        Ok(())
    }

    pub(super) fn save(
        profile: &Path,
        provider: StoreProvider,
        account: &str,
        cookies: Vec<Cookie<'static>>,
    ) -> Result<()> {
        ensure!(cookies.len() <= MAX_COOKIES, "SESSION_CACHE_INVALID");
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let mut stored = Vec::new();
        for cookie in cookies {
            let cookie = StoredCookie::capture(cookie, now);
            cookie.validate(provider)?;
            if cookie.expires.is_none_or(|expires| expires > now) {
                stored.push(cookie);
            }
        }
        let mut plaintext =
            serde_json::to_vec(&stored).map_err(|_| anyhow!("SESSION_CACHE_INVALID"))?;
        ensure!(plaintext.len() <= MAX_PLAINTEXT, "SESSION_CACHE_INVALID");
        let encrypted = crypt(&mut plaintext, &mut entropy(provider, account)?, true);
        plaintext.fill(0);
        let encrypted = encrypted?;
        reject_links(profile)?;
        fs::create_dir_all(profile).map_err(|_| anyhow!("SESSION_CACHE_UNAVAILABLE"))?;
        let target = profile.join("session.bin");
        reject_links(&target)?;
        let temporary = profile.join(format!("session.{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|_| anyhow!("SESSION_CACHE_UNAVAILABLE"))?;
            file.write_all(&encrypted)
                .map_err(|_| anyhow!("SESSION_CACHE_UNAVAILABLE"))?;
            file.sync_all()
                .map_err(|_| anyhow!("SESSION_CACHE_UNAVAILABLE"))?;
            drop(file);
            install(&temporary, &target)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub(super) fn load(
        profile: &Path,
        provider: StoreProvider,
        account: &str,
    ) -> Result<Vec<Cookie<'static>>> {
        let mut binding = entropy(provider, account)?;
        let target = profile.join("session.bin");
        reject_links(&target)?;
        let file = match fs::File::open(target) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(anyhow!("SESSION_CACHE_UNAVAILABLE")),
        };
        let mut encrypted = Vec::new();
        file.take(MAX_CIPHERTEXT as u64 + 1)
            .read_to_end(&mut encrypted)
            .map_err(|_| anyhow!("SESSION_CACHE_UNAVAILABLE"))?;
        ensure!(
            !encrypted.is_empty() && encrypted.len() <= MAX_CIPHERTEXT,
            "SESSION_CACHE_INVALID"
        );
        let mut plaintext = crypt(&mut encrypted, &mut binding, false)?;
        let parsed = if plaintext.len() <= MAX_PLAINTEXT {
            serde_json::from_slice::<Vec<StoredCookie>>(&plaintext)
                .map_err(|_| anyhow!("SESSION_CACHE_INVALID"))
        } else {
            Err(anyhow!("SESSION_CACHE_INVALID"))
        };
        plaintext.fill(0);
        let stored = parsed?;
        ensure!(stored.len() <= MAX_COOKIES, "SESSION_CACHE_INVALID");
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let mut cookies = Vec::new();
        for cookie in stored {
            cookie.validate(provider)?;
            if cookie.expires.is_none_or(|expires| expires > now) {
                cookies.push(cookie.restore()?);
            }
        }
        Ok(cookies)
    }

    pub(super) fn clear(profile: &Path) -> Result<()> {
        let target = profile.join("session.bin");
        reject_links(&target)?;
        match fs::remove_file(target) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(anyhow!("SESSION_CACHE_UNAVAILABLE")),
        }
    }
}

pub fn save(
    profile: &Path,
    provider: StoreProvider,
    account: &str,
    cookies: Vec<Cookie<'static>>,
) -> Result<()> {
    #[cfg(windows)]
    {
        platform::save(profile, provider, account, cookies)
    }
    #[cfg(not(windows))]
    {
        let _ = (profile, provider, account, cookies);
        Ok(())
    }
}

pub fn load(
    profile: &Path,
    provider: StoreProvider,
    account: &str,
) -> Result<Vec<Cookie<'static>>> {
    #[cfg(windows)]
    {
        platform::load(profile, provider, account)
    }
    #[cfg(not(windows))]
    {
        let _ = (profile, provider, account);
        Ok(Vec::new())
    }
}

pub fn clear(profile: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        platform::clear(profile)
    }
    #[cfg(not(windows))]
    {
        match std::fs::remove_file(profile.join("session.bin")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(anyhow!("SESSION_CACHE_UNAVAILABLE")),
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use tauri::webview::cookie::{
        time::{Duration, OffsetDateTime},
        SameSite,
    };

    #[test]
    fn protected_cookie_snapshot_preserves_attributes_binding_expiry_and_clear() {
        let profile = std::env::temp_dir().join(format!(
            "nna-authored-session-cache-{}",
            uuid::Uuid::new_v4()
        ));
        let account = "11111111-1111-4111-8111-111111111111";
        assert!(load(&profile, StoreProvider::Gumroad, account)
            .unwrap()
            .is_empty());
        let expiry = OffsetDateTime::now_utc() + Duration::hours(1);
        let session = Cookie::build(("authored_session", "AUTHORED_COOKIE_VALUE"))
            .domain("gumroad.com")
            .path("/")
            .secure(true)
            .http_only(true)
            .same_site(SameSite::Lax)
            .build();
        let persistent = Cookie::build(("authored_persistent", "AUTHORED_PERSISTENT_VALUE"))
            .domain("gumroad.com")
            .path("/library")
            .secure(true)
            .http_only(false)
            .same_site(SameSite::Strict)
            .expires(expiry)
            .build();
        let expired = Cookie::build(("authored_expired", "AUTHORED_EXPIRED_VALUE"))
            .domain("gumroad.com")
            .path("/")
            .expires(OffsetDateTime::now_utc() - Duration::hours(1))
            .build();
        let max_age = Cookie::build(("authored_max_age", "AUTHORED_MAX_AGE_VALUE"))
            .domain("gumroad.com")
            .path("/")
            .max_age(Duration::minutes(5))
            .build();
        let deadline = OffsetDateTime::now_utc().unix_timestamp() + 300;
        save(
            &profile,
            StoreProvider::Gumroad,
            account,
            vec![session, persistent, expired, max_age],
        )
        .unwrap();
        let encrypted = std::fs::read(profile.join("session.bin")).unwrap();
        assert!(!encrypted
            .windows(b"AUTHORED_COOKIE_VALUE".len())
            .any(|bytes| bytes == b"AUTHORED_COOKIE_VALUE"));
        let loaded = load(&profile, StoreProvider::Gumroad, account).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].value(), "AUTHORED_COOKIE_VALUE");
        assert!(loaded[0].expires_datetime().is_none());
        assert_eq!(loaded[0].domain(), Some("gumroad.com"));
        assert_eq!(loaded[0].path(), Some("/"));
        assert_eq!(loaded[0].secure(), Some(true));
        assert_eq!(loaded[0].http_only(), Some(true));
        assert_eq!(loaded[0].same_site(), Some(SameSite::Lax));
        assert_eq!(
            loaded[1].expires_datetime().unwrap().unix_timestamp(),
            expiry.unix_timestamp()
        );
        assert_eq!(loaded[1].path(), Some("/library"));
        assert_eq!(loaded[1].same_site(), Some(SameSite::Strict));
        assert!(loaded[2].max_age().is_none());
        let restored_deadline = loaded[2].expires_datetime().unwrap().unix_timestamp();
        assert!((deadline..=deadline + 2).contains(&restored_deadline));
        assert_eq!(
            load(&profile, StoreProvider::Gumroad, account).unwrap()[2]
                .expires_datetime()
                .unwrap()
                .unix_timestamp(),
            restored_deadline
        );
        assert!(load(&profile, StoreProvider::Jinxxy, account).is_err());
        assert!(load(
            &profile,
            StoreProvider::Gumroad,
            "22222222-2222-4222-8222-222222222222"
        )
        .is_err());
        save(
            &profile,
            StoreProvider::Gumroad,
            account,
            vec![
                Cookie::build(("authored_replacement", "AUTHORED_REPLACEMENT"))
                    .domain("gumroad.com")
                    .path("/")
                    .build(),
            ],
        )
        .unwrap();
        assert_eq!(
            load(&profile, StoreProvider::Gumroad, account)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(std::fs::read_dir(&profile).unwrap().count(), 1);
        clear(&profile).unwrap();
        assert!(load(&profile, StoreProvider::Gumroad, account)
            .unwrap()
            .is_empty());
        clear(&profile).unwrap();
        std::fs::remove_dir(profile).unwrap();
    }
}
