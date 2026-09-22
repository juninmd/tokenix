use anyhow::{anyhow, Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const GITHUB_REPO: &str = "juninmd/tokenix";
const USER_AGENT: &str = concat!("tokenix/", env!("CARGO_PKG_VERSION"));
const CACHE_TTL_SECS: u64 = 86400; // 24 hours

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let clean = s.trim().strip_prefix('v').unwrap_or(s.trim());
        let (core, pre) = match clean.split_once('-') {
            Some((c, p)) => (c, Some(p.to_string())),
            None => (clean, None),
        };
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() < 3 {
            return None;
        }
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        let patch = parts[2].parse().ok()?;
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.major.cmp(&other.major) {
            std::cmp::Ordering::Equal => match self.minor.cmp(&other.minor) {
                std::cmp::Ordering::Equal => match self.patch.cmp(&other.patch) {
                    std::cmp::Ordering::Equal => match (&self.pre, &other.pre) {
                        (None, None) => std::cmp::Ordering::Equal,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (Some(a), Some(b)) => a.cmp(b),
                    },
                    other => other,
                },
                other => other,
            },
            other => other,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GithubRelease {
    pub tag_name: String,
    pub html_url: String,
    #[allow(dead_code)]
    pub body: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub prerelease: bool,
    #[serde(default)]
    #[allow(dead_code)]
    pub draft: bool,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UpdateCache {
    pub last_checked_epoch: u64,
    pub current_version: String,
    pub latest_version: String,
    pub release_url: String,
    #[serde(default = "default_true")]
    pub auto_install: bool,
    #[serde(default)]
    pub auto_install_override: Option<bool>,
    #[serde(default)]
    pub just_updated: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct UpdateStatusJson {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release_url: String,
    pub asset_name: String,
    pub target_path: String,
    pub updated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_install: Option<bool>,
}

pub fn get_target_asset_name(directml: bool) -> Result<&'static str> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    match (os, arch) {
        ("linux", "x86_64") => Ok("tokenix-linux-x86_64"),
        ("linux", "aarch64") => Ok("tokenix-linux-aarch64"),
        ("macos", "aarch64") => Ok("tokenix-macos-aarch64"),
        ("windows", "x86_64") => {
            if directml || cfg!(feature = "directml") {
                Ok("tokenix-windows-x86_64-directml.exe")
            } else {
                Ok("tokenix-windows-x86_64.exe")
            }
        }
        _ => anyhow::bail!(
            "Unsupported platform for prebuilt binary: {os}-{arch}. Please update via `cargo install --locked tokenix`"
        ),
    }
}

fn create_client_with_timeout(timeout: Duration) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()?)
}

fn create_client() -> Result<reqwest::blocking::Client> {
    create_client_with_timeout(Duration::from_secs(45))
}

fn create_api_client_with_timeout(timeout: Duration) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        // An API redirect must never carry an optional GitHub token to a
        // different origin. Release asset downloads use the separate client.
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

fn github_api_request(
    client: &reqwest::blocking::Client,
    url: &str,
    token: Option<&str>,
) -> reqwest::blocking::RequestBuilder {
    let request = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github.v3+json");
    match token
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| reqwest::header::HeaderValue::from_str(&format!("Bearer {value}")).ok())
    {
        Some(mut value) => {
            value.set_sensitive(true);
            request.header(reqwest::header::AUTHORIZATION, value)
        }
        None => request,
    }
}

pub fn fetch_release(
    client: &reqwest::blocking::Client,
    specific_version: Option<&str>,
) -> Result<GithubRelease> {
    let url = match specific_version {
        Some(v) => {
            let tag = if v.starts_with('v') {
                v.to_string()
            } else {
                format!("v{v}")
            };
            format!("https://api.github.com/repos/{GITHUB_REPO}/releases/tags/{tag}")
        }
        None => format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest"),
    };

    let token = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok();
    let res = github_api_request(client, &url, token.as_deref())
        .send()
        .with_context(|| format!("Failed to connect to GitHub releases at {url}"))?;

    if !res.status().is_success() {
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("Release not found at {url}");
        }
        anyhow::bail!(
            "GitHub API error (HTTP {}): {}",
            res.status(),
            res.text().unwrap_or_default()
        );
    }

    let release: GithubRelease = res
        .json()
        .with_context(|| "Failed to parse GitHub release JSON")?;
    Ok(release)
}

