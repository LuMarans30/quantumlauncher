//! Downloads signed GitHub releases and installs them through a staging folder.
//! Build IDs include the archive checksum because `nightly` is republished.
//! Imported build IDs and archive paths are validated before use.

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ql_core::{
    DownloadProgress, HorizonProgress, IntoIoError, LAUNCHER_DIR, err, file_utils::exists, info,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::{Reporter, error::HorizonError};

pub const REPO: &str = "horizon-rust/horizon";
pub const RELEASE_TAG: &str = "nightly";

const RELEASE_ASSET_BASE: &str = "https://github.com/horizon-rust/horizon/releases/download";

/// Horizon's release-signing public keys (base64 Ed25519), pinned from
/// `crates/horizon-release-sign/release-keys.txt` upstream. During a key
/// rotation both keys sign for a while, so any verifying line is enough.
const RELEASE_KEYS: &[&str] = &["MU4oiIQcCxkZvbwSjjXs7SySCuM6ywRQ3nGaJYZG0gw="];

const REQUIRED_PROGRAMS: &[&str] = &["horizon-client", "horizon-prepare"];
const REQUIRED_FILES: &[&str] = &["research-lock.json"];

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const INSTALL_LOCK_WAIT: Duration = Duration::from_secs(15 * 60);
const INSTALL_LOCK_STALE: Duration = Duration::from_secs(15 * 60);
const INSTALL_LITTER_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// A build of Horizon that is published, with its expected checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HorizonBuild {
    pub tag: String,
    pub asset: String,
    pub asset_url: String,
    pub sha256: String,
}

impl HorizonBuild {
    #[must_use]
    pub fn id(&self) -> String {
        let short = self.sha256.get(..16).unwrap_or(&self.sha256);
        format!("{}-{short}", sanitize_tag(&self.tag))
    }
}

#[derive(Debug, Clone)]
pub struct InstalledBuild {
    pub dir: PathBuf,
    pub id: String,
    pub tag: String,
}

impl InstalledBuild {
    #[must_use]
    pub fn program(&self, name: &str) -> PathBuf {
        program_path(&self.dir, name)
    }

    #[must_use]
    pub fn client(&self) -> PathBuf {
        self.program("horizon-client")
    }

    #[must_use]
    pub fn prepare(&self) -> PathBuf {
        self.program("horizon-prepare")
    }
}

/// `QuantumLauncher/horizon/`
#[must_use]
pub fn horizon_root() -> PathBuf {
    LAUNCHER_DIR.join("horizon")
}

/// `QuantumLauncher/horizon/builds/`
#[must_use]
pub fn builds_root() -> PathBuf {
    horizon_root().join("builds")
}

/// Where a build's binaries are kept.
pub fn build_dir(id: &str) -> Result<PathBuf, HorizonError> {
    validate_build_id(id)?;
    Ok(builds_root().join(id))
}

/// Where a build's prepared resources are kept. They depend on the build
/// and its Minecraft version, not the instance, so one folder serves every
/// instance of a build.
pub fn prepared_dir(id: &str) -> Result<PathBuf, HorizonError> {
    validate_build_id(id)?;
    Ok(horizon_root().join("prepared").join(id))
}

/// A build id is a plain folder name (`<tag>-<first 16 hex of SHA-256>`).
///
/// It comes from an instance's `config.json`, so it must never escape the
/// builds folder (absolute paths and `..` are rejected).
pub fn validate_build_id(id: &str) -> Result<(), HorizonError> {
    // Windows reserves these as device names
    let upper = id.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit());

    let valid = !id.is_empty()
        && id.len() <= 128
        && !reserved
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(HorizonError::InvalidBuildId(id.to_owned()))
    }
}

/// Turns a release tag into a folder-name-safe prefix.
fn sanitize_tag(tag: &str) -> String {
    let mut sanitized: String = tag
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        sanitized.push_str(RELEASE_TAG);
    }
    sanitized
}

/// The archive Horizon publishes for this platform, if any.
pub fn platform_asset() -> Result<&'static str, HorizonError> {
    let asset = if cfg!(target_os = "windows") {
        if cfg!(target_arch = "x86_64") {
            "horizon-windows-x64.zip"
        } else if cfg!(target_arch = "aarch64") {
            "horizon-windows-arm64.zip"
        } else {
            return Err(unsupported());
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "horizon-macos-arm64.zip"
        } else if cfg!(target_arch = "x86_64") {
            "horizon-macos-x64.zip"
        } else {
            return Err(unsupported());
        }
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        "horizon-linux-x86_64.tar.gz"
    } else {
        return Err(unsupported());
    };
    Ok(asset)
}

