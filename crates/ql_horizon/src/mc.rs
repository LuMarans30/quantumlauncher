//! Collecting the Minecraft files `horizon-prepare` reads.
//!
//! Horizon ships none of Mojang's code or assets: it builds its resources
//! from the player's copy of the version its build is pinned to. Those
//! files are downloaded into a shared folder per version:
//! `QuantumLauncher/horizon/minecraft/<version>/`.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use ql_core::{
    DownloadProgress, HorizonProgress, IntoIoError, IntoJsonError, LAUNCHER_DIR,
    file_utils::{self, exists},
    info,
    json::{AssetIndex, VersionDetails},
    pt,
};

use crate::{
    Reporter,
    build::{self, MinecraftPin},
    error::HorizonError,
};

/// The files `horizon-prepare` is pointed at.
#[derive(Debug, Clone)]
pub struct MinecraftFiles {
    /// `QuantumLauncher/horizon/minecraft/<version>/`
    pub dir: PathBuf,
    pub version_json: PathBuf,
    pub client_jar: PathBuf,
    pub libraries_dir: PathBuf,
    /// Holds `indexes/` and `objects/`.
    pub assets_dir: PathBuf,
    pub server_jar: PathBuf,
}

impl MinecraftFiles {
    #[must_use]
    pub fn new(version: &str) -> Self {
        let dir = LAUNCHER_DIR.join("horizon").join("minecraft").join(version);
        Self {
            version_json: dir
                .join("versions")
                .join(version)
                .join(format!("{version}.json")),
            client_jar: dir
                .join("versions")
                .join(version)
                .join(format!("{version}.jar")),
            libraries_dir: dir.join("libraries"),
            assets_dir: dir.join("assets"),
            server_jar: dir.join("server.jar"),
            dir,
        }
    }
}

/// Makes sure the Minecraft files for `version` are on disk.
///
/// Downloads only what is missing, so this is cheap once done.
pub async fn ensure_minecraft_files(
    version: &str,
    pin: &MinecraftPin,
    reporter: Reporter<'_>,
) -> Result<MinecraftFiles, HorizonError> {
    let files = MinecraftFiles::new(version);
    tokio::fs::create_dir_all(files.dir.join("versions"))
        .await
        .path(files.dir.join("versions"))?;

    // A killed download leaves a `.part` behind that is never reused.
    sweep_stale_parts(&files.dir).await;

    reporter.send(DownloadProgress::Horizon(HorizonProgress::VersionJson));
    let details = fetch_version_json(pin, &files.version_json).await?;
    info!("Prepared version JSON for Minecraft {version}");

    reporter.send(DownloadProgress::Horizon(HorizonProgress::Jar));
    let client_sha1 = pin
        .client_sha1
        .as_deref()
        .or_else(|| details.downloads.client.sha1());
    ensure_file(
        &files.client_jar,
        &details.downloads.client.url,
        client_sha1,
        "client jar",
    )
    .await?;

    download_libraries(&details, &files, reporter).await?;
    download_assets(&details, &files, reporter).await?;

    let server = details.downloads.server.as_ref().ok_or_else(|| {
        HorizonError::Message(format!(
            "no server download for Minecraft {version}: Horizon can't be prepared from it"
        ))
    })?;
    info!("Downloading the Minecraft {version} server jar (needed by Horizon's preparer)");
    reporter.send(DownloadProgress::Horizon(HorizonProgress::ServerJar));
    let server_sha1 = pin.server_sha1.as_deref().or_else(|| server.sha1());
    ensure_file(&files.server_jar, &server.url, server_sha1, "server jar").await?;

    Ok(files)
}

/// Downloads a jar unless the one on disk matches Horizon's pinned hash,
/// repairing a corrupted one instead of trusting its name.
///
/// Everything lands in a `.part` file first, so a killed download never
/// looks like a finished jar.
async fn ensure_file(
    path: &PathBuf,
    url: &str,
    expected_sha1: Option<&str>,
    label: &str,
) -> Result<(), HorizonError> {
    if exists(path).await {
        match expected_sha1 {
            None => return Ok(()),
            Some(expected) => {
                let actual = build::sha1_of_file(path).await?;
                if actual.eq_ignore_ascii_case(expected) {
                    return Ok(());
                }
                ql_core::err!(
                    "the {label} is damaged (SHA-1 {actual}, expected {expected}); downloading it again"
                );
            }
        }
    }

    let part = build::unique_part(path);
    ql_core::download(url).path(&part).await?;

    if let Some(expected) = expected_sha1 {
        let actual = build::sha1_of_file(&part).await?;
        if !actual.eq_ignore_ascii_case(expected) {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(HorizonError::FileHashMismatch {
                path: path.clone(),
                expected: expected.to_owned(),
                actual,
            });
        }
    }

    let _ = tokio::fs::remove_file(path).await;
    tokio::fs::rename(&part, path).await.path(path)?;
    Ok(())
}