pub fn parse_sha256sums(content: &str, target_asset: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() >= 2 {
            let hash = parts[0];
            let filename = parts[1].strip_prefix('*').unwrap_or(parts[1]);
            if filename == target_asset {
                return Some(hash.to_lowercase());
            }
        }
    }
    None
}

fn verified_release_asset_url(
    release: &GithubRelease,
    asset: &ReleaseAsset,
) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(&asset.browser_download_url)?;
    let expected_path = format!(
        "/{GITHUB_REPO}/releases/download/{}/{}",
        release.tag_name, asset.name
    );
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != expected_path
        || url.query().is_some()
        || url.fragment().is_some()
    {
        anyhow::bail!("Unexpected GitHub release asset URL for {}", asset.name);
    }
    Ok(url)
}

pub fn resolve_target_path(custom_path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = custom_path {
        if let Some(parent) = p.parent() {
            clean_stale_old_files(parent);
        }
        return Ok(p);
    }

    let current = std::env::current_exe()?;
    let current_str = current.to_string_lossy();

    if let Some(parent) = current.parent() {
        clean_stale_old_files(parent);
    }

    // If running from a build tree (cargo run / target/debug or target/release), default to global bin dir
    if current_str.contains("target/debug")
        || current_str.contains("target\\debug")
        || current_str.contains("target/release")
        || current_str.contains("target\\release")
    {
        if let Some(bin_dir) = crate::global_bin_dir() {
            let binary_name = if cfg!(windows) {
                "tokenix.exe"
            } else {
                "tokenix"
            };
            return Ok(bin_dir.join(binary_name));
        }
    }

    Ok(current)
}

fn cache_path() -> Option<PathBuf> {
    crate::store::global_dir().map(|h| h.join("update_cache.json"))
}

fn check_lock_path() -> Option<PathBuf> {
    crate::store::global_dir().map(|h| h.join("update_check.lock"))
}

pub fn load_update_cache() -> Option<UpdateCache> {
    let path = cache_path()?;
    let file = File::open(path).ok()?;
    serde_json::from_reader(file).ok()
}

pub fn save_cache_struct(cache: &UpdateCache) -> Result<()> {
    if let Some(path) = cache_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
            crate::store::restrict_to_owner(parent);
        }
        let mut f = File::create(&path)?;
        serde_json::to_writer_pretty(&mut f, cache)?;
        crate::store::restrict_to_owner(&path);
    }
    Ok(())
}

pub fn save_update_cache(latest_version: &str, release_url: &str) {
    let existing = load_update_cache();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let cache = UpdateCache {
        last_checked_epoch: now,
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        latest_version: latest_version.to_string(),
        release_url: release_url.to_string(),
        auto_install: existing.as_ref().map(|c| c.auto_install).unwrap_or(true),
        auto_install_override: existing.and_then(|c| c.auto_install_override),
        just_updated: false,
    };

    let _ = save_cache_struct(&cache);
}

pub fn set_auto_install(enabled: bool) -> Result<()> {
    let path = cache_path()
        .ok_or_else(|| anyhow!("Cannot determine global directory for update cache"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        crate::store::restrict_to_owner(parent);
    }
    let mut cache = load_update_cache().unwrap_or_else(|| UpdateCache {
        last_checked_epoch: 0,
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        latest_version: env!("CARGO_PKG_VERSION").to_string(),
        release_url: String::new(),
        auto_install: enabled,
        auto_install_override: Some(enabled),
        just_updated: false,
    });
    cache.auto_install = enabled;
    cache.auto_install_override = Some(enabled);
    save_cache_struct(&cache)
}

pub fn is_check_enabled() -> bool {
    if std::env::var("TOKENIX_NO_UPDATE").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        return false;
    }
    if std::env::var("CI").is_ok_and(|v| v == "true" || v == "1") {
        return false;
    }
    if let Ok(val) = std::env::var("TOKENIX_AUTO_UPDATE") {
        if val == "0" || val.eq_ignore_ascii_case("false") || val.eq_ignore_ascii_case("off") {
            return false;
        }
    }
    let cfg = crate::chunker::update_config();
    if let Some(check) = cfg.check {
        return check;
    }
    true
}

