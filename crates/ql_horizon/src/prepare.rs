//! Building Horizon's resources from the player's Minecraft files.
//!
//! `horizon-prepare` reads a version JSON, client jar, libraries, assets
//! and (the first time) the server jar, then writes the folder
//! `horizon-client --resources` reads. It reports progress as JSON lines
//! (`--progress json`) and can check a folder cheaply (`--check`), which
//! every launch does first.
//!
//! Upstream reference:
//! <https://github.com/horizon-rust/horizon/blob/main/docs/api/launch.md#preparing>

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use ql_core::{DownloadProgress, HorizonProgress, IntoIoError, JavaVersion, info, no_window, pt};
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
};

use crate::{
    Reporter,
    api::{self, PrepareApi},
    build::{self, InstalledBuild, prepared_dir},
    error::HorizonError,
    mc::{MinecraftFiles, ensure_minecraft_files},
};

/// How long `--check` may take (it only reads a folder).
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the preparer may go without printing a line before it is
/// treated as hung. The first preparation legitimately takes minutes.
const PREPARE_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Until it has printed anything, the preparer may be waiting for another
/// preparation (upstream allows one at a time).
const PREPARE_START_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// A ready-to-use resources folder.
#[derive(Debug, Clone)]
pub struct PreparedResources {
    pub dir: PathBuf,
    /// The Minecraft version they were made from.
    pub minecraft_version: String,
}

/// One line of `horizon-prepare --progress json` output.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PrepareLine {
    Message { message: String },
    Fraction { fraction: f64 },
    Error { error: String },
}

/// Makes sure this build's resources are prepared, preparing if needed.
///
/// The check runs before every launch; the preparation itself only happens
/// once per build.
pub async fn ensure_prepared(
    build: &InstalledBuild,
    reporter: Reporter<'_>,
) -> Result<PreparedResources, HorizonError> {
    let prepared = prepared_dir(&build.id)?;

    // The build's own pins decide which Minecraft version is valid; a
    // build without them can't be prepared safely.
    let pin = build::research_lock(&build.dir).await?;

    // Cheap and read-only: no `--launch-api` needed on the fast path.
    if check(&build.prepare(), &prepared).await?.is_ready() {
        return Ok(PreparedResources {
            dir: prepared,
            minecraft_version: pin.version,
        });
    }

    let api: PrepareApi = api::query(&build.prepare(), "horizon-prepare").await?;
    info!(
        "Preparing Horizon (Launch API {}) from Minecraft {} with Java {}",
        api.launch_api, api.minecraft, api.java_major
    );

    if pin.version != api.minecraft {
        ql_core::err!(
            "Horizon's pins say Minecraft {}, but its preparer says {}; using the pins",
            pin.version,
            api.minecraft
        );
    }
    let version = pin.version.clone();

    info!("Downloading Minecraft {version} for this Horizon build (this happens once)");
    let files = ensure_minecraft_files(&version, &pin, reporter).await?;
    let java = java_home(api.java_major, reporter).await?;

    prepare(
        &build.prepare(),
        &prepared,
        &files,
        java.as_deref(),
        reporter,
    )
    .await?;

    let result = check(&build.prepare(), &prepared).await?;
    if !result.is_ready() {
        return Err(HorizonError::PrepareIncomplete(result.gaps().to_owned()));
    }

    Ok(PreparedResources {
        dir: prepared,
        minecraft_version: version,
    })
}

/// What `horizon-prepare --check` said.
pub enum CheckResult {
    Ready,
    Gaps(String),
}

impl CheckResult {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    #[must_use]
    pub fn gaps(&self) -> &str {
        match self {
            Self::Ready => "",
            Self::Gaps(gaps) => gaps,
        }
    }
}

