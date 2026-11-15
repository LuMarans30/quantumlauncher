//! Creates instance folders. Shared builds are downloaded here; Mojang's
//! files are deferred until the first authenticated launch.

use ql_core::{
    Instance, InstanceConfigJson, InstanceKind, IntoIoError, LAUNCHER_VERSION_NAME, info,
    json::instance_config::VersionInfo, sanitize_instance_name,
};

use crate::{HorizonConfig, Reporter, build, error::HorizonError};

/// Creates an instance, removing its folder if setup fails.
pub async fn create_instance(name: String, reporter: Reporter<'_>) -> Result<String, HorizonError> {
    let name = sanitize_instance_name(name);
    if name.is_empty() {
        return Err(HorizonError::InvalidName);
    }

    let instance = Instance::client(&name);
    let dir = instance.get_instance_path();
    let parent = dir.parent().expect("instance has a parent folder");
    tokio::fs::create_dir_all(parent).await.path(parent)?;
    // Reserve the name atomically; only this creator may clean up the folder.
    match tokio::fs::create_dir(&dir).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(HorizonError::InstanceAlreadyExists(name));
        }
        Err(error) => return Err(error.path(&dir).into()),
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
    tokio::fs::create_dir_all(&dot_minecraft)
        .await
        .path(&dot_minecraft)?;

    let installed = build::ensure_build(reporter).await?;
    reporter.send(ql_core::DownloadProgress::Horizon(
        ql_core::HorizonProgress::BuildDone,
    ));
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