pub fn is_auto_install_enabled() -> bool {
    if !is_check_enabled() {
        return false;
    }
    if let Ok(val) = std::env::var("TOKENIX_AUTO_UPDATE") {
        if val == "0"
            || val.eq_ignore_ascii_case("false")
            || val.eq_ignore_ascii_case("off")
            || val.eq_ignore_ascii_case("no")
            || val.eq_ignore_ascii_case("notify")
        {
            return false;
        }
        if val == "1"
            || val.eq_ignore_ascii_case("true")
            || val.eq_ignore_ascii_case("auto")
            || val.eq_ignore_ascii_case("install")
        {
            return true;
        }
    }
    if let Some(override_value) = load_update_cache().and_then(|c| c.auto_install_override) {
        return override_value;
    }
    let cfg = crate::chunker::update_config();
    if let Some(auto) = cfg.auto_install {
        return auto;
    }
    load_update_cache().map(|c| c.auto_install).unwrap_or(true)
}

pub fn check_interval_secs() -> u64 {
    let hours = crate::chunker::update_config()
        .interval_hours
        .unwrap_or(CACHE_TTL_SECS / 3600);
    hours.max(1).saturating_mul(3600)
}

pub fn spawn_background_check() {
    if !is_check_enabled() {
        return;
    }

    let Some(lock) = check_lock_path() else {
        return;
    };
    if let Some(parent) = lock.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        crate::store::restrict_to_owner(parent);
    }
    // create_new makes the daily check single-flight across simultaneous CLI
    // launches. A crash cannot hold it forever: a later launch removes stale locks.
    let acquire = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
    };
    if acquire().is_err() {
        let stale = std::fs::metadata(&lock)
            .and_then(|m| m.modified())
            .and_then(|t| t.elapsed().map_err(std::io::Error::other))
            .is_ok_and(|elapsed| elapsed >= Duration::from_secs(600));
        if !stale {
            return;
        }
        let _ = std::fs::remove_file(&lock);
        if acquire().is_err() {
            return;
        }
    }
    crate::store::restrict_to_owner(&lock);

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => {
            let _ = std::fs::remove_file(&lock);
            return;
        }
    };

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("update")
        .arg("--check-background")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x00000008 | 0x08000000);
    }

    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt as _;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    if cmd.spawn().is_err() {
        let _ = std::fs::remove_file(&lock);
    }
}