/// Runs `horizon-prepare --check` on a resources folder.
pub async fn check(prepare: &Path, prepared: &Path) -> Result<CheckResult, HorizonError> {
    let mut command = Command::new(prepare);
    command
        .arg("--check")
        .arg("--output")
        .arg(prepared)
        // A missing folder isn't an error: it just isn't ready.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    no_window!(command);

    let output = tokio::time::timeout(CHECK_TIMEOUT, command.output())
        .await
        .map_err(|_| HorizonError::CommandTimedOut {
            program: prepare.to_owned(),
            seconds: CHECK_TIMEOUT.as_secs(),
        })?
        .map_err(|error| HorizonError::CommandError {
            program: prepare.to_owned(),
            error,
        })?;

    match output.status.code() {
        Some(0) => Ok(CheckResult::Ready),
        Some(1) => Ok(CheckResult::Gaps(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        )),
        _ => Err(HorizonError::CommandFailed {
            program: prepare.to_owned(),
            status: output.status.to_string(),
            stderr: format!(
                "horizon-prepare --check failed:\n{}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }),
    }
}

async fn prepare(
    prepare: &Path,
    prepared: &Path,
    files: &MinecraftFiles,
    java_home: Option<&Path>,
    reporter: Reporter<'_>,
) -> Result<(), HorizonError> {
    tokio::fs::create_dir_all(prepared).await.path(prepared)?;

    info!("Preparing Horizon's resources (this takes a while the first time)");
    reporter.send(DownloadProgress::Horizon(HorizonProgress::Prepare {
        percent: 0,
    }));

    let mut command = Command::new(prepare);
    command
        .arg("--prepare")
        .arg("--server-jar")
        .arg(&files.server_jar)
        .arg("--version-json")
        .arg(&files.version_json)
        .arg("--client-jar")
        .arg(&files.client_jar)
        .arg("--libraries-dir")
        .arg(&files.libraries_dir)
        .arg("--assets-dir")
        .arg(&files.assets_dir)
        .arg("--output")
        .arg(prepared)
        .arg("--complete")
        .arg("--progress")
        .arg("json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(java_home) = java_home {
        command.arg("--java-home").arg(java_home);
    }
    no_window!(command);

    let mut child = command
        .spawn()
        .map_err(|error| HorizonError::CommandError {
            program: prepare.to_owned(),
            error,
        })?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    // Read stderr on the side, so a chatty preparer can't fill the pipe
    // and deadlock the progress stream.
    let stderr_task = tokio::spawn(async move {
        let mut text = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut text).await;
        text
    });

    let mut lines = BufReader::new(stdout).lines();
    let mut failure = None;
    let mut started = false;
    loop {
        let timeout = if started {
            PREPARE_IDLE_TIMEOUT
        } else {
            PREPARE_START_TIMEOUT
        };
        let line = match tokio::time::timeout(timeout, lines.next_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => return Err(error.path(prepared).into()),
            Err(_) => {
                let _ = child.kill().await;
                let _ = stderr_task.await;
                return Err(HorizonError::CommandTimedOut {
                    program: prepare.to_owned(),
                    seconds: timeout.as_secs(),
                });
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<PrepareLine>(line) {
            Ok(PrepareLine::Fraction { fraction }) => {
                // A zero fraction can come out before the preparer waits for
                // upstream's one-at-a-time lock; only real progress may start
                // the idle clock.
                if fraction > 0.0 {
                    started = true;
                }
                let percent = (fraction.clamp(0.0, 1.0) * 100.0).round() as usize;
                reporter.send(DownloadProgress::Horizon(HorizonProgress::Prepare {
                    percent,
                }));
            }
            Ok(PrepareLine::Message { message }) => pt!("{message}"),
            Ok(PrepareLine::Error { error }) => failure = Some(error),
            Err(_) => pt!("{line}"),
        }
    }

    let status = tokio::time::timeout(CHECK_TIMEOUT, child.wait())
        .await
        .map_err(|_| HorizonError::CommandTimedOut {
            program: prepare.to_owned(),
            seconds: CHECK_TIMEOUT.as_secs(),
        })?
        .path(prepared)?;
    let stderr = stderr_task.await.unwrap_or_default();
    let stderr = stderr.trim();

    if !status.success() {
        let mut output = String::new();
        if let Some(failure) = failure {
            output.push_str(&failure);
        }
        if !stderr.is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(stderr);
        }
        return Err(HorizonError::PrepareFailed {
            code: status.code().unwrap_or(-1),
            output: output.trim().to_owned(),
        });
    }

    info!("Horizon's resources are prepared");
    Ok(())
}

/// The Java home `horizon-prepare` should use, installed if needed.
async fn java_home(
    java_major: u32,
    reporter: Reporter<'_>,
) -> Result<Option<PathBuf>, HorizonError> {
    let Some(version) = java_version(java_major) else {
        return Err(HorizonError::UnsupportedJava(java_major));
    };

    let java =
        ql_java_handler::get_java_binary(version, ql_java_handler::JAVA, reporter.java_sender())
            .await?;

    // `<home>/bin/java` (and `<home>/Contents/Home/bin/java` on macOS),
    // which is what `--java-home` wants.
    Ok(java.parent().and_then(Path::parent).map(Path::to_owned))
}

/// Maps Horizon's `java_major` to a Java version this launcher manages.
#[must_use]
pub fn java_version(java_major: u32) -> Option<JavaVersion> {
    match java_major {
        8 => Some(JavaVersion::Java8),
        16 => Some(JavaVersion::Java16),
        17 => Some(JavaVersion::Java17),
        21 => Some(JavaVersion::Java21),
        25 => Some(JavaVersion::Java25),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn understands_progress_lines() {
        let fraction: PrepareLine = serde_json::from_str(r#"{"fraction": 0.25}"#).unwrap();
        assert!(matches!(fraction, PrepareLine::Fraction { .. }));

        let message: PrepareLine =
            serde_json::from_str(r#"{"message": "packing sounds"}"#).unwrap();
        assert!(matches!(message, PrepareLine::Message { .. }));

        let error: PrepareLine = serde_json::from_str(r#"{"error": "no server jar"}"#).unwrap();
        assert!(matches!(error, PrepareLine::Error { .. }));
    }

    #[test]
    fn maps_horizons_java_versions() {
        assert_eq!(java_version(25), Some(JavaVersion::Java25));
        assert_eq!(java_version(21), Some(JavaVersion::Java21));
        assert_eq!(java_version(11), None);
    }
}
