//! Launching Horizon instances.
//!
//! A Horizon instance is a normal instance folder whose `config.json` has
//! a [`HorizonConfig`] section, which tells the launcher to skip the Java
//! pipeline and start the game from its own binary.

use std::{
    process::Stdio,
    sync::{Arc, mpsc::Sender},
};

use ql_core::{
    GenericProgress, Instance, InstanceConfigJson, IntoIoError, LaunchedProcess, info,
    json::GlobalSettings, no_window, pt,
};
use serde::Serialize;
use tokio::{io::AsyncWriteExt, process::Command};

use crate::{
    HorizonConfig, Reporter,
    api::{self, LaunchApi},
    build,
    error::HorizonError,
    prepare,
};

/// Options a user's extra game arguments may never pass: they control how
/// the launcher set the game up, and some (`--session-file`) make the game
/// read and delete a file. Horizon's parser treats `--gameDir` as
/// `--game-dir`.
const LAUNCHER_CONTROLLED_OPTIONS: &[&str] = &[
    "--session-file",
    "--session-stdin",
    "--game-dir",
    "--gameDir",
    "--resources",
    "--home",
    "--player-dir",
    "--dedicated",
    "--control",
    "--control-socket",
    "--transfers",
    "--benchmark",
    "--test-terrain",
    "--frames",
    "--world-frames",
    "--smoke-report",
    "--timeout-seconds",
    "--dev-mod",
    "--launch-api",
];

/// Environment variables an instance may not set: Horizon reads the
/// session file, folders and player name from them.
const LAUNCHER_CONTROLLED_ENV: &[&str] = &[
    "HORIZON_SESSION_FILE",
    "HORIZON_HOME",
    "HORIZON_PLAYER_NAME",
    "HORIZON_MINECRAFT_DIR",
];

/// The account handed to the game on standard input, so the token never
/// lands in the process list.
#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
    pub user_type: &'static str,
}

impl Session {
    #[must_use]
    pub fn new(name: String, uuid: String, access_token: String) -> Self {
        Self {
            name,
            uuid,
            access_token,
            user_type: "msa",
        }
    }

    fn to_json(&self) -> String {
        serde_json::to_string(self).expect("session serializes")
    }
}