/// Quick check that reads local cache and displays hint, triggering a background check if cache is stale
pub fn maybe_print_update_hint() {
    if !is_check_enabled() {
        return;
    }

    let current_ver_str = env!("CARGO_PKG_VERSION");
    let current_ver = match Version::parse(current_ver_str) {
        Some(v) => v,
        None => return,
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let cache = load_update_cache();

    // If an update was automatically installed, notify the user once and reset the flag
    if let Some(mut c) = cache.clone() {
        if c.just_updated {
            eprintln!(
                "{} tokenix was automatically updated to {}.",
                "✓".green().bold(),
                current_ver_str.bold()
            );
            c.just_updated = false;
            let _ = save_cache_struct(&c);
            return;
        }
    }

    let ttl = check_interval_secs();
    let is_stale = cache
        .as_ref()
        .map(|c| now.saturating_sub(c.last_checked_epoch) > ttl)
        .unwrap_or(true);

    if is_stale {
        spawn_background_check();
    }

    if let Some(c) = cache {
        if let Some(latest_ver) = Version::parse(&c.latest_version) {
            if latest_ver > current_ver {
                if is_auto_install_enabled() {
                    eprintln!(
                        "{} A newer tokenix is available: {} -> {}. Automatic updates are enabled.",
                        "tip:".cyan(),
                        current_ver_str,
                        c.latest_version.yellow()
                    );
                } else {
                    eprintln!(
                        "{} A newer tokenix is available: {} -> {}. Run '{}' to update.",
                        "tip:".cyan(),
                        current_ver_str,
                        c.latest_version.yellow(),
                        "tokenix update".bold()
                    );
                }
            }
        }
    }
}

pub fn clean_stale_old_files(target_dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(target_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                let old_exe = name
                    .strip_prefix("tokenix.exe.old.")
                    .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()));
                let download = name
                    .strip_prefix("tokenix_download_")
                    .and_then(|rest| rest.strip_suffix(".tmp"))
                    .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()));
                if (old_exe || download)
                    && std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .and_then(|t| t.elapsed().map_err(std::io::Error::other))
                        .is_ok_and(|age| age >= Duration::from_secs(86400))
                {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

fn replace_executable(temp_path: &Path, target_path: &Path) -> Result<()> {
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
        clean_stale_old_files(parent);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(temp_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(temp_path, perms)?;
        std::fs::rename(temp_path, target_path).with_context(|| {
            format!(
                "Failed to move new binary {} to {}",
                temp_path.display(),
                target_path.display()
            )
        })?;
    }

    #[cfg(windows)]
    {
        if target_path.exists() {
            let pid = std::process::id();
            let old_path = target_path.with_file_name(format!("tokenix.exe.old.{}", pid));

            // Rename running executable to .old
            std::fs::rename(target_path, &old_path).with_context(|| {
                format!(
                    "Failed to rename current executable {} to temporary .old backup",
                    target_path.display()
                )
            })?;

            // Move new executable into place
            if let Err(e) = std::fs::rename(temp_path, target_path) {
                // Try rolling back if moving the new binary fails
                let _ = std::fs::rename(&old_path, target_path);
                return Err(anyhow!("Failed to install new binary: {e}"));
            }

            // Attempt to remove .old file immediately (succeeds if not locked, ignored otherwise)
            let _ = std::fs::remove_file(&old_path);
        } else {
            std::fs::rename(temp_path, target_path).with_context(|| {
                format!(
                    "Failed to install new binary {} to {}",
                    temp_path.display(),
                    target_path.display()
                )
            })?;
        }
    }

    Ok(())
}

fn download_and_install_release(
    client: &reqwest::blocking::Client,
    release: &GithubRelease,
    directml: bool,
    target_path: &Path,
    quiet: bool,
) -> Result<String> {
    let asset_name = get_target_asset_name(directml)?;

    let binary_asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| {
            anyhow!(
                "Asset '{asset_name}' not found in release {}. Available assets: {:?}",
                release.tag_name,
                release.assets.iter().map(|a| &a.name).collect::<Vec<_>>()
            )
        })?;

    let checksum_asset = release
        .assets
        .iter()
        .find(|a| a.name == "sha256sums.txt")
        .ok_or_else(|| anyhow!("'sha256sums.txt' not found in release {}", release.tag_name))?;
    let checksum_url = verified_release_asset_url(release, checksum_asset)?;
    let binary_url = verified_release_asset_url(release, binary_asset)?;

    if !quiet {
        println!("Fetching sha256sums.txt...");
    }
    let checksums_raw = client
        .get(checksum_url)
        .send()
        .with_context(|| "Failed to download sha256sums.txt")?
        .error_for_status()
        .with_context(|| "GitHub returned an error for sha256sums.txt")?
        .text()
        .with_context(|| "Failed to read sha256sums.txt content")?;

    let expected_hash = parse_sha256sums(&checksums_raw, asset_name)
        .ok_or_else(|| anyhow!("Asset '{asset_name}' not found in release checksums file"))?;

    let target_dir = target_path
        .parent()
        .ok_or_else(|| anyhow!("Target path has no parent directory"))?;
    std::fs::create_dir_all(target_dir)?;

    let temp_file_path = target_dir.join(format!("tokenix_download_{}.tmp", std::process::id()));

    if !quiet {
        println!(
            "Downloading {} ({} MB)...",
            asset_name.cyan(),
            binary_asset.size / (1024 * 1024)
        );
    }

    let mut response = client.get(binary_url).send().with_context(|| {
        format!(
            "Failed to download asset from {}",
            binary_asset.browser_download_url
        )
    })?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed with HTTP {}", response.status());
    }

    let mut temp_file = File::create(&temp_file_path).with_context(|| {
        format!(
            "Failed to create temporary file at {}",
            temp_file_path.display()
        )
    })?;

    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let bytes_read = response
            .read(&mut buffer)
            .with_context(|| "Error reading download stream")?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
        temp_file
            .write_all(&buffer[..bytes_read])
            .with_context(|| "Error writing to temporary file")?;
    }
    temp_file.flush()?;
    drop(temp_file);

    let calculated_hash = hex::encode(hasher.finalize());
    if calculated_hash != expected_hash {
        let _ = std::fs::remove_file(&temp_file_path);
        anyhow::bail!(
            "SHA-256 checksum mismatch!\nExpected:   {}\nCalculated: {}",
            expected_hash,
            calculated_hash
        );
    }

    if !quiet {
        println!("{} Checksum verified: {}", "✓".green(), calculated_hash);
        println!("Installing to {}...", target_path.display());
    }

    if let Err(e) = replace_executable(&temp_file_path, target_path) {
        let _ = std::fs::remove_file(&temp_file_path);
        return Err(e);
    }

    Ok(asset_name.to_string())
}

