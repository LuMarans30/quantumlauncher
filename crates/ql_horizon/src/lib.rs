//! Support for [Horizon](https://playhorizon.dev), a Minecraft
//! reimplementation in Rust, through its
//! [Launch API](https://github.com/horizon-rust/horizon/blob/main/docs/api/launch.md).
//! No Horizon code is linked in; only its published binaries are run.
//!
//! - [`build`] downloads and verifies a build from its GitHub releases
//! - [`create_instance`] creates an instance with only the build
//! - [`update`] installs a newer build and points an instance at it
//! - [`prepare`] downloads the Minecraft files and builds the game's
//!   resources on first launch
//! - [`launch`] starts the game, signed in or (with [`ALLOW_OFFLINE`])
//!   offline
//!
//! Horizon's Launch API and its Nightly channel are experimental upstream,
//! so this integration is too. See `docs/HORIZON.md` for user-facing details.

#![allow(clippy::missing_errors_doc)]

use std::sync::mpsc::Sender;

use ql_core::{DownloadProgress, GenericProgress, Progress, err};

pub mod api;
pub mod build;
mod error;
mod instance;
mod launch;
mod mc;
pub mod prepare;
pub mod update;

pub use error::HorizonError;
pub use instance::create_instance;
pub use launch::{Session, launch};
pub use ql_core::json::HorizonConfig;
pub use update::{BuildUpdate, check_update, update_instance};

/// The pseudo "version" the create-instance menu lists for Horizon.
///
/// [`ql_core::ListEntryKind::Horizon`] keeps it apart from real versions.
pub const GAME_NAME: &str = "Horizon (nightly)";

/// Whether Horizon may run with an **offline** account.
///
/// Off by default: Horizon's Launch API asks launchers to start it only
/// for a Microsoft account that owns Minecraft: Java Edition. Turning it
/// on starts the game with `--name <username>` instead of a session, and
/// online-mode servers will refuse the player.
pub const ALLOW_OFFLINE: bool = false;

/// Whether a version name picked in the create-instance menu means
/// "create a Horizon instance" instead of downloading Minecraft.
#[must_use]
pub fn is_horizon_version(name: &str) -> bool {
    name == GAME_NAME
}

/// The version entry that the create-instance menu shows for Horizon.
#[must_use]
pub fn version_entry() -> ql_core::ListEntry {
    ql_core::ListEntry {
        name: GAME_NAME.to_owned(),
        // Only client instances are created for now, though Horizon also
        // has a server binary.
        supports_server: false,
        kind: ql_core::ListEntryKind::Horizon,
    }
}

/// Reads an instance's [`HorizonConfig`], if it's a Horizon instance.
pub async fn config_for(instance_name: &str) -> Result<Option<HorizonConfig>, HorizonError> {
    use ql_core::{Instance, InstanceConfigJson, IoError, JsonFileError};

    let instance = Instance::client(instance_name);
    match InstanceConfigJson::read(&instance).await {
        Ok(config) => Ok(config.horizon),
        Err(JsonFileError::Io(IoError::Io { error, .. }))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(err) => Err(err.into()),
    }
}

/// Where progress updates go: creating sends [`DownloadProgress`], a
/// launch sends [`GenericProgress`].
#[derive(Clone, Copy)]
pub enum Reporter<'a> {
    Download(&'a Sender<DownloadProgress>),
    Generic(&'a Sender<GenericProgress>),
    /// Nobody is watching (e.g. a background re-prepare).
    Nobody,
}

impl<'a> Reporter<'a> {
    pub(crate) fn send(self, progress: DownloadProgress) {
        match self {
            Reporter::Download(sender) => {
                if sender.send(progress).is_err() {
                    err!("Could not send Horizon progress update (receiver dropped)");
                }
            }
            Reporter::Generic(sender) => {
                let _ = sender.send(progress.into_generic());
            }
            Reporter::Nobody => {}
        }
    }

    /// The sender that Java installation progress goes to, if any.
    pub(crate) fn java_sender(self) -> Option<&'a Sender<GenericProgress>> {
        match self {
            Reporter::Download(_) | Reporter::Nobody => None,
            Reporter::Generic(sender) => Some(sender),
        }
    }
}