/// Where Mojang serves a version file by its SHA-1 (the `packages`
/// folder is content-addressed, so old republished files stay there).
fn pinned_version_url(sha1: &str, version: &str) -> String {
    format!("https://piston-meta.mojang.com/v1/packages/{sha1}/{version}.json")
}

/// Downloads the pinned version JSON, trying every SHA-1 the build
/// accepts, and verifies what arrives before parsing it.
async fn fetch_version_json(
    pin: &MinecraftPin,
    path: &PathBuf,
) -> Result<VersionDetails, HorizonError> {
    // A pinned copy on disk is as good as a fresh one (checked against the
    // same SHA-1), so re-preparing works offline.
    if exists(path).await {
        let text = tokio::fs::read_to_string(path).await.path(path)?;
        if pin.accepts(&sha1_hex(text.as_bytes())) {
            return Ok(serde_json::from_str(&text).json(text)?);
        }
        ql_core::err!(
            "the version JSON on disk doesn't match Horizon's pins; downloading it again"
        );
    }

    let version = &pin.version;
    let mut last_error = None;

    for sha1 in pin.manifest_sha1s() {
        let url = pinned_version_url(sha1, version);
        match file_utils::download_file_to_string(&url, false).await {
            Ok(text) => {
                let actual = sha1_hex(text.as_bytes());
                if !pin.accepts(&actual) {
                    return Err(HorizonError::VersionJsonMismatch {
                        version: version.clone(),
                        expected: pin.expected(),
                        actual,
                    });
                }

                let details: VersionDetails = serde_json::from_str(&text).json(text.clone())?;
                let parent = path.parent().expect("version json has a folder");
                tokio::fs::create_dir_all(parent).await.path(parent)?;
                // Write through `.part`: two preparations of the same
                // version must never interleave their in-place writes.
                let part = build::unique_part(path);
                tokio::fs::write(&part, text).await.path(&part)?;
                let _ = tokio::fs::remove_file(path).await;
                tokio::fs::rename(&part, path).await.path(path)?;
                return Ok(details);
            }
            Err(error) => last_error = Some(HorizonError::from(error)),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        HorizonError::Message(format!(
            "no version file for Minecraft {version} (expected SHA-1: {})",
            pin.expected()
        ))
    }))
}