/// Launches a Horizon instance, preparing the build's resources first if
/// needed.
///
/// `session` is `None` for offline play, which then uses `offline_name`.
/// Callers should only pass `None` when [`crate::ALLOW_OFFLINE`] is on.
#[allow(clippy::too_many_arguments)]
pub async fn launch(
    instance_name: &str,
    config: &InstanceConfigJson,
    horizon: &HorizonConfig,
    session: Option<Session>,
    offline_name: &str,
    java_progress: Option<&Sender<GenericProgress>>,
    global_settings: Option<&GlobalSettings>,
) -> Result<LaunchedProcess, HorizonError> {
    // Enforced here, not just by the caller: preparation downloads
    // Mojang's files and must never run unauthenticated.
    match &session {
        None if !crate::ALLOW_OFFLINE => return Err(HorizonError::OfflineDisabled),
        Some(session)
            if session.name.trim().is_empty()
                || session.uuid.trim().is_empty()
                || session.access_token.trim().is_empty() =>
        {
            return Err(HorizonError::InvalidSession);
        }
        _ => {}
    }

    let reporter = match java_progress {
        Some(sender) => Reporter::Generic(sender),
        None => Reporter::Nobody,
    };

    let build = match build::installed_build(&horizon.build_id).await {
        Ok(build) => build,
        // A copied tree can lose its exec bits; repair it instead of making
        // the player re-download the whole build.
        Err(HorizonError::BuildMissing(dir)) => {
            if build::repair_permissions(&dir).await? {
                build::installed_build(&horizon.build_id).await?
            } else {
                return Err(HorizonError::BuildMissing(dir));
            }
        }
        Err(error) => return Err(error),
    };
    let (width, height) = config.get_window_size(global_settings);

    // Ask the build what it accepts first: the Launch API is experimental,
    // so a dropped option must fail clearly here instead of as exit code 2
    // mid-launch.
    let api: LaunchApi = api::query(&build.client(), "horizon-client").await?;
    api.require("--resources")?;
    api.require("--game-dir")?;
    api.require("--home")?;
    if session.is_some() {
        api.require("--session-stdin")?;
    }
    if width.is_some() {
        api.require("--width")?;
    }
    if height.is_some() {
        api.require("--height")?;
    }

    // Imported instances may supply arguments, but not launcher-owned options.
    let extra_args: Vec<&str> = config
        .game_args
        .as_ref()
        .map(|args| {
            args.iter()
                .map(String::as_str)
                .filter(|n| !n.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();
    check_extra_args(&extra_args, &api)?;

    let prepared = prepare::ensure_prepared(&build, reporter).await?;

    let instance = Instance::client(instance_name);
    let game_dir = instance.get_dot_minecraft_path();
    let home = instance.get_instance_path().join("horizon");
    tokio::fs::create_dir_all(&game_dir).await.path(&game_dir)?;
    tokio::fs::create_dir_all(&home).await.path(&home)?;

    let mut command = Command::new(build.client());
    command
        .arg("--resources")
        .arg(&prepared.dir)
        .arg("--game-dir")
        .arg(&game_dir)
        .arg("--home")
        .arg(&home);

    if session.is_some() {
        command.arg("--session-stdin");
    } else {
        // Offline: no account to hand over. `--name` sets the name used by
        // offline play and LAN, but isn't promised by the Launch API.
        info!(
            "Starting Horizon offline as {offline_name}: online-mode servers will refuse this player"
        );
        command.arg("--name").arg(offline_name);
    }

    if let Some(width) = width {
        command.arg("--width").arg(width.to_string());
    }
    if let Some(height) = height {
        command.arg("--height").arg(height.to_string());
    }

    if !extra_args.is_empty() {
        info!("Horizon extra arguments: {extra_args:?}");
        command.args(&extra_args);
    }

    apply_env_vars(config, &mut command);

    command
        .stdin(if session.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    no_window!(command);

    let mut child = command
        .spawn()
        .map_err(|error| HorizonError::CommandError {
            program: build.client(),
            error,
        })?;

    // Closing stdin after the write tells the game the session is complete.
    if let Some(session) = session {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(session.to_json().as_bytes())
                .await
                .path("Horizon's standard input")?;
            drop(stdin);
        }
    }

    if let Some(id) = child.id() {
        pt!("Horizon launched (PID: {id})");
    }

    Ok(LaunchedProcess {
        child: Arc::new(tokio::sync::Mutex::new(child)),
        instance,
        is_classic_server: false,
    })
}

fn apply_env_vars(config: &InstanceConfigJson, command: &mut Command) {
    for (key, value) in allowed_env_vars(config) {
        command.env(key, value);
    }
}

/// Filters out launcher-controlled environment variables.
fn allowed_env_vars(config: &InstanceConfigJson) -> Vec<(String, String)> {
    let Some(vars) = config
        .global_settings
        .as_ref()
        .and_then(|settings| settings.env_vars.as_ref())
    else {
        return Vec::new();
    };

    let mut allowed = Vec::new();
    for var in vars {
        let Some((key, value)) = var.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        if LAUNCHER_CONTROLLED_ENV
            .iter()
            .any(|name| name.eq_ignore_ascii_case(key))
        {
            info!("Ignoring Horizon environment variable {key}: the launcher controls it");
            continue;
        }
        allowed.push((key.to_owned(), value.to_owned()));
    }
    allowed
}

/// Refuses extras the build doesn't promise and the launcher's own options.
fn check_extra_args(args: &[&str], api: &LaunchApi) -> Result<(), HorizonError> {
    let mut expects_value = None;

    for &arg in args {
        if expects_value.take().is_some() {
            // A value for the previous option; Horizon doesn't accept
            // `--option=value`, so values are separate arguments.
            continue;
        }

        if !arg.starts_with('-') {
            return Err(HorizonError::InvalidExtraArgument {
                argument: arg.to_owned(),
                reason: "a value has to follow the option it belongs to",
            });
        }

        if LAUNCHER_CONTROLLED_OPTIONS.contains(&arg) {
            return Err(HorizonError::ForbiddenOption {
                option: arg.to_owned(),
            });
        }

        let Some(option) = api.option(arg) else {
            return Err(HorizonError::UnsupportedOption {
                option: arg.to_owned(),
                api_version: api.launch_api.clone(),
            });
        };
        if option.value.is_some() {
            expects_value = Some(arg);
        }
    }

    if let Some(option) = expects_value {
        return Err(HorizonError::InvalidExtraArgument {
            argument: option.to_owned(),
            reason: "this option needs a value",
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(options: &[(&str, Option<&str>)]) -> LaunchApi {
        LaunchApi {
            launch_api: "0.1.0".to_owned(),
            options: options
                .iter()
                .map(|(name, value)| api::LaunchOption {
                    name: (*name).to_owned(),
                    value: value.map(str::to_owned),
                })
                .collect(),
            content: None,
        }
    }

    fn check(args: &[&str], api: &LaunchApi) -> Result<(), HorizonError> {
        check_extra_args(args, api)
    }

    #[test]
    fn filters_extra_game_arguments() {
        let api = api(&[
            ("--fullscreen", None),
            ("--width", Some("N")),
            ("--quickPlaySingleplayer", Some("WORLD")),
        ]);

        check(&["--fullscreen"], &api).unwrap();
        check(&["--width", "1280"], &api).unwrap();
        check(&[], &api).unwrap();

        let err = check(&["--session-file", "/tmp/x"], &api).unwrap_err();
        assert!(matches!(err, HorizonError::ForbiddenOption { .. }), "{err}");

        for launcher_option in ["--gameDir", "--game-dir", "--home", "--dedicated"] {
            let err = check(&[launcher_option], &api).unwrap_err();
            assert!(
                matches!(err, HorizonError::ForbiddenOption { .. }),
                "{launcher_option} must be refused"
            );
        }

        let err = check(&["--nope"], &api).unwrap_err();
        assert!(
            matches!(err, HorizonError::UnsupportedOption { .. }),
            "{err}"
        );

        let err = check(&["100"], &api).unwrap_err();
        assert!(
            matches!(err, HorizonError::InvalidExtraArgument { .. }),
            "{err}"
        );

        let err = check(&["--width"], &api).unwrap_err();
        assert!(
            matches!(err, HorizonError::InvalidExtraArgument { .. }),
            "an option that takes a value needs one: {err}"
        );
    }

    #[test]
    fn environment_variables_cannot_take_over_the_launch() {
        let mut config = InstanceConfigJson::new(
            ql_core::InstanceKind::Client,
            false,
            ql_core::json::instance_config::VersionInfo::new("horizon-test"),
        );
        config.global_settings = Some(
            serde_json::from_str(
                r#"{"env_vars": [
                    "LD_PRELOAD=/tmp/evil.so",
                    "HORIZON_SESSION_FILE=/home/user/.ssh/id_rsa",
                    "horizon_home=/tmp/x"
                ]}"#,
            )
            .unwrap(),
        );

        let applied = allowed_env_vars(&config);

        assert!(
            applied.iter().any(|(key, _)| key == "LD_PRELOAD"),
            "ordinary environment variables still apply: {applied:?}"
        );
        for forbidden in ["HORIZON_SESSION_FILE", "horizon_home"] {
            assert!(
                !applied
                    .iter()
                    .any(|(key, _)| key.eq_ignore_ascii_case(forbidden)),
                "{forbidden} must not reach the game: {applied:?}"
            );
        }
    }

    #[test]
    fn session_json_matches_horizons_format() {
        let session = Session::new(
            "Alex".to_owned(),
            "069a79f444e94726a5befca90e38aaf5".to_owned(),
            "token".to_owned(),
        );
        let json: serde_json::Value = serde_json::from_str(&session.to_json()).unwrap();
        assert_eq!(json["name"], "Alex");
        assert_eq!(json["uuid"], "069a79f444e94726a5befca90e38aaf5");
        assert_eq!(json["access_token"], "token");
        assert_eq!(json["user_type"], "msa");
    }
}