pub fn run_background_check_and_auto_update() -> Result<()> {
    struct CheckLockGuard(Option<PathBuf>);
    impl Drop for CheckLockGuard {
        fn drop(&mut self) {
            if let Some(path) = &self.0 {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    let _lock_guard = CheckLockGuard(check_lock_path());

    if !is_check_enabled() {
        return Ok(());
    }

    let current_version_str = env!("CARGO_PKG_VERSION");
    let current_version = match Version::parse(current_version_str) {
        Some(v) => v,
        None => return Ok(()),
    };

    let api_client = match create_api_client_with_timeout(Duration::from_secs(15)) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    let client = match create_client_with_timeout(Duration::from_secs(45)) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    let release = match fetch_release(&api_client, None) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };

    let latest_version_str = release.tag_name.trim_start_matches('v');
    let latest_version = match Version::parse(latest_version_str) {
        Some(v) => v,
        None => return Ok(()),
    };

    save_update_cache(&release.tag_name, &release.html_url);

    let is_newer = latest_version > current_version;
    if is_newer && is_auto_install_enabled() {
        let directml = cfg!(feature = "directml");
        let target_path = match resolve_target_path(None) {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        // A `cargo run` build resolves to the per-user bin directory for manual
        // updates. Never silently install there from a development executable:
        // the invoked binary would stay old and another installation could be
        // overwritten.
        if std::env::current_exe().is_ok_and(|exe| exe != target_path) {
            return Ok(());
        }
        if download_and_install_release(&client, &release, directml, &target_path, true).is_ok() {
            let updated_cache = UpdateCache {
                last_checked_epoch: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                current_version: latest_version_str.to_string(),
                latest_version: latest_version_str.to_string(),
                release_url: release.html_url,
                auto_install: true,
                auto_install_override: load_update_cache().and_then(|c| c.auto_install_override),
                just_updated: true,
            };
            let _ = save_cache_struct(&updated_cache);
        }
    }

    Ok(())
}

pub struct UpdateOptions {
    pub check: bool,
    pub force: bool,
    pub auto: bool,
    pub enable_auto: bool,
    pub disable_auto: bool,
    pub json: bool,
    pub directml: bool,
    pub version: Option<String>,
    pub target_path: Option<PathBuf>,
    pub check_background: bool,
}

pub fn run_update(opts: UpdateOptions) -> Result<()> {
    if opts.check_background {
        return run_background_check_and_auto_update();
    }

    if opts.enable_auto {
        set_auto_install(true)?;
        if opts.json {
            println!("{}", serde_json::json!({ "auto_install": true }));
        } else {
            println!(
                "{} Automatic updates enabled. tokenix will install updates automatically in the background.",
                "✓".green().bold()
            );
        }
        return Ok(());
    }

    if opts.disable_auto {
        set_auto_install(false)?;
        if opts.json {
            println!("{}", serde_json::json!({ "auto_install": false }));
        } else {
            println!("{} Automatic updates disabled.", "✓".green().bold());
        }
        return Ok(());
    }

    let current_version_str = env!("CARGO_PKG_VERSION");
    let current_version = Version::parse(current_version_str)
        .ok_or_else(|| anyhow!("Failed to parse current version '{current_version_str}'"))?;

    let client = create_client()?;
    let api_client = create_api_client_with_timeout(Duration::from_secs(45))?;
    let release = fetch_release(&api_client, opts.version.as_deref())?;

    let latest_version_str = release.tag_name.trim_start_matches('v');
    let latest_version = Version::parse(latest_version_str)
        .ok_or_else(|| anyhow!("Failed to parse release version '{}'", release.tag_name))?;

    save_update_cache(&release.tag_name, &release.html_url);

    let is_newer = latest_version > current_version;
    let should_update = is_newer || opts.force;

    let asset_name = get_target_asset_name(opts.directml)?;
    let target_path = resolve_target_path(opts.target_path)?;

    if opts.json {
        let status = UpdateStatusJson {
            current_version: current_version_str.to_string(),
            latest_version: latest_version_str.to_string(),
            update_available: is_newer,
            release_url: release.html_url.clone(),
            asset_name: asset_name.to_string(),
            target_path: target_path.display().to_string(),
            updated: false,
            auto_install: Some(is_auto_install_enabled()),
        };
        if opts.check || !should_update {
            println!("{}", serde_json::to_string_pretty(&status)?);
            return Ok(());
        }
    }

    if !opts.json {
        println!("Current version: {}", current_version_str.bold());
        println!(
            "Latest release:  {} ({})",
            release.tag_name.bold().green(),
            release.html_url
        );
        if is_auto_install_enabled() {
            println!("Auto-update:     {}", "enabled".green());
        }
    }

    if opts.check {
        if is_newer {
            if !opts.json {
                println!(
                    "{} Update available: {} -> {}. Run 'tokenix update' to upgrade.",
                    "✓".green(),
                    current_version_str,
                    release.tag_name.bold()
                );
            }
        } else if !opts.json {
            println!("{} tokenix is already up to date.", "✓".green());
        }
        return Ok(());
    }

    if opts.auto && !should_update {
        if !opts.json {
            println!("{} tokenix is already up to date.", "✓".green());
        }
        return Ok(());
    }

    if !should_update {
        if !opts.json {
            println!(
                "{} tokenix is already up to date. Use '--force' to reinstall.",
                "✓".green()
            );
        }
        return Ok(());
    }

    let installed_asset =
        download_and_install_release(&client, &release, opts.directml, &target_path, opts.json)?;

    if opts.json {
        let status = UpdateStatusJson {
            current_version: current_version_str.to_string(),
            latest_version: latest_version_str.to_string(),
            update_available: false,
            release_url: release.html_url,
            asset_name: installed_asset,
            target_path: target_path.display().to_string(),
            updated: true,
            auto_install: Some(is_auto_install_enabled()),
        };
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!(
            "{} Successfully updated tokenix to {} at {}",
            "✓".green().bold(),
            release.tag_name.bold(),
            target_path.display()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_parse_and_compare() {
        let v1 = Version::parse("0.64.0").unwrap();
        let v2 = Version::parse("v0.64.1").unwrap();
        let v3 = Version::parse("0.65.0-rc1").unwrap();
        let v4 = Version::parse("v0.65.0").unwrap();

        assert!(v2 > v1);
        assert!(v3 > v2);
        assert!(v4 > v3);
        assert_eq!(v1, Version::parse("v0.64.0").unwrap());
    }

    #[test]
    fn test_parse_sha256sums() {
        let sample = r#"
# Checksums for tokenix release
90cc3cbc08dd5198bea161c08c9774e5ed33329ee05e863e9b6e1b5504cd42f8  tokenix-linux-aarch64
9e3611f841691188274ef6a94906720d61f6f14b8bed686d66f1f66905f13e0d  tokenix-linux-x86_64
bc0f1a0cb0050f79d6cbc336521d759aaf080fa8095ea2a32e49425d6ce7ddde *tokenix-windows-x86_64.exe
"#;
        assert_eq!(
            parse_sha256sums(sample, "tokenix-linux-x86_64"),
            Some("9e3611f841691188274ef6a94906720d61f6f14b8bed686d66f1f66905f13e0d".to_string())
        );
        assert_eq!(
            parse_sha256sums(sample, "tokenix-windows-x86_64.exe"),
            Some("bc0f1a0cb0050f79d6cbc336521d759aaf080fa8095ea2a32e49425d6ce7ddde".to_string())
        );
        assert_eq!(parse_sha256sums(sample, "non-existent"), None);
    }

    #[test]
    fn github_token_is_only_attached_to_the_api_request() {
        let api_client = create_api_client_with_timeout(Duration::from_secs(1)).unwrap();
        let asset_client = create_client_with_timeout(Duration::from_secs(1)).unwrap();
        let api_request = github_api_request(
            &api_client,
            "https://api.github.com/repos/juninmd/tokenix/releases/latest",
            Some("test-token"),
        )
        .build()
        .unwrap();
        assert_eq!(
            api_request.headers()[reqwest::header::AUTHORIZATION],
            "Bearer test-token"
        );
        let asset_request = asset_client
            .get("https://github.com/juninmd/tokenix/releases/download/v1.0.0/tokenix.exe")
            .build()
            .unwrap();
        assert!(!asset_request
            .headers()
            .contains_key(reqwest::header::AUTHORIZATION));
    }

    #[test]
    fn release_asset_urls_are_limited_to_this_repositories_https_release() {
        let release = GithubRelease {
            tag_name: "v1.0.0".to_string(),
            html_url: String::new(),
            body: None,
            prerelease: false,
            draft: false,
            assets: Vec::new(),
        };
        let mut asset = ReleaseAsset {
            name: "tokenix-windows-x86_64.exe".to_string(),
            browser_download_url: "https://github.com/juninmd/tokenix/releases/download/v1.0.0/tokenix-windows-x86_64.exe".to_string(),
            size: 1,
        };
        assert!(verified_release_asset_url(&release, &asset).is_ok());
        for bad in [
            "http://github.com/juninmd/tokenix/releases/download/v1.0.0/tokenix-windows-x86_64.exe",
            "https://example.com/juninmd/tokenix/releases/download/v1.0.0/tokenix-windows-x86_64.exe",
            "https://github.com/other/tokenix/releases/download/v1.0.0/tokenix-windows-x86_64.exe",
            "https://github.com/juninmd/tokenix/releases/download/v1.0.0/tokenix-windows-x86_64.exe?token=secret",
        ] {
            asset.browser_download_url = bad.to_string();
            assert!(verified_release_asset_url(&release, &asset).is_err(), "{bad}");
        }
    }

    #[test]
    fn test_target_asset_name() {
        let asset = get_target_asset_name(false);
        assert!(asset.is_ok());
        let name = asset.unwrap();
        assert!(name.starts_with("tokenix-"));
    }

    #[test]
    fn test_update_cache_roundtrip() {
        let cache = UpdateCache {
            last_checked_epoch: 123456789,
            current_version: "0.64.0".to_string(),
            latest_version: "0.65.0".to_string(),
            release_url: "https://github.com/juninmd/tokenix/releases/tag/v0.65.0".to_string(),
            auto_install: true,
            auto_install_override: None,
            just_updated: false,
        };
        let serialized = serde_json::to_string(&cache).unwrap();
        let deserialized: UpdateCache = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.current_version, "0.64.0");
        assert_eq!(deserialized.latest_version, "0.65.0");
        assert!(deserialized.auto_install);
    }

    #[test]
    fn test_update_config_parse() {
        let toml_str = r#"
            check = false
            auto_install = true
            interval_hours = 12
        "#;
        let cfg: crate::chunker::UpdateConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.check, Some(false));
        assert_eq!(cfg.auto_install, Some(true));
        assert_eq!(cfg.interval_hours, Some(12));
    }

    #[test]
    fn test_check_interval_calculation() {
        let interval = check_interval_secs();
        // default is 24 hours = 86400 secs
        assert!(interval >= 3600);
    }
}