/// The SHA-1 of some bytes, as lowercase hex.
#[must_use]
pub fn sha1_hex(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    use std::fmt::Write;

    let digest = Sha1::digest(bytes);
    let mut hex = String::with_capacity(40);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

async fn download_libraries(
    details: &VersionDetails,
    files: &MinecraftFiles,
    reporter: Reporter<'_>,
) -> Result<(), HorizonError> {
    let libraries: Vec<_> = details
        .libraries
        .iter()
        .filter(|n| n.is_allowed())
        .filter_map(|n| n.get_artifact())
        .collect();

    let total = libraries.len();
    let done = Arc::new(AtomicUsize::new(0));
    reporter.send(DownloadProgress::Horizon(HorizonProgress::Libraries {
        progress: 0,
        out_of: total,
    }));

    let results = libraries.into_iter().map(|artifact| {
        let path = files.libraries_dir.join(artifact.get_path());
        let expected = artifact.sha1().map(str::to_owned);
        let done = done.clone();
        async move {
            ensure_file(&path, &artifact.url, expected.as_deref(), "library").await?;
            let progress = done.fetch_add(1, Ordering::Relaxed) + 1;
            reporter.send(DownloadProgress::Horizon(HorizonProgress::Libraries {
                progress,
                out_of: total,
            }));
            Ok::<(), HorizonError>(())
        }
    });

    ql_core::do_jobs_with_limit(results, 16).await?;
    pt!("Libraries done ({total})");
    Ok(())
}

async fn download_assets(
    details: &VersionDetails,
    files: &MinecraftFiles,
    reporter: Reporter<'_>,
) -> Result<(), HorizonError> {
    let indexes = files.assets_dir.join("indexes");
    let objects = files.assets_dir.join("objects");

    let index_path = indexes.join(format!("{}.json", details.assetIndex.id));
    ensure_asset_index(
        &index_path,
        &details.assetIndex.url,
        details.assetIndex.sha1(),
    )
    .await?;

    let text = tokio::fs::read_to_string(&index_path)
        .await
        .path(&index_path)?;
    let index: AssetIndex = serde_json::from_str(&text).json(text)?;

    tokio::fs::create_dir_all(&objects).await.path(&objects)?;

    let total = index.objects.len();
    let done = Arc::new(AtomicUsize::new(0));
    reporter.send(DownloadProgress::Horizon(HorizonProgress::Assets {
        progress: 0,
        out_of: total,
    }));

    let results = index.objects.values().map(|asset| {
        let done = done.clone();
        let objects = objects.clone();
        async move {
            ensure_asset_object(asset, &objects).await?;

            let progress = done.fetch_add(1, Ordering::Relaxed) + 1;
            // Thousands of assets: don't flood the progress channel.
            if progress % 50 == 0 || progress == total {
                reporter.send(DownloadProgress::Horizon(HorizonProgress::Assets {
                    progress,
                    out_of: total,
                }));
            }
            Ok::<(), HorizonError>(())
        }
    });

    ql_core::do_jobs_with_limit(results, 32).await?;
    pt!("Assets done ({total})");
    Ok(())
}

/// The asset index is content-addressed and pinned by the version JSON.
/// It is written through a `.part` file and parse-checked before it
/// replaces the old one, so a truncated copy is never reused.
async fn ensure_asset_index(
    path: &PathBuf,
    url: &str,
    expected_sha1: Option<&str>,
) -> Result<(), HorizonError> {
    if exists(path).await {
        match expected_sha1 {
            Some(expected) => {
                let actual = build::sha1_of_file(path).await?;
                if actual.eq_ignore_ascii_case(expected) {
                    return Ok(());
                }
                ql_core::err!("the asset index is damaged; downloading it again");
            }
            None => {
                // No pinned hash (old build): at least prove it parses.
                let text = tokio::fs::read_to_string(path).await.path(path)?;
                if serde_json::from_str::<AssetIndex>(&text).is_ok() {
                    return Ok(());
                }
                ql_core::err!("the asset index can't be read; downloading it again");
            }
        }
    }

    info!("Downloading asset index");
    let part = build::unique_part(path);
    ql_core::download(url).path(&part).await?;

    if let Some(expected) = expected_sha1 {
        let actual = build::sha1_of_file(&part).await?;
        if !actual.eq_ignore_ascii_case(expected) {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(HorizonError::FileHashMismatch {
                path: path.clone(),
                expected: expected.to_owned(),
                actual,
            });
        }
    }
    // Never install an index that doesn't parse.
    let text = tokio::fs::read_to_string(&part).await.path(&part)?;
    let _: AssetIndex = serde_json::from_str(&text).json(text)?;

    let _ = tokio::fs::remove_file(path).await;
    tokio::fs::rename(&part, path).await.path(path)?;
    Ok(())
}

/// Assets are named by their own SHA-1, so a corrupt or truncated object
/// is detectable and replaceable.
async fn ensure_asset_object(
    asset: &ql_core::json::asset_index::AssetObject,
    objects: &Path,
) -> Result<(), HorizonError> {
    let hash = asset.hash.trim();
    if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(HorizonError::Message(format!(
            "asset index names a malformed hash: {hash:?}"
        )));
    }
    let path = objects.join(&hash[0..2]).join(hash);

    if exists(&path).await {
        let actual = build::sha1_of_file(&path).await?;
        if actual.eq_ignore_ascii_case(hash) {
            return Ok(());
        }
        ql_core::err!("asset {hash} is damaged; downloading it again");
        let _ = tokio::fs::remove_file(&path).await;
    }

    // Fetch it ourselves (rather than `AssetObject::download`) so the
    // write is atomic.
    let url = asset.url().map(str::to_owned).unwrap_or_else(|| {
        format!(
            "https://resources.download.minecraft.net/{}/{}",
            &hash[0..2],
            hash
        )
    });
    let part = build::unique_part(&path);
    ql_core::download(&url).path(&part).await?;

    let actual = build::sha1_of_file(&part).await?;
    if !actual.eq_ignore_ascii_case(hash) {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(HorizonError::FileHashMismatch {
            path,
            expected: hash.to_owned(),
            actual,
        });
    }

    let _ = tokio::fs::remove_file(&path).await;
    tokio::fs::rename(&part, &path).await.path(&path)?;
    Ok(())
}

/// Removes `.part` files left by killed downloads (older than a day).
async fn sweep_stale_parts(dir: &Path) {
    let dir = dir.to_owned();
    let _ = tokio::task::spawn_blocking(move || {
        let now = SystemTime::now();
        for entry in walkdir::WalkDir::new(&dir).into_iter().flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !name.starts_with('.') || !name.ends_with(".part") {
                continue;
            }
            let old = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| now.duration_since(time).ok())
                .is_some_and(|age| age > Duration::from_secs(24 * 60 * 60));
            if old {
                let _ = std::fs::remove_file(path);
            }
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_like_sha1sum() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn pinned_urls_are_content_addressed() {
        assert_eq!(
            pinned_version_url("11f1aefe473eefba0af62e3286c33b692807b2eb", "26.3"),
            "https://piston-meta.mojang.com/v1/packages/11f1aefe473eefba0af62e3286c33b692807b2eb/26.3.json"
        );
    }

    #[test]
    fn files_live_in_a_version_specific_folder() {
        let files = MinecraftFiles::new("26.3");
        assert!(files.dir.ends_with("horizon/minecraft/26.3"));
        assert!(files.version_json.ends_with("versions/26.3/26.3.json"));
        assert!(files.assets_dir.ends_with("26.3/assets"));
        assert!(files.libraries_dir.ends_with("26.3/libraries"));
    }
}
