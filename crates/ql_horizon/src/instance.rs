//! Creating Horizon instances.
//!
//! Only the Horizon build is fetched here; the game's Mojang files wait
//! for the first launch. Builds and prepared resources are shared by all
//! instances (`QuantumLauncher/horizon/`), only the game folder is per
//! instance.

use ql_core::{
    Instance, InstanceConfigJson, InstanceKind, IntoIoError, LAUNCHER_VERSION_NAME,
    file_utils::exists, info, json::instance_config::VersionInfo, sanitize_instance_name,
};

use crate::{HorizonConfig, Reporter, build, error::HorizonError};

/// Creates a Horizon instance and writes its config.
///
/// Only the MIT-licensed horizon build is fetched here; Mojang's files
/// are downloaded on the first launch, after the sign-in gate. The
/// folder is removed again if anything fails, so a broken download never
/// leaves a half-instance behind.
pub async fn create_instance(name: String, reporter: Reporter<'_>) -> Result<String, HorizonError> {
    let name = sanitize_instance_name(name);
    if name.is_empty() {
        return Err(HorizonError::InvalidName);
    }

    let instance = Instance::client(&name);
    let dir = instance.get_instance_path();
    if exists(&dir).await {
        return Err(HorizonError::InstanceAlreadyExists(name));
    }

    info!("Started creating Horizon instance: {name}");
    if let Err(err) = create(&instance, reporter).await {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Err(err);
    }
    info!("Finished creating Horizon instance: {name}");

    Ok(name)
}

async fn create(instance: &Instance, reporter: Reporter<'_>) -> Result<(), HorizonError> {
    let instance_dir = instance.get_instance_path();
    let dot_minecraft = instance.get_dot_minecraft_path();
    tokio::fs::create_dir_all(&instance_dir)
        .await
        .path(&instance_dir)?;
    tokio::fs::create_dir_all(&dot_minecraft)
        .await
        .path(&dot_minecraft)?;

    let installed = build::ensure_build(reporter).await?;
    reporter.send(ql_core::DownloadProgress::Horizon(
        ql_core::HorizonProgress::BuildDone,
    ));
    // The build's own Minecraft pin; the downloads wait for the first
    // signed-in launch.
    let minecraft_version = build::research_lock(&installed.dir).await?.version;

    let mut config = InstanceConfigJson::new(
        InstanceKind::Client,
        false,
        VersionInfo::new(&format!("horizon-{}", installed.id)),
    );
    config.horizon = Some(HorizonConfig::new(
        installed.tag,
        installed.id.clone(),
        minecraft_version,
    ));
    config.save(instance).await?;

    let version_file = instance_dir.join("launcher_version.txt");
    tokio::fs::write(&version_file, LAUNCHER_VERSION_NAME)
        .await
        .path(version_file)?;

    Ok(())
}
