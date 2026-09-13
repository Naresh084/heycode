//! Explicit, bounded GitHub release updates for standalone Unix installations.
//! GitHub's HTTPS API supplies the asset digest; this is not Sigstore verification.

use anyhow::{Context, Result, bail, ensure};
use heycode_http::{HttpRequest, HttpTransport, ReqwestHttpTransport};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use tokio_util::sync::CancellationToken;

const REPOSITORY: &str = "Naresh084/heycode";
const MAX_BINARY: usize = 128 * 1024 * 1024;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    size: usize,
}

fn version(text: &str) -> Result<(u64, u64, u64)> {
    let parts = text.strip_prefix('v').unwrap_or(text).split('.').collect::<Vec<_>>();
    ensure!(
        parts.len() == 3,
        "expected a stable major.minor.patch version"
    );
    Ok((parts[0].parse()?, parts[1].parse()?, parts[2].parse()?))
}

fn platform() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("macos-aarch64"),
        ("macos", "x86_64") => Ok("macos-x86_64"),
        ("linux", "x86_64") => Ok("linux-x86_64"),
        _ => bail!("automatic updates are unavailable on this platform; see GitHub Releases"),
    }
}

fn allowed_url(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && matches!(
                url.host_str(),
                Some(
                    "api.github.com"
                        | "github.com"
                        | "release-assets.githubusercontent.com"
                        | "objects.githubusercontent.com"
                )
            )
    })
}

async fn download(
    transport: &ReqwestHttpTransport,
    initial: &str,
    limit: usize,
) -> Result<Vec<u8>> {
    let mut url = initial.to_owned();
    for _ in 0..4 {
        ensure!(
            allowed_url(&url),
            "release download left the allowed HTTPS hosts"
        );
        let request = HttpRequest::get(&url)?
            .header("User-Agent", "heycode-updater")?
            .with_max_response_bytes(limit);
        let response = transport.send(request, CancellationToken::new()).await?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            url = response
                .headers
                .get("location")
                .context("release redirect has no location")?
                .clone();
            continue;
        }
        ensure!(
            response.status == 200,
            "GitHub release download returned HTTP {}",
            response.status
        );
        return Ok(response.body);
    }
    bail!("too many release redirects")
}

fn verify(bytes: &[u8], asset: &Asset) -> Result<()> {
    ensure!(
        !bytes.is_empty() && bytes.len() == asset.size && bytes.len() <= MAX_BINARY,
        "release asset size mismatch"
    );
    let actual = format!("sha256:{:x}", Sha256::digest(bytes));
    ensure!(
        asset.digest.as_deref() == Some(actual.as_str()),
        "release asset SHA-256 mismatch or missing digest; installation unchanged"
    );
    Ok(())
}