fn unsupported() -> HorizonError {
    HorizonError::UnsupportedPlatform(format!(
        "{}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ))
}

pub(crate) fn program_path(dir: &Path, name: &str) -> PathBuf {
    if cfg!(target_os = "windows") {
        dir.join(format!("{name}.exe"))
    } else {
        dir.join(name)
    }
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

pub async fn fetch_latest_build() -> Result<HorizonBuild, HorizonError> {
    let asset = platform_asset()?;
    let url = format!("https://api.github.com/repos/{REPO}/releases/tags/{RELEASE_TAG}");
    let release: Release = get_json(&url).await?;

    let asset_url = release
        .assets
        .iter()
        .find(|n| n.name == asset)
        .map(|n| n.browser_download_url.clone())
        .ok_or_else(|| HorizonError::NoAssetForPlatform(asset.to_owned()))?;
    check_release_url(&asset_url, &release.tag_name)?;

    let sums_url = release
        .assets
        .iter()
        .find(|n| n.name == "SHA256SUMS")
        .map(|n| n.browser_download_url.clone())
        .ok_or_else(|| HorizonError::AssetNotFound("SHA256SUMS".to_owned()))?;
    check_release_url(&sums_url, &release.tag_name)?;

    let signature_url = release
        .assets
        .iter()
        .find(|n| n.name == "SHA256SUMS.sig")
        .map(|n| n.browser_download_url.clone())
        .ok_or_else(|| HorizonError::AssetNotFound("SHA256SUMS.sig".to_owned()))?;
    check_release_url(&signature_url, &release.tag_name)?;

    // Verify before parsing: nothing in the list is trusted until the
    // signature over the exact bytes and the tag verifies.
    let sums = fetch_signed_sums(&release.tag_name, &sums_url, &signature_url).await?;

    let sha256 = parse_sums(&sums)?
        .into_iter()
        .find(|(name, _)| name == asset)
        .map(|(_, hash)| hash)
        .ok_or_else(|| HorizonError::NoChecksum(asset.to_owned()))?;

    Ok(HorizonBuild {
        tag: release.tag_name,
        asset: asset.to_owned(),
        asset_url,
        sha256,
    })
}

/// Refuses a release file that doesn't come from Horizon's own releases
fn check_release_url(url: &str, tag: &str) -> Result<(), HorizonError> {
    let expected = format!("{RELEASE_ASSET_BASE}/{tag}/");
    if url.starts_with(&expected) {
        Ok(())
    } else {
        Err(HorizonError::UnexpectedAssetUrl(url.to_owned()))
    }
}

/// Makes sure the latest build is installed, downloading it if needed.
pub async fn ensure_build(reporter: Reporter<'_>) -> Result<InstalledBuild, HorizonError> {
    let build = fetch_latest_build().await?;
    install_build(&build, reporter).await
}

pub async fn install_build(
    build: &HorizonBuild,
    reporter: Reporter<'_>,
) -> Result<InstalledBuild, HorizonError> {
    let id = build.id();
    let dir = build_dir(&id)?;

    if is_installed(&dir).await {
        info!("Horizon build {id} is already installed");
        return Ok(InstalledBuild {
            dir,
            id,
            tag: build.tag.clone(),
        });
    }
    if repair_permissions(&dir).await? {
        info!("Repaired Horizon build {id}'s executable bits");
        return Ok(InstalledBuild {
            dir,
            id,
            tag: build.tag.clone(),
        });
    }

    let lock = InstallLock::acquire(&horizon_root().join(".install.lock")).await?;
    // Another process may have finished while we waited for the lock.
    if is_installed(&dir).await {
        return Ok(InstalledBuild {
            dir,
            id,
            tag: build.tag.clone(),
        });
    }

    sweep_install_litter();

    info!("Installing Horizon build {id} ({})", build.asset);
    reporter.send(DownloadProgress::Horizon(HorizonProgress::Build));

    // Unpack next to the final folder, then rename it into place.
    let staging = builds_root().join(format!("{id}.staging.{}", lock.token()));
    if let Err(err) = install_into(&staging, build).await {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(err);
    }

    if let Some(parent) = dir.parent() {
        tokio::fs::create_dir_all(parent).await.path(parent)?;
    }

    let old = builds_root().join(format!(".{id}.old.{}", lock.token()));
    let installed = swap_into_place(&staging, &dir, &old).await?;
    if installed {
        info!("Installed Horizon build {id}");
    } else {
        info!("Another Horizon install finished first; using it");
    }

    Ok(InstalledBuild {
        dir,
        id,
        tag: build.tag.clone(),
    })
}

/// Moves a fully unpacked `staging` folder into `dir`.
async fn swap_into_place(staging: &Path, dir: &Path, old: &Path) -> Result<bool, HorizonError> {
    if is_installed(dir).await {
        let _ = tokio::fs::remove_dir_all(staging).await;
        return Ok(false);
    }

    let _ = tokio::fs::remove_dir_all(old).await;
    let had_old = exists(dir).await;
    if had_old {
        tokio::fs::rename(dir, old).await.path(dir)?;
    }

    if let Err(error) = tokio::fs::rename(staging, dir).await.path(dir) {
        if had_old {
            if let Err(restore) = tokio::fs::rename(old, dir).await {
                err!(
                    "Could not restore the previous Horizon build to {}: {restore}; it is still at {}",
                    dir.display(),
                    old.display()
                );
            }
        }
        let _ = tokio::fs::remove_dir_all(staging).await;
        return Err(error.into());
    }

    if had_old {
        let _ = tokio::fs::remove_dir_all(old).await;
    }
    Ok(true)
}

/// Downloads, checks and unpacks a build into `staging`.
async fn install_into(staging: &Path, build: &HorizonBuild) -> Result<(), HorizonError> {
    let _ = tokio::fs::remove_dir_all(staging).await;
    tokio::fs::create_dir_all(staging).await.path(staging)?;

    let archive = staging.join(".download");
    ql_core::download(&build.asset_url).path(&archive).await?;

    let actual = sha256_of_file(&archive).await?;
    if actual != build.sha256 {
        return Err(HorizonError::ChecksumMismatch {
            expected: build.sha256.clone(),
            actual,
        });
    }

    let unpacked = staging.join("unpacked");
    tokio::fs::create_dir_all(&unpacked).await.path(&unpacked)?;
    extract(&build.asset, &archive, &unpacked).await?;

    let root = find_program_root(&unpacked).await?;
    promote(staging, &root).await?;
    let _ = tokio::fs::remove_dir_all(&unpacked).await;
    let _ = tokio::fs::remove_file(&archive).await;

    for program in REQUIRED_PROGRAMS {
        if !is_regular_file(&program_path(staging, program)).await {
            return Err(HorizonError::InstalledIncomplete(staging.to_owned()));
        }
    }
    for file in REQUIRED_FILES {
        if !is_regular_file(&staging.join(file)).await {
            return Err(HorizonError::InstalledIncomplete(staging.to_owned()));
        }
    }

    make_executable(staging).await?;

    let marker = InstalledBuildMarker {
        tag: build.tag.clone(),
        asset: build.asset.clone(),
        sha256: build.sha256.clone(),
    };
    let marker_path = staging.join("build.json");
    let marker_part = unique_part(&marker_path);
    tokio::fs::write(
        &marker_part,
        serde_json::to_vec_pretty(&marker).map_err(|err| HorizonError::Message(err.to_string()))?,
    )
    .await
    .path(&marker_part)?;
    tokio::fs::rename(&marker_part, &marker_path)
        .await
        .path(&marker_path)?;

    Ok(())
}

/// Is this folder a complete, installed build?
pub(crate) async fn is_installed(dir: &Path) -> bool {
    if read_marker(dir).await.is_none() {
        return false;
    }
    for program in REQUIRED_PROGRAMS {
        let path = program_path(dir, program);
        if !is_regular_file(&path).await || !is_executable(&path).await {
            return false;
        }
    }
    for file in REQUIRED_FILES {
        if !is_regular_file(&dir.join(file)).await {
            return false;
        }
    }
    true
}

/// Repairs lost executable bits on an otherwise complete build.
/// Returns whether the build is usable afterwards.
pub(crate) async fn repair_permissions(dir: &Path) -> Result<bool, HorizonError> {
    let complete = read_marker_sync(dir).is_some()
        && REQUIRED_PROGRAMS
            .iter()
            .all(|program| is_regular_file_sync(&program_path(dir, program)))
        && REQUIRED_FILES
            .iter()
            .all(|file| is_regular_file_sync(&dir.join(file)));
    if !complete {
        return Ok(false);
    }
    make_executable(dir).await?;
    Ok(is_installed(dir).await)
}

/// Finds an already-installed build by its id.
pub async fn installed_build(id: &str) -> Result<InstalledBuild, HorizonError> {
    let dir = build_dir(id)?;
    if !is_installed(&dir).await {
        return Err(HorizonError::BuildMissing(dir));
    }
    let tag = read_marker(&dir)
        .await
        .map(|n| n.tag)
        .unwrap_or_else(|| RELEASE_TAG.to_owned());
    Ok(InstalledBuild {
        dir,
        id: id.to_owned(),
        tag,
    })
}

/// Whether a program can actually be run, so a `chmod -x` build gets
/// repaired instead of failing later with a confusing `CommandError`.
async fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::metadata(path)
            .await
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MinecraftPin {
    pub version: String,
    pub manifest_sha1: String,
    #[serde(default)]
    pub accepted_manifest_sha1: Vec<String>,
    #[serde(default)]
    pub client_sha1: Option<String>,
    #[serde(default)]
    pub server_sha1: Option<String>,
}

impl MinecraftPin {
    #[must_use]
    pub fn accepts(&self, sha1: &str) -> bool {
        sha1.eq_ignore_ascii_case(&self.manifest_sha1)
            || self
                .accepted_manifest_sha1
                .iter()
                .any(|n| n.eq_ignore_ascii_case(sha1))
    }

    #[must_use]
    pub fn manifest_sha1s(&self) -> Vec<&str> {
        let mut all = vec![self.manifest_sha1.as_str()];
        all.extend(self.accepted_manifest_sha1.iter().map(String::as_str));
        all
    }

    #[must_use]
    pub fn expected(&self) -> String {
        self.manifest_sha1s().join(", ")
    }
}

#[derive(Debug, Deserialize)]
struct ResearchLock {
    minecraft: MinecraftPin,
}

/// Reads the Minecraft pins beside a build's binaries.
pub async fn research_lock(dir: &Path) -> Result<MinecraftPin, HorizonError> {
    let path = dir.join("research-lock.json");
    let text = tokio::fs::read_to_string(&path)
        .await
        .map_err(|_| HorizonError::ResearchLockMissing(path.clone()))?;
    match serde_json::from_str::<ResearchLock>(&text) {
        Ok(lock) => Ok(lock.minecraft),
        Err(error) => Err(HorizonError::ResearchLockInvalid {
            path,
            error: error.to_string(),
        }),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct InstalledBuildMarker {
    tag: String,
    asset: String,
    sha256: String,
}

async fn read_marker(dir: &Path) -> Option<InstalledBuildMarker> {
    let path = dir.join("build.json");
    let text = tokio::fs::read_to_string(&path).await.ok()?;
    serde_json::from_str(&text).ok()
}

fn read_marker_sync(dir: &Path) -> Option<InstalledBuildMarker> {
    std::fs::read_to_string(dir.join("build.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

/// Whether a folder really is a complete build, with the same rules
/// (including the Unix exec bit) as [`is_installed`].
fn is_installed_sync(dir: &Path) -> bool {
    read_marker_sync(dir).is_some()
        && REQUIRED_PROGRAMS.iter().all(|program| {
            let path = program_path(dir, program);
            is_regular_file_sync(&path) && is_executable_sync(&path)
        })
        && REQUIRED_FILES
            .iter()
            .all(|file| is_regular_file_sync(&dir.join(file)))
}

fn is_executable_sync(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

/// The id in `.{id}.old.{token}`.
fn old_build_id(name: &str) -> Option<&str> {
    name.strip_prefix('.')?
        .split_once(".old.")
        .map(|(id, _)| id)
}

/// Fetches the checksum list and its signature as a pair, retrying both
/// once: a Nightly republish replaces both assets, so crossing that window
/// can briefly yield a 404 or new sums with an old signature.
async fn fetch_signed_sums(
    tag: &str,
    sums_url: &str,
    signature_url: &str,
) -> Result<String, HorizonError> {
    let mut last_error = None;
    for attempt in 0..2 {
        let result = async {
            let sums = request_text(sums_url).await?;
            let signatures = request_text(signature_url).await?;
            verify_release_signature(tag, sums.as_bytes(), &signatures)?;
            Ok::<_, HorizonError>(sums)
        }
        .await;

        match result {
            Ok(sums) => return Ok(sums),
            Err(error) => {
                if attempt == 0 {
                    err!("{error}; retrying the signed checksum pair once in 1.5s");
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                }
                last_error = Some(error);
            }
        }
    }
    Err(last_error.expect("the signed checksum pair was tried twice"))
}

/// Verifies `SHA256SUMS.sig` over `horizon release <tag>\n` + the exact
/// `SHA256SUMS` bytes. The file holds one base64 Ed25519 signature per
/// line (one per signing key during a rotation); any line that verifies
/// with a pinned key is enough.
fn verify_release_signature(tag: &str, sums: &[u8], signatures: &str) -> Result<(), HorizonError> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ring::signature::{ED25519, UnparsedPublicKey};

    let mut message = format!("horizon release {tag}\n").into_bytes();
    message.extend_from_slice(sums);

    for line in signatures.lines().map(str::trim).filter(|n| !n.is_empty()) {
        let Ok(signature) = STANDARD.decode(line) else {
            continue;
        };
        for key in RELEASE_KEYS {
            let Ok(key) = STANDARD.decode(key) else {
                continue;
            };
            if UnparsedPublicKey::new(&ED25519, &key)
                .verify(&message, &signature)
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    Err(HorizonError::ReleaseSignatureInvalid {
        tag: tag.to_owned(),
    })
}

/// Parses a `SHA256SUMS` file into `(file name, SHA-256 hex)` pairs.
/// `<hex> *<name>` (GNU coreutils' binary mode) is accepted too.
pub fn parse_sums(text: &str) -> Result<Vec<(String, String)>, HorizonError> {
    let mut sums = Vec::new();
    for line in text.lines().filter(|n| !n.trim().is_empty()) {
        let (hash, name) = line
            .split_once(' ')
            .ok_or_else(|| HorizonError::Message(format!("bad SHA256SUMS line: {line:?}")))?;
        let name = name.strip_prefix([' ', '*']).unwrap_or(name);
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) || name.is_empty() {
            return Err(HorizonError::Message(format!(
                "bad SHA256SUMS line: {line:?}"
            )));
        }
        sums.push((name.to_owned(), hash.to_ascii_lowercase()));
    }
    Ok(sums)
}

async fn get_json<T>(url: &str) -> Result<T, HorizonError>
where
    T: serde::de::DeserializeOwned,
{
    let text = request_text(url).await?;
    serde_json::from_str(&text)
        .map_err(|err| HorizonError::Message(format!("while reading {url}: {err}")))
}

/// A GitHub request with a timeout, retried like every other download.
async fn request_text(url: &str) -> Result<String, HorizonError> {
    ql_core::retry(|| async {
        let response = tokio::time::timeout(
            REQUEST_TIMEOUT,
            ql_core::request::CLIENT_UNCACHED
                .get(url)
                .header(reqwest::header::USER_AGENT, "QuantumLauncher")
                .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                .send(),
        )
        .await
        .map_err(|_| HorizonError::RequestTimedOut {
            what: url.to_owned(),
            seconds: REQUEST_TIMEOUT.as_secs(),
        })??;

        let response = response.error_for_status()?;
        tokio::time::timeout(REQUEST_TIMEOUT, response.text())
            .await
            .map_err(|_| HorizonError::RequestTimedOut {
                what: url.to_owned(),
                seconds: REQUEST_TIMEOUT.as_secs(),
            })?
            .map_err(Into::into)
    })
    .await
}

/// The SHA-256 of a file, as lowercase hex.
pub async fn sha256_of_file(path: &Path) -> Result<String, HorizonError> {
    let mut file = tokio::fs::File::open(path).await.path(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await.path(path)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_lower(hasher.finalize()))
}

/// The SHA-1 of a file, as lowercase hex.
pub(crate) async fn sha1_of_file(path: &Path) -> Result<String, HorizonError> {
    use sha1::{Digest, Sha1};

    let mut file = tokio::fs::File::open(path).await.path(path)?;
    let mut hasher = Sha1::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await.path(path)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_lower(hasher.finalize()))
}

fn hex_lower(digest: impl IntoIterator<Item = u8>) -> String {
    use std::fmt::Write;

    let mut hex = String::new();
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

async fn extract(asset: &str, archive: &Path, output: &Path) -> Result<(), HorizonError> {
    let asset = asset.to_owned();
    let archive = archive.to_owned();
    let output = output.to_owned();
    tokio::task::spawn_blocking(move || extract_blocking(&asset, &archive, &output))
        .await
        .map_err(|err| HorizonError::Message(format!("extraction task failed: {err}")))?
}

fn extract_blocking(asset: &str, archive: &Path, output: &Path) -> Result<(), HorizonError> {
    if asset.ends_with(".tar.gz") {
        extract_tar(archive, output)
    } else if asset.ends_with(".zip") {
        extract_zip(archive, output)
    } else {
        Err(HorizonError::Message(format!(
            "unknown Horizon archive type: {asset}"
        )))
    }
}

/// Unpacks a `.tar.gz`, refusing anything a release has no business
/// shipping: absolute paths, `..`, symlinks, hardlinks and device files.
fn extract_tar(archive: &Path, output: &Path) -> Result<(), HorizonError> {
    use tar::EntryType;

    let file = std::fs::File::open(archive).map_err(|error| HorizonError::CommandError {
        program: archive.to_owned(),
        error,
    })?;
    let decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let mut tar = tar::Archive::new(decoder);

    let entries = tar.entries().map_err(|error| HorizonError::Archive {
        path: archive.to_owned(),
        error: error.to_string(),
    })?;
    let mut seen = HashSet::new();

    for entry in entries {
        let mut entry = entry.map_err(|error| HorizonError::Archive {
            path: archive.to_owned(),
            error: error.to_string(),
        })?;
        let kind = entry.header().entry_type();
        let name = entry
            .path()
            .map(|n| n.display().to_string())
            .unwrap_or_else(|_| "<unreadable>".to_owned());

        // tar's metadata entries (pax headers, GNU long names) carry no
        // file of their own; the tar reader consumes them.
        if matches!(
            kind,
            EntryType::GNULongName
                | EntryType::GNULongLink
                | EntryType::XHeader
                | EntryType::XGlobalHeader
        ) {
            continue;
        }

        if !matches!(kind, EntryType::Regular | EntryType::Directory) {
            return Err(HorizonError::UnsafeArchive {
                entry: name,
                reason: "symlinks, hardlinks and special files are not allowed",
            });
        }

        let path = entry
            .path()
            .map_err(|error| HorizonError::Archive {
                path: archive.to_owned(),
                error: error.to_string(),
            })?
            .into_owned();
        check_archive_path(&path)?;
        if kind == EntryType::Regular {
            check_duplicate_entry(&path, &mut seen)?;
        }

        let unpacked = entry
            .unpack_in(output)
            .map_err(|error| HorizonError::Archive {
                path: archive.to_owned(),
                error: error.to_string(),
            })?;
        if !unpacked {
            return Err(HorizonError::UnsafeArchive {
                entry: path.display().to_string(),
                reason: "path escapes the folder it is unpacked into",
            });
        }
    }

    Ok(())
}

/// Unpacks a `.zip`, refusing absolute paths, `..`, backslashes and
/// symlink entries.
fn extract_zip(archive: &Path, output: &Path) -> Result<(), HorizonError> {
    let file = std::fs::File::open(archive).map_err(|error| HorizonError::CommandError {
        program: archive.to_owned(),
        error,
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| HorizonError::Archive {
        path: archive.to_owned(),
        error: error.to_string(),
    })?;
    let mut seen = HashSet::new();

    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|error| HorizonError::Archive {
            path: archive.to_owned(),
            error: error.to_string(),
        })?;

        let name = entry
            .name()
            .map_err(|error| HorizonError::Archive {
                path: archive.to_owned(),
                error: error.to_string(),
            })?
            .into_owned();
        if name.contains('\\') {
            return Err(HorizonError::UnsafeArchive {
                entry: name,
                reason: "backslashes in paths are not allowed",
            });
        }

        let relative = Path::new(&name);
        check_archive_path(relative)?;
        if !entry.is_dir() {
            check_duplicate_entry(relative, &mut seen)?;
        }

        // A symlink stored in a zip carries the Unix symlink file type.
        if let Some(mode) = entry.unix_mode() {
            let file_type = mode & 0o170_000;
            if file_type == 0o120_000 {
                return Err(HorizonError::UnsafeArchive {
                    entry: name,
                    reason: "symlinks are not allowed",
                });
            }
            if file_type != 0 && file_type != 0o100_000 && file_type != 0o040_000 {
                return Err(HorizonError::UnsafeArchive {
                    entry: name,
                    reason: "special files are not allowed",
                });
            }
        }

        let target = output.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|error| HorizonError::Io(error.path(&target)))?;
            continue;
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| HorizonError::Io(error.path(parent)))?;
        }
        let mut out = std::fs::File::create(&target)
            .map_err(|error| HorizonError::Io(error.path(&target)))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|error| HorizonError::Io(error.path(&target)))?;
    }

    Ok(())
}

/// Refuses a path that could leave the folder it is unpacked into or
/// use a Windows alternate data stream/drive letter.
fn check_archive_path(path: &Path) -> Result<(), HorizonError> {
    check_relative_path(path)?;

    let text = path.to_string_lossy();
    if text.contains(':') {
        return Err(HorizonError::UnsafeArchive {
            entry: text.into_owned(),
            reason: "colons (drive letters, alternate data streams) are not allowed",
        });
    }

    for component in path.components() {
        if let Component::Normal(part) = component {
            if is_reserved_device_name(&part.to_string_lossy()) {
                return Err(HorizonError::UnsafeArchive {
                    entry: text.into_owned(),
                    reason: "Windows device names are not allowed",
                });
            }
        }
    }

    Ok(())
}

/// `CON`, `NUL`, `COM1`, `aux.txt`, … — Windows writes to a device
/// instead of a file, even inside a folder.
fn is_reserved_device_name(name: &str) -> bool {
    let base = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches([' ', '.']);
    let upper = base.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || (upper.len() == 4
        && (upper.starts_with("COM") || upper.starts_with("LPT"))
        && upper.as_bytes()[3].is_ascii_digit())
}

/// Refuses a second **file** that lands on the same path as an earlier one
/// (last-one-wins would silently change the install). Directory entries may
/// repeat: some writers emit both `exporters/` and `exporters`.
fn check_duplicate_entry(path: &Path, seen: &mut HashSet<String>) -> Result<(), HorizonError> {
    if !seen.insert(archive_key(path)) {
        return Err(HorizonError::UnsafeArchive {
            entry: path.display().to_string(),
            reason: "duplicate files are not allowed",
        });
    }
    Ok(())
}

/// A comparison key built from path components (`./a`, `a//b` and `a` are
/// the same file), case-folded on case-insensitive filesystems and with
/// Windows' ignored trailing dots/spaces removed.
fn archive_key(path: &Path) -> String {
    let mut key = String::new();
    for component in path.components() {
        if let Component::Normal(part) = component {
            if !key.is_empty() {
                key.push('/');
            }
            let mut part = part.to_string_lossy().into_owned();
            if cfg!(target_os = "windows") {
                part = part.trim_end_matches([' ', '.']).to_owned();
            }
            key.push_str(&part);
        }
    }
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        key.make_ascii_lowercase();
    }
    key
}

/// Refuses a path that could leave the folder it is unpacked into.
fn check_relative_path(path: &Path) -> Result<(), HorizonError> {
    let mut has_components = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => has_components = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(HorizonError::UnsafeArchive {
                    entry: path.display().to_string(),
                    reason: "absolute paths and `..` are not allowed",
                });
            }
        }
    }
    if has_components {
        Ok(())
    } else {
        Err(HorizonError::UnsafeArchive {
            entry: path.display().to_string(),
            reason: "empty path",
        })
    }
}

