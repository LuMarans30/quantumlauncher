//! Moving a Horizon instance to a newer Nightly build.
//!
use ql_core::{DownloadProgress, HorizonProgress, Instance, InstanceConfigJson, info};

use crate::{
    HorizonConfig, Reporter,
    build::{self, HorizonBuild, InstalledBuild},
    error::HorizonError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildUpdate {
    pub current: String,
    pub latest: HorizonBuild,
}

impl BuildUpdate {
    /// The id the instance has after updating.
    #[must_use]
    pub fn id(&self) -> String {
        self.latest.id()
    }
}

#[must_use]
pub fn needs_update(current_id: &str, latest: &HorizonBuild) -> bool {
    current_id != latest.id()
}

pub async fn check_update(config: &HorizonConfig) -> Result<Option<BuildUpdate>, HorizonError> {
    check_channel(config)?;

    let latest = build::fetch_latest_build().await?;
    if !needs_update(&config.build_id, &latest) {
        return Ok(None);
    }

    Ok(Some(BuildUpdate {
        current: config.build_id.clone(),
        latest,
    }))
}

pub async fn update_instance(
    instance_name: &str,
    mut config: InstanceConfigJson,
    build: HorizonBuild,
    reporter: Reporter<'_>,
) -> Result<HorizonConfig, HorizonError> {
    let instance = Instance::client(instance_name);
    let Some(mut horizon) = config.horizon.clone() else {
        return Err(HorizonError::NotHorizonInstance(instance_name.to_owned()));
    };
    check_channel(&horizon)?;

    if !needs_update(&horizon.build_id, &build) {
        return Ok(horizon);
    }

    info!(
        "Updating Horizon instance {instance_name}: {} -> {}",
        horizon.build_id,
        build.id()
    );
    let installed = match build::install_build(&build, reporter).await {
        Ok(installed) => installed,
        // Nightly can be republished while the player is deciding
        Err(HorizonError::ChecksumMismatch { .. }) => {
            info!("Horizon's Nightly was republished; fetching the newer build");
            let latest = build::fetch_latest_build().await?;
            build::install_build(&latest, reporter).await?
        }
        Err(error) => return Err(error),
    };

    // The new build may be pinned to a different Minecraft version
    let minecraft_version = build::research_lock(&installed.dir).await?.version;
    retarget(&mut horizon, &installed, minecraft_version);

    config.horizon = Some(horizon.clone());
    config.save(&instance).await?;
    reporter.send(DownloadProgress::Horizon(HorizonProgress::BuildDone));

    info!(
        "Updated Horizon instance {instance_name} to {}",
        horizon.build_id
    );
    Ok(horizon)
}

fn check_channel(config: &HorizonConfig) -> Result<(), HorizonError> {
    if config.build_tag == build::RELEASE_TAG {
        Ok(())
    } else {
        Err(HorizonError::UnsupportedUpdateChannel(
            config.build_tag.clone(),
        ))
    }
}

fn retarget(horizon: &mut HorizonConfig, installed: &InstalledBuild, minecraft_version: String) {
    horizon.build_tag.clone_from(&installed.tag);
    horizon.build_id.clone_from(&installed.id);
    horizon.minecraft_version = minecraft_version;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn build(sha: &str) -> HorizonBuild {
        HorizonBuild {
            tag: build::RELEASE_TAG.to_owned(),
            asset: "horizon-linux-x86_64.tar.gz".to_owned(),
            asset_url: String::new(),
            sha256: sha.to_owned(),
        }
    }

    #[test]
    fn compares_builds_by_id() {
        let latest = build(&"a".repeat(64));
        assert!(needs_update("nightly-old", &latest));
        assert!(!needs_update(&latest.id(), &latest));
    }

    #[test]
    fn update_result_names_the_new_build() {
        let latest = build(&"b".repeat(64));
        let update = BuildUpdate {
            current: "nightly-old".to_owned(),
            latest: latest.clone(),
        };
        assert_eq!(update.id(), latest.id());
        assert_ne!(update.id(), update.current);
    }

    #[test]
    fn retargeting_keeps_fields_from_newer_launchers() {
        let mut horizon: HorizonConfig = serde_json::from_str(
            r#"{"build_tag": "nightly", "build_id": "nightly-old",
                "minecraft_version": "26.2", "future_field": {"a": 1}}"#,
        )
        .unwrap();
        let installed = InstalledBuild {
            dir: PathBuf::new(),
            id: "nightly-new".to_owned(),
            tag: "nightly".to_owned(),
        };

        retarget(&mut horizon, &installed, "26.3".to_owned());

        assert_eq!(horizon.build_tag, "nightly");
        assert_eq!(horizon.build_id, "nightly-new");
        assert_eq!(horizon.minecraft_version, "26.3");

        let json: serde_json::Value = serde_json::to_value(&horizon).unwrap();
        assert_eq!(json["future_field"]["a"], 1, "unknown fields survive");
    }

    #[test]
    fn refuses_updates_off_the_nightly_channel() {
        let config: HorizonConfig = serde_json::from_str(
            r#"{"build_tag": "v0.3.0", "build_id": "v0.3.0-abc",
                "minecraft_version": "26.3"}"#,
        )
        .unwrap();
        assert!(matches!(
            check_channel(&config),
            Err(HorizonError::UnsupportedUpdateChannel(tag)) if tag == "v0.3.0"
        ));
    }
}