fn replace(executable: &Path, bytes: &[u8]) -> Result<()> {
    let parent = executable
        .parent()
        .context("executable has no installation directory")?;
    let lock_path = parent.join(".heycode-update.lock");
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .context("could not lock installation; another update may be running")?;
    let result = (|| -> Result<()> {
        let metadata = std::fs::metadata(executable)?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        staged.write_all(bytes)?;
        staged.as_file().set_permissions(metadata.permissions())?;
        staged.as_file().sync_all()?;
        let mut backup = tempfile::NamedTempFile::new_in(parent)?;
        std::io::copy(&mut std::fs::File::open(executable)?, &mut backup)?;
        backup.as_file().set_permissions(metadata.permissions())?;
        backup.as_file().sync_all()?;
        backup.persist(parent.join("heycode.previous"))?;
        staged.persist(executable)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    drop(lock);
    let cleanup = std::fs::remove_file(lock_path);
    result?;
    cleanup?;
    Ok(())
}

/// Check or install the latest stable release. Never called by normal startup.
/// # Errors
/// Refuses unsupported platforms, invalid metadata, failed downloads or unsafe replacements.
pub fn run(args: &[String]) -> Result<String> {
    ensure!(
        args.is_empty() || args == ["--check"],
        "usage: heycode update [--check]"
    );
    let asset_name = format!("heycode-{}", platform()?);
    let executable = std::env::current_exe()?.canonicalize()?;
    ensure!(
        executable.file_name().is_some_and(|name| name == "heycode"),
        "update only supports an executable named heycode"
    );
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
        let transport = ReqwestHttpTransport::with_timeout(std::time::Duration::from_secs(120))?;
        let bytes = download(&transport, &format!("https://api.github.com/repos/{REPOSITORY}/releases/latest"), 1024 * 1024).await?;
        let release: Release = serde_json::from_slice(&bytes)?;
        ensure!(!release.draft && !release.prerelease, "expected a published stable release");
        let latest = version(&release.tag_name)?;
        let current = version(env!("CARGO_PKG_VERSION"))?;
        if latest <= current {
            return Ok(format!("HeyCode {} is up to date.", env!("CARGO_PKG_VERSION")));
        }
        if !args.is_empty() {
            return Ok(format!("HeyCode {} is available. Run `heycode update` to install it.", release.tag_name));
        }
        let asset = release.assets.iter().find(|asset| asset.name == asset_name).context("this release has no binary for your platform")?;
        ensure!(asset.size > 0 && asset.size <= MAX_BINARY, "release asset exceeds download limit");
        let expected_url = format!("https://github.com/{REPOSITORY}/releases/download/{}/{asset_name}", release.tag_name);
        ensure!(asset.browser_download_url == expected_url, "unexpected release asset location");
        let binary = download(&transport, &expected_url, MAX_BINARY).await?;
        verify(&binary, asset)?;
        replace(&executable, &binary)?;
        Ok(format!("Installed HeyCode {}. The previous executable is saved as heycode.previous. Restart HeyCode to use the new version.", release.tag_name))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn release_downloads_have_strict_origins() {
        assert!(allowed_url(
            "https://release-assets.githubusercontent.com/file?signature=example"
        ));
        for url in [
            "http://github.com/a",
            "https://github.com.evil.example/a",
            "https://user@github.com/a",
            "file:///tmp/a",
        ] {
            assert!(!allowed_url(url));
        }
        assert!(version("1.2.3-preview.1").is_err());
        assert!(version("v1.10.0").unwrap() > version("1.9.0").unwrap());
    }
    #[test]
    fn corrupt_asset_never_replaces_installed_file() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("heycode");
        std::fs::write(&executable, b"previous").unwrap();
        let asset = Asset {
            name: "test".into(),
            browser_download_url: String::new(),
            digest: Some(format!("sha256:{:x}", Sha256::digest(b"new"))),
            size: 3,
        };
        assert!(verify(b"bad", &asset).is_err());
        assert_eq!(std::fs::read(&executable).unwrap(), b"previous");
        verify(b"new", &asset).unwrap();
        replace(&executable, b"new").unwrap();
        assert_eq!(std::fs::read(&executable).unwrap(), b"new");
        assert_eq!(
            std::fs::read(directory.path().join("heycode.previous")).unwrap(),
            b"previous"
        );
        assert!(!directory.path().join(".heycode-update.lock").exists());
    }
}

/// Start a best-effort automatic update for installer-managed interactive launches.
/// Development builds and explicitly disabled installations never contact GitHub.
pub fn start_automatic() {
    if std::env::var_os("HEYCODE_AUTO_UPDATE").is_some_and(|value| value == "0") {
        return;
    }
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let Some(directory) = executable.parent() else {
        return;
    };
    if !directory.join(".heycode-install").is_file() {
        return;
    }
    let Ok(Some(home)) = heycode_config::home_root() else {
        return;
    };
    let _ = std::thread::spawn(move || {
        let checked = home.join("update-checked");
        loop {
            let elapsed = std::fs::metadata(&checked).and_then(|metadata| metadata.modified()).ok()
                .and_then(|time| time.elapsed().ok()).map_or(3600, |age| age.as_secs());
            if elapsed < 3600 {
                std::thread::sleep(std::time::Duration::from_secs(3600 - elapsed));
                continue;
            }
            if std::fs::create_dir_all(&home).is_err() || std::fs::write(&checked, b"").is_err() {
                return;
            }
            let status = match run(&[]) {
                Ok(message) => message,
                Err(_) => "Automatic update could not confirm completion; another check will run later. Use heycode update for details.".to_owned(),
            };
            let installed = status.starts_with("Installed HeyCode ");
            let _ = std::fs::write(home.join("update-status.txt"), status);
            // This process still has the old version compiled in. Stop after a
            // replacement so it cannot repeatedly reinstall and overwrite rollback.
            if installed { return; }
        }
    });
}