/// Finds the folder that has `horizon-client` in it (one level deep, since
/// archives wrap everything in a single folder).
///
/// `symlink_metadata` is used on purpose: a symlinked program must not be
/// discovered (and run) as one.
async fn find_program_root(dir: &Path) -> Result<PathBuf, HorizonError> {
    if is_regular_file(&program_path(dir, "horizon-client")).await {
        return Ok(dir.to_owned());
    }
    let mut entries = tokio::fs::read_dir(dir).await.path(dir)?;
    while let Some(entry) = entries.next_entry().await.path(dir)? {
        if !entry.file_type().await.path(dir)?.is_dir() {
            continue;
        }
        let path = entry.path();
        if is_regular_file(&program_path(&path, "horizon-client")).await {
            return Ok(path);
        }
    }
    err!(
        "Horizon's download has no horizon-client binary in {}",
        dir.display()
    );
    Err(HorizonError::ClientMissingFromDownload)
}

async fn is_regular_file(path: &Path) -> bool {
    tokio::fs::symlink_metadata(path)
        .await
        .is_ok_and(|meta| meta.is_file())
}

fn is_regular_file_sync(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file())
}

/// Moves everything from `root` into `dir` (its parent).
async fn promote(dir: &Path, root: &Path) -> Result<(), HorizonError> {
    if root == dir {
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(root).await.path(root)?;
    while let Some(entry) = entries.next_entry().await.path(root)? {
        let file_type = entry.file_type().await.path(root)?;
        if file_type.is_symlink() {
            return Err(HorizonError::UnsafeArchive {
                entry: entry.path().display().to_string(),
                reason: "symlinks are not allowed",
            });
        }
        let from = entry.path();
        let to = dir.join(entry.file_name());
        tokio::fs::rename(&from, &to).await.path(&to)?;
    }
    Ok(())
}

async fn make_executable(dir: &Path) -> Result<(), HorizonError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for name in [
            "horizon-client",
            "horizon-prepare",
            "horizon-server",
            "horizon-ctl",
        ] {
            let path = program_path(dir, name);
            if exists(&path).await {
                if let Err(err) = ql_core::file_utils::set_executable(&path).await {
                    err!("Could not make {name} executable: {err}");
                }
            }
        }

        // An install whose programs aren't executable would pass
        // `exists()` and only fail later with a permission error, so check
        // before writing the marker.
        for name in REQUIRED_PROGRAMS {
            let path = program_path(dir, name);
            let mode = tokio::fs::metadata(&path)
                .await
                .path(&path)?
                .permissions()
                .mode();
            if mode & 0o111 == 0 {
                return Err(HorizonError::InstalledIncomplete(dir.to_owned()));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = dir;

    Ok(())
}

/// One installer at a time, so two creations can't unpack into the same
/// staging folder.
///
/// The lock is a directory (atomic to create) holding an ownership token
/// and a heartbeat. A lock whose owner is gone is **stolen by renaming it**
/// (atomic, so only one waiter wins), and `Drop` only removes a lock it
/// still owns.
struct InstallLock {
    path: PathBuf,
    token: String,
    heartbeat: tokio::task::JoinHandle<()>,
}

impl InstallLock {
    fn token(&self) -> &str {
        &self.token
    }

    async fn acquire(path: &Path) -> Result<Self, HorizonError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.path(parent)?;
        }

        let token = unique_lock_token();
        // Also records the PID's start time, so a reused PID can't keep a
        // dead lock alive. Older locks without that file fall back to PID.
        let started = process_start_time(std::process::id()).await;
        let mut waited = Duration::ZERO;
        let mut attempt = 0_u32;
        loop {
            match tokio::fs::create_dir(path).await {
                Ok(()) => {
                    if let Err(error) = write_lock_owner(path, &token, started) {
                        let _ = tokio::fs::remove_dir_all(path).await;
                        return Err(error);
                    }
                    if !waited.is_zero() {
                        info!(
                            "Waited {:.1}s for another Horizon install to finish",
                            waited.as_secs_f32()
                        );
                    }
                    let heartbeat = start_heartbeat(path.to_owned());
                    return Ok(Self {
                        path: path.to_owned(),
                        token,
                        heartbeat,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Checking liveness starts a process list refresh; no
                    // need to do that five times a second.
                    if attempt % 25 == 0 && steal_if_stale(path, &token).await? {
                        continue;
                    }
                    attempt = attempt.wrapping_add(1);

                    if waited >= INSTALL_LOCK_WAIT {
                        return Err(HorizonError::InstallBusy);
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    waited += Duration::from_millis(200);
                }
                Err(error) => return Err(error.path(path).into()),
            }
        }
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        self.heartbeat.abort();

        // Rename first, then check the owner: a steal + re-acquire between
        // a read and a remove must not delete someone else's lock. Renaming
        // is atomic, so only we can be looking at this folder name.
        let name = lock_folder_name(&self.path);
        let dropped = self
            .path
            .with_file_name(format!(".{name}.drop.{}", self.token));
        if std::fs::rename(&self.path, &dropped).is_err() {
            return;
        }

        let owner = std::fs::read_to_string(dropped.join("owner")).unwrap_or_default();
        if owner.trim() == self.token {
            let _ = std::fs::remove_dir_all(&dropped);
        } else {
            // Not ours after all: put it back.
            let _ = std::fs::rename(&dropped, &self.path);
        }
    }
}

fn start_heartbeat(path: PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(30));
        loop {
            ticker.tick().await;
            if std::fs::write(path.join("heartbeat"), b"").is_err() {
                return;
            }
        }
    })
}

fn write_lock_owner(path: &Path, token: &str, started: Option<u64>) -> Result<(), HorizonError> {
    let owner = path.join("owner");
    std::fs::write(&owner, token).map_err(|error| HorizonError::Io(error.path(&owner)))?;
    if let Some(started) = started {
        let file = path.join("started");
        std::fs::write(&file, started.to_string())
            .map_err(|error| HorizonError::Io(error.path(&file)))?;
    }
    let heartbeat = path.join("heartbeat");
    std::fs::write(&heartbeat, b"").map_err(|error| HorizonError::Io(error.path(&heartbeat)))?;
    Ok(())
}

/// Takes over a lock whose owner is gone.
///
/// The rename is atomic, so only one waiter wins, and a live owner is never
/// stolen (which also rules out clock-jump races).
async fn steal_if_stale(lock: &Path, token: &str) -> Result<bool, HorizonError> {
    if !lock_is_stale(lock).await {
        return Ok(false);
    }

    let name = lock_folder_name(lock);
    let stale = lock.with_file_name(format!(".{name}.stale.{token}"));

    if tokio::fs::rename(lock, &stale).await.is_err() {
        return Ok(false);
    }

    // Between the staleness check and the rename, another process may have
    // stolen this lock, acquired it and started installing. Check the folder
    // we actually moved before deleting it; put a live one back.
    if !lock_is_stale(&stale).await {
        let _ = tokio::fs::rename(&stale, lock).await;
        return Ok(false);
    }

    let _ = tokio::fs::remove_dir_all(&stale).await;
    Ok(true)
}

/// Whether a lock's owner is gone.
///
/// A recorded process is the signal: a dead process means a dead lock
/// immediately, and a live one is trusted even if its heartbeat looks old,
/// so a clock jump can't make a running install look dead. Locks without a
/// readable owner use the heartbeat instead, where a timestamp in the
/// future counts as stale.
async fn lock_is_stale(lock: &Path) -> bool {
    let owner = std::fs::read_to_string(lock.join("owner")).unwrap_or_default();
    if let Some(pid) = owner_pid(owner.trim()) {
        let started = std::fs::read_to_string(lock.join("started"))
            .ok()
            .and_then(|n| n.trim().parse().ok());
        return owner_is_alive(pid, started).await == Some(false);
    }

    match lock_age(lock).await {
        Some(age) => age > INSTALL_LOCK_STALE,
        None => true,
    }
}

fn owner_pid(owner: &str) -> Option<u32> {
    owner.split('-').next()?.parse().ok()
}

async fn owner_is_alive(pid: u32, started: Option<u64>) -> Option<bool> {
    tokio::task::spawn_blocking(move || {
        let pid = sysinfo::Pid::from_u32(pid);
        let mut system = sysinfo::System::new().ok()?;
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        Some(system.process(pid).is_some_and(|process| {
            process.status() != sysinfo::ProcessStatus::Zombie
                && started.is_none_or(|started| process.start_time() == started)
        }))
    })
    .await
    .ok()
    .flatten()
}

async fn process_start_time(pid: u32) -> Option<u64> {
    tokio::task::spawn_blocking(move || {
        let pid = sysinfo::Pid::from_u32(pid);
        let mut system = sysinfo::System::new().ok()?;
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        system.process(pid).map(sysinfo::Process::start_time)
    })
    .await
    .ok()
    .flatten()
}

/// How long ago the lock's heartbeat (or the folder itself) was touched.
/// `None` when the timestamp is in the future or unreadable.
async fn lock_age(lock: &Path) -> Option<Duration> {
    let heartbeat = lock.join("heartbeat");
    let modified = match std::fs::metadata(&heartbeat) {
        Ok(meta) => meta.modified().ok(),
        Err(_) => std::fs::metadata(lock)
            .ok()
            .and_then(|meta| meta.modified().ok()),
    };
    modified?.elapsed().ok()
}

fn lock_folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "install".to_owned())
}

fn unique_lock_token() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|n| n.as_nanos())
        .unwrap_or_default();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{nanos}-{counter}", std::process::id())
}

/// Removes old staging/stolen/dropped folders and `.part` leftovers so a
/// killed install doesn't keep gigabytes around forever.
fn sweep_install_litter() {
    // Build folders, plus the lock's own leftovers, which live one directory
    // up (`.install.lock.stale.*`, `.install.lock.drop.*`).
    let builds = builds_root();
    sweep_litter_directory(&builds, |name, path| {
        if let Some(id) = old_build_id(name) {
            // A failed swap+restore can strand the only complete build under
            // `.old`; keep it while the canonical folder is gone, but a
            // redundant copy beside a healthy build is fair game.
            return match read_marker_sync(path) {
                Some(marker)
                    if marker
                        .sha256
                        .get(..16)
                        .is_some_and(|short| id.ends_with(short)) =>
                {
                    is_installed_sync(&builds.join(id))
                }
                _ => true,
            };
        }
        name.contains(".staging.") || name.contains(".stale.") || name.contains(".drop.")
    });
    sweep_litter_directory(&horizon_root(), |name, _| {
        name.starts_with(".install.lock.")
    });
}

fn sweep_litter_directory(root: &Path, is_litter: impl Fn(&str, &Path) -> bool) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if !is_litter(&name, &path) {
            continue;
        }
        let old = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.elapsed().ok())
            .is_some_and(|age| age > INSTALL_LITTER_AGE);
        if old {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// A `.part` path that two concurrent processes can't share.
pub(crate) fn unique_part(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_owned());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|n| n.as_nanos())
        .unwrap_or_default();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(
        ".{name}.{}.{nanos}.{counter}.part",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ql-horizon-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_horizon_sums_file() {
        let text = "\
a7507d819769d01a365ab707794a4084392c824f54a7a6a7862f8c3d0892b283  horizon-linux-x86_64.tar.gz
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef *horizon-windows-x64.zip
";
        let sums = parse_sums(text).unwrap();
        assert_eq!(sums.len(), 2);
        assert_eq!(sums[0].0, "horizon-linux-x86_64.tar.gz");
        assert_eq!(
            sums[1].0, "horizon-windows-x64.zip",
            "binary-mode `*` prefix is stripped"
        );
    }

    #[test]
    fn rejects_malformed_sums() {
        assert!(parse_sums("not a checksum line").is_err());
        assert!(parse_sums("abc  file.zip").is_err());
    }

    // A real Nightly's signed checksum list, kept as a fixture so the
    // verification path is exercised without network access.
    const REAL_NIGHTLY_SUMS: &str = "\
da05362420ce861af1cb641e013678e12cee6a97144224573950be6ef756b4e3  horizon-linux-x86_64.tar.gz
b4a21d8004689844281b88509beb101467c492ec563e070a0de3e8b95311998e  horizon-windows-x64.zip
8207fad8d8151f4437571b3b58cd7375a3e846d45e14e5ddc981c58a147c0f96  horizon-windows-arm64.zip
1e394b5d5e11e6ca2a8351798e47bc1deafd8c8396e5aa049e6e8a47ea5c04e5  horizon-macos-arm64.zip
e04185f69c4960c8d8904faac09d8570bbf9a8067cba738c61f6f7347cd0511f  horizon-macos-x64.zip
d4d3aba3e26fdb85a7b56c643f3cd2c3a4a198b0bd757e614f1efdf0654c75dc  horizon-server-linux-x86_64.tar.gz
7986b66867069a57b938a3479fb3264fdb202e790aa5368420371212222d5bd3  horizon-source.tar.gz
";
    const REAL_NIGHTLY_SIG: &str = "CIM1HjA2Lv2GE+q0sDXauwFOBlNxDk8hU1hu5eVd2Dy1eM9NpTDbh/vmHoz0JztfjW9t/GICyFxyIe1T/IDLDw==\n";

    #[test]
    fn verifies_a_real_nightly_signature() {
        verify_release_signature("nightly", REAL_NIGHTLY_SUMS.as_bytes(), REAL_NIGHTLY_SIG)
            .expect("the pinned key must verify the real Nightly signature");
    }

    #[test]
    fn rejects_tampered_sums() {
        let tampered = REAL_NIGHTLY_SUMS.replace("da05362420ce861a", "0000000000000000");
        let err = verify_release_signature("nightly", tampered.as_bytes(), REAL_NIGHTLY_SIG)
            .expect_err("a changed checksum must not verify");
        assert!(
            matches!(err, HorizonError::ReleaseSignatureInvalid { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn rejects_sums_replayed_under_another_tag() {
        let err = verify_release_signature(
            "nightly-2026-11-12",
            REAL_NIGHTLY_SUMS.as_bytes(),
            REAL_NIGHTLY_SIG,
        )
        .expect_err("the signed text names the tag");
        assert!(
            matches!(err, HorizonError::ReleaseSignatureInvalid { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn rejects_malformed_signature_files() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let sums = REAL_NIGHTLY_SUMS.as_bytes();
        let zero_sig = STANDARD.encode([0_u8; 64]);
        let short_sig = STANDARD.encode([0_u8; 32]);
        for signatures in [
            "",
            "   \n\t\n",
            "not base64!",
            zero_sig.as_str(),
            short_sig.as_str(),
        ] {
            let err = verify_release_signature("nightly", sums, signatures)
                .expect_err("a malformed signature must fail closed");
            assert!(
                matches!(err, HorizonError::ReleaseSignatureInvalid { .. }),
                "{signatures:?}: {err:?}"
            );
        }
    }

    #[test]
    fn build_ids_change_with_the_checksum() {
        let build = |sha: &str| HorizonBuild {
            tag: RELEASE_TAG.to_owned(),
            asset: platform_asset()
                .unwrap_or("horizon-linux-x86_64.tar.gz")
                .to_owned(),
            asset_url: String::new(),
            sha256: sha.to_owned(),
        };
        let one = build("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").id();
        let two = build("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").id();
        assert_ne!(one, two);
        assert!(one.starts_with("nightly-"));
    }

    #[test]
    fn tags_cannot_escape_the_builds_folder() {
        let build = HorizonBuild {
            tag: "../../etc".to_owned(),
            asset: "x.tar.gz".to_owned(),
            asset_url: String::new(),
            sha256: "a".repeat(64),
        };
        let id = build.id();
        validate_build_id(&id).expect("a sanitized tag makes a valid id");
        assert!(!id.contains('/'));
    }

    #[test]
    fn rejects_unsafe_build_ids() {
        for id in [
            "",
            "..",
            "../evil",
            "/tmp/evil",
            "a/b",
            "a\\b",
            ".hidden",
            "nightly:evil",
            "CON",
            "nul",
            "COM1",
            "LPT9",
        ] {
            assert!(validate_build_id(id).is_err(), "{id:?} must be rejected");
        }
        assert!(validate_build_id("nightly-da05362420ce861a").is_ok());
        assert!(validate_build_id("nightly_da05362420ce861a").is_ok());
    }

    #[test]
    fn build_paths_cannot_escape_the_launcher_folder() {
        // An imported instance's `config.json` can name any build id;
        // it must never become a path outside the builds folder.
        for id in ["/tmp/evil", "../../instances/x/.minecraft", "a/../../b"] {
            assert!(
                matches!(build_dir(id), Err(HorizonError::InvalidBuildId(_))),
                "{id:?} must not become a build path"
            );
            assert!(
                matches!(prepared_dir(id), Err(HorizonError::InvalidBuildId(_))),
                "{id:?} must not become a prepared path"
            );
        }
    }

    #[test]
    fn refuses_tar_path_traversal() {
        let dir = temp_dir("tar-traversal");
        let archive = dir.join("bad.tar.gz");
        write_raw_tar(
            &archive,
            "../evil.txt",
            tar::EntryType::Regular,
            None,
            b"boom",
        );
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let err = extract_tar(&archive, &out).expect_err("must be refused");
        assert!(matches!(err, HorizonError::UnsafeArchive { .. }), "{err:?}");
        assert!(!dir.join("evil.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_tar_absolute_paths() {
        let dir = temp_dir("tar-absolute");
        let archive = dir.join("bad.tar.gz");
        write_raw_tar(
            &archive,
            "/tmp/evil.txt",
            tar::EntryType::Regular,
            None,
            b"boom",
        );
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let err = extract_tar(&archive, &out).expect_err("must be refused");
        assert!(matches!(err, HorizonError::UnsafeArchive { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_tar_symlinks() {
        let dir = temp_dir("tar-symlink");
        let archive = dir.join("bad.tar.gz");
        write_raw_tar(
            &archive,
            "horizon-client",
            tar::EntryType::Symlink,
            Some("/bin/sh"),
            b"",
        );

        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let err = extract_tar(&archive, &out).expect_err("must be refused");
        assert!(matches!(err, HorizonError::UnsafeArchive { .. }), "{err:?}");
        assert!(!out.join("horizon-client").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_tar_hardlinks() {
        let dir = temp_dir("tar-hardlink");
        let archive = dir.join("bad.tar.gz");
        write_raw_tar(
            &archive,
            "horizon-client",
            tar::EntryType::Link,
            Some("../../etc/passwd"),
            b"",
        );
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let err = extract_tar(&archive, &out).expect_err("must be refused");
        assert!(matches!(err, HorizonError::UnsafeArchive { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_check_normalizes_dot_and_slash_forms() {
        let dir = temp_dir("tar-duplicate-normalized");
        let archive = dir.join("bad.tar.gz");
        write_raw_tar_entries(
            &archive,
            &[
                ("horizon-client", tar::EntryType::Regular, None, b"one"),
                ("./horizon-client", tar::EntryType::Regular, None, b"two"),
            ],
        );
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let err = extract_tar(&archive, &out).expect_err("must be refused");
        assert!(matches!(err, HorizonError::UnsafeArchive { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_directory_entries_are_allowed() {
        let dir = temp_dir("zip-repeated-dirs");
        let archive = dir.join("good.zip");
        write_zip(&archive, &["exporters/", "./exporters/", "exporters/a.jar"]);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        extract_zip(&archive, &out).expect("repeated directory entries are harmless");
        assert!(out.join("exporters/a.jar").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_duplicate_tar_entries() {
        let dir = temp_dir("tar-duplicate");
        let archive = dir.join("bad.tar.gz");
        write_raw_tar_entries(
            &archive,
            &[
                ("horizon-client", tar::EntryType::Regular, None, b"one"),
                ("horizon-client", tar::EntryType::Regular, None, b"two"),
            ],
        );
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let err = extract_tar(&archive, &out).expect_err("must be refused");
        assert!(matches!(err, HorizonError::UnsafeArchive { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_windows_device_names_in_archives() {
        for name in [
            "NUL", "aux.jar", "Com1", "con.txt", "LPT9", "CONIN$", "conout$", "NUL ", "COM1 ",
            "aux. ",
        ] {
            assert!(
                is_reserved_device_name(name),
                "{name} is a Windows device name"
            );
        }
        for name in ["console", "auxiliary.jar", "com10", "null"] {
            assert!(!is_reserved_device_name(name), "{name} is an ordinary name");
        }

        let dir = temp_dir("zip-device-name");
        let archive = dir.join("bad.zip");
        write_zip(&archive, &["NUL"]);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let err = extract_zip(&archive, &out).expect_err("must be refused");
        assert!(matches!(err, HorizonError::UnsafeArchive { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_colons_in_zip_names() {
        let dir = temp_dir("zip-colon");
        let archive = dir.join("bad.zip");
        write_zip(&archive, &["build.json:hidden"]);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let err = extract_zip(&archive, &out).expect_err("must be refused");
        assert!(matches!(err, HorizonError::UnsafeArchive { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn refuses_case_folded_duplicates_on_insensitive_filesystems() {
        let dir = temp_dir("zip-case");
        let archive = dir.join("bad.zip");
        write_zip(&archive, &["Horizon-Client", "horizon-client"]);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let err = extract_zip(&archive, &out).expect_err("must be refused");
        assert!(matches!(err, HorizonError::UnsafeArchive { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_programs(dir: &Path) {
        for program in REQUIRED_PROGRAMS {
            let path = program_path(dir, program);
            std::fs::write(&path, b"binary").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }

    fn write_installed_layout(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        write_programs(dir);
        std::fs::write(dir.join("research-lock.json"), b"{}").unwrap();
        std::fs::write(
            dir.join("build.json"),
            br#"{"tag":"nightly","asset":"x","sha256":"a"}"#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn incomplete_install_is_not_installed() {
        let dir = temp_dir("incomplete-install");
        std::fs::create_dir_all(&dir).unwrap();
        write_programs(&dir);
        std::fs::write(dir.join("research-lock.json"), b"{}").unwrap();

        assert!(
            !is_installed(&dir).await,
            "without the marker it must not count as installed"
        );

        std::fs::write(
            dir.join("build.json"),
            br#"{"tag":"nightly","asset":"x","sha256":"a"}"#,
        )
        .unwrap();
        assert!(
            is_installed(&dir).await,
            "marker + programs + lock is enough"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn directories_do_not_count_as_installed_files() {
        let dir = temp_dir("directory-in-install");
        for path in [
            program_path(&dir, "horizon-client"),
            dir.join("research-lock.json"),
        ] {
            write_installed_layout(&dir);
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            assert!(!is_installed(&dir).await);
            assert!(!is_installed_sync(&dir));
            assert!(!repair_permissions(&dir).await.unwrap());
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_programs_are_not_installed_or_repaired() {
        let dir = temp_dir("symlink-in-install");
        write_installed_layout(&dir);
        let client = program_path(&dir, "horizon-client");
        let target = dir.join("target");
        std::fs::rename(&client, &target).unwrap();
        std::os::unix::fs::symlink(&target, &client).unwrap();
        assert!(!is_installed(&dir).await);
        assert!(!is_installed_sync(&dir));
        assert!(!repair_permissions(&dir).await.unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_executable_program_is_not_installed_and_can_be_repaired() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("non-executable");
        write_installed_layout(&dir);
        let client = program_path(&dir, "horizon-client");
        std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_installed(&dir).await, "chmod -x needs a repair");
        assert!(
            !is_installed_sync(&dir),
            "the sweeper must agree with the launcher about health"
        );

        assert!(
            repair_permissions(&dir).await.unwrap(),
            "the existing tree is repaired in place"
        );
        assert!(is_installed(&dir).await);
        assert!(is_installed_sync(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn swap_leaves_a_complete_build_untouched() {
        let root = temp_dir("swap-complete");
        let dir = root.join("build");
        write_installed_layout(&dir);
        std::fs::write(dir.join("keep"), b"complete").unwrap();

        let staging = root.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("new"), b"new").unwrap();
        let old = root.join("old");

        assert!(
            !swap_into_place(&staging, &dir, &old).await.unwrap(),
            "a complete build must be reused, not replaced"
        );
        assert!(!staging.exists(), "the unused staging folder is removed");
        assert!(!old.exists(), "nothing was moved aside");
        assert_eq!(std::fs::read(dir.join("keep")).unwrap(), b"complete");
        assert_eq!(
            std::fs::read(program_path(&dir, "horizon-client")).unwrap(),
            b"binary"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn swap_replaces_a_partial_build_and_removes_the_old_folder() {
        let root = temp_dir("swap-partial");
        let dir = root.join("build");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("partial"), b"old").unwrap();

        let staging = root.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("new"), b"new").unwrap();
        let old = root.join("old");

        assert!(swap_into_place(&staging, &dir, &old).await.unwrap());
        assert!(!staging.exists());
        assert!(
            !old.exists(),
            "the partial old folder is gone after the swap"
        );
        assert!(!dir.join("partial").exists());
        assert_eq!(std::fs::read(dir.join("new")).unwrap(), b"new");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_failed_swap_puts_the_partial_build_back() {
        let root = temp_dir("swap-restore");
        let dir = root.join("build");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("partial"), b"old").unwrap();

        // A staging folder that doesn't exist makes the swap fail after
        // the old folder has been moved aside.
        let staging = root.join("missing-staging");
        let old = root.join("old");

        let err = swap_into_place(&staging, &dir, &old)
            .await
            .expect_err("moving a missing staging folder must fail");
        assert!(matches!(err, HorizonError::Io(_)), "{err:?}");
        assert!(dir.join("partial").is_file(), "the old build is restored");
        assert!(!old.exists(), "nothing is left under the old name");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stranded_build_is_recognised_by_its_marker() {
        let dir = temp_dir("stranded-marker");
        write_installed_layout(&dir);
        assert!(read_marker_sync(&dir).is_some());
        assert!(is_installed_sync(&dir));
        std::fs::remove_file(dir.join("build.json")).unwrap();
        assert!(read_marker_sync(&dir).is_none());
        assert!(!is_installed_sync(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_folders_yield_the_build_id() {
        assert_eq!(
            old_build_id(".nightly-2026-11-12-da05362420ce861a.old.123-456"),
            Some("nightly-2026-11-12-da05362420ce861a")
        );
        assert_eq!(old_build_id("nightly-da05362420ce861a"), None);
    }

    #[tokio::test]
    async fn a_live_lock_is_not_stolen_even_with_an_old_heartbeat() {
        let dir = temp_dir("lock-live");
        let path = dir.join("install.lock");
        let lock = InstallLock::acquire(&path).await.unwrap();

        // Pretend the heartbeat is ancient; the owner's process is still
        // alive, so liveness must win over the timestamp.
        std::fs::OpenOptions::new()
            .write(true)
            .open(path.join("heartbeat"))
            .unwrap()
            .set_modified(SystemTime::now() - INSTALL_LOCK_STALE - Duration::from_secs(60))
            .unwrap();

        let waiter_path = path.clone();
        let waiter = tokio::spawn(async move { InstallLock::acquire(&waiter_path).await });
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            !waiter.is_finished(),
            "a live owner must not lose its lock to a stale-looking heartbeat"
        );

        drop(lock);
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the waiter gets the lock after release")
            .unwrap()
            .expect("acquire succeeds");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_reused_pid_with_a_different_start_time_is_not_alive() {
        let pid = std::process::id();
        let started = process_start_time(pid).await.expect("own start time");
        assert_eq!(owner_is_alive(pid, Some(started)).await, Some(true));
        assert_eq!(
            owner_is_alive(pid, Some(started + 1)).await,
            Some(false),
            "a different start time means the PID was reused"
        );
    }

    #[tokio::test]
    async fn concurrent_waiters_serialize_on_one_lock() {
        let dir = temp_dir("lock-race");
        let path = dir.join("install.lock");

        // An owner-less, old lock: every waiter will try to steal it.
        std::fs::create_dir_all(&path).unwrap();
        let heartbeat = path.join("heartbeat");
        std::fs::write(&heartbeat, b"").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&heartbeat)
            .unwrap()
            .set_modified(SystemTime::now() - INSTALL_LOCK_STALE - Duration::from_secs(60))
            .unwrap();

        let mut waiters = Vec::new();
        for _ in 0..4 {
            let path = path.clone();
            waiters.push(tokio::spawn(async move {
                let lock = InstallLock::acquire(&path).await.expect("acquire succeeds");
                tokio::time::sleep(Duration::from_millis(50)).await;
                drop(lock);
            }));
        }

        for waiter in waiters {
            tokio::time::timeout(Duration::from_secs(10), waiter)
                .await
                .expect("every waiter gets its turn")
                .unwrap();
        }

        assert!(!path.exists(), "the lock is gone when everyone is done");
        let litter: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".stale.") || name.contains(".drop."))
            .collect();
        assert!(
            litter.is_empty(),
            "no stolen/dropped folders leak: {litter:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn install_lock_is_exclusive_and_reusable() {
        let dir = temp_dir("lock-exclusive");
        let path = dir.join("install.lock");

        let first = InstallLock::acquire(&path).await.unwrap();
        let waiter_path = path.clone();
        let waiter = tokio::spawn(async move { InstallLock::acquire(&waiter_path).await });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!waiter.is_finished(), "a second install must wait");

        drop(first);
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the waiter gets the lock")
            .unwrap()
            .expect("second acquire succeeds");
        assert!(!path.exists(), "the lock is removed when released");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stale_install_lock_is_stolen_once() {
        let dir = temp_dir("lock-stale");
        let path = dir.join("install.lock");
        std::fs::create_dir_all(&path).unwrap();
        let heartbeat = path.join("heartbeat");
        std::fs::write(&heartbeat, b"").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&heartbeat)
            .unwrap()
            .set_modified(SystemTime::now() - INSTALL_LOCK_STALE - Duration::from_secs(60))
            .unwrap();

        let lock = InstallLock::acquire(&path).await.unwrap();
        assert!(path.join("owner").is_file(), "the new owner is recorded");
        drop(lock);
        assert!(!path.exists(), "the stolen lock is replaced and cleaned up");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_zip_path_traversal() {
        let dir = temp_dir("zip-traversal");
        let archive = dir.join("bad.zip");
        write_zip(&archive, &["../evil.txt"]);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let err = extract_zip(&archive, &out).expect_err("must be refused");
        assert!(matches!(err, HorizonError::UnsafeArchive { .. }));
        assert!(!dir.join("evil.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_absolute_zip_paths() {
        let dir = temp_dir("zip-absolute");
        let archive = dir.join("bad.zip");
        write_zip(&archive, &["/tmp/ql-horizon-evil.txt"]);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let err = extract_zip(&archive, &out).expect_err("must be refused");
        assert!(matches!(err, HorizonError::UnsafeArchive { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepts_regular_tar_and_zip_files() {
        let dir = temp_dir("good-archives");

        let tar = dir.join("good.tar.gz");
        write_tar_gz(&tar, &[("horizon-linux-x86_64/horizon-client", b"bin")]);
        let tar_out = dir.join("tar-out");
        std::fs::create_dir_all(&tar_out).unwrap();
        extract_tar(&tar, &tar_out).unwrap();
        assert!(
            tar_out
                .join("horizon-linux-x86_64/horizon-client")
                .is_file()
        );

        let zip_path = dir.join("good.zip");
        write_zip(&zip_path, &["horizon-windows-x64/horizon-client.exe"]);
        let zip_out = dir.join("zip-out");
        std::fs::create_dir_all(&zip_out).unwrap();
        extract_zip(&zip_path, &zip_out).unwrap();
        assert!(
            zip_out
                .join("horizon-windows-x64/horizon-client.exe")
                .is_file()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn research_lock_fails_closed() {
        let dir = temp_dir("research-lock");
        let err = research_lock(&dir).await.unwrap_err();
        assert!(matches!(err, HorizonError::ResearchLockMissing(_)));

        std::fs::write(dir.join("research-lock.json"), "{ not json").unwrap();
        let err = research_lock(&dir).await.unwrap_err();
        assert!(matches!(err, HorizonError::ResearchLockInvalid { .. }));

        std::fs::write(
            dir.join("research-lock.json"),
            r#"{"minecraft": {"version": "26.3",
                "manifest_sha1": "11f1aefe473eefba0af62e3286c33b692807b2eb",
                "accepted_manifest_sha1": ["4fe1"],
                "client_sha1": "e877b6a07acd633fb3bb475002175cec036e7b87"}}"#,
        )
        .unwrap();
        let pin = research_lock(&dir).await.unwrap();
        assert_eq!(pin.version, "26.3");
        assert!(pin.accepts("11f1aefe473eefba0af62e3286c33b692807b2eb"));
        assert!(pin.accepts("4fe1"));
        assert!(!pin.accepts("9f67e10ed013deeaad20ac3c800652765d493408"));
        assert_eq!(
            pin.client_sha1.as_deref(),
            Some("e877b6a07acd633fb3bb475002175cec036e7b87")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Writes one tar entry with a raw name, bypassing the tar crate's own
    /// path validation (that's what a hostile archive looks like).
    fn write_raw_tar(
        path: &Path,
        name: &str,
        kind: tar::EntryType,
        link: Option<&str>,
        data: &[u8],
    ) {
        write_raw_tar_entries(path, &[(name, kind, link, data)]);
    }

    fn write_raw_tar_entries(path: &Path, entries: &[(&str, tar::EntryType, Option<&str>, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);

        for (name, kind, link, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            {
                let gnu = header.as_gnu_mut().expect("GNU header");
                let bytes = name.as_bytes();
                assert!(bytes.len() < gnu.name.len());
                gnu.name[..bytes.len()].copy_from_slice(bytes);
                if let Some(link) = link {
                    let bytes = link.as_bytes();
                    assert!(bytes.len() < gnu.linkname.len());
                    gnu.linkname[..bytes.len()].copy_from_slice(bytes);
                }
            }
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        builder.finish().unwrap();
    }

    fn write_tar_gz(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, *name, *data).unwrap();
        }
        builder.finish().unwrap();
    }

    fn write_zip(path: &Path, names: &[&str]) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for name in names {
            let options: zip::write::SimpleFileOptions = Default::default();
            zip.start_file(*name, options).unwrap();
            use std::io::Write;
            zip.write_all(b"data").unwrap();
        }
        zip.finish().unwrap();
    }
}
