//! Horizon's Launch API (`--launch-api`), the contract between a launcher
//! and a Horizon build.
//!
//! Upstream reference:
//! <https://github.com/horizon-rust/horizon/blob/main/docs/api/launch-reference.md>

use std::{path::Path, process::Stdio, time::Duration};

use ql_core::no_window;
use serde::Deserialize;
use tokio::process::Command;

use crate::error::HorizonError;

/// How long `--launch-api` may take before it is treated as hung.
const API_TIMEOUT: Duration = Duration::from_secs(20);

/// What `horizon-client --launch-api` prints.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct LaunchApi {
    pub launch_api: String,
    #[serde(default)]
    pub options: Vec<LaunchOption>,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
}

/// One option of the Launch API.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct LaunchOption {
    pub name: String,
    #[serde(default)]
    pub value: Option<String>,
}

/// What `horizon-prepare --launch-api` prints.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct PrepareApi {
    pub launch_api: String,
    pub minecraft: String,
    pub java_major: u32,
    #[serde(default)]
    pub options: Vec<LaunchOption>,
}

impl LaunchApi {
    pub fn require(&self, option: &str) -> Result<(), HorizonError> {
        require_option(&self.options, &self.launch_api, option)
    }

    #[must_use]
    pub fn allows(&self, option: &str) -> bool {
        self.options.iter().any(|n| n.name == option)
    }

    #[must_use]
    pub fn option(&self, option: &str) -> Option<&LaunchOption> {
        self.options.iter().find(|n| n.name == option)
    }
}

impl PrepareApi {
    /// Check every option used for a full preparation before downloading files.
    pub fn require_preparation_options(&self) -> Result<(), HorizonError> {
        for option in [
            "--prepare",
            "--server-jar",
            "--version-json",
            "--client-jar",
            "--libraries-dir",
            "--assets-dir",
            "--output",
            "--complete",
            "--progress",
            "--java-home",
            "--check",
        ] {
            require_option(&self.options, &self.launch_api, option)?;
        }
        Ok(())
    }
}

fn require_option(
    options: &[LaunchOption],
    version: &str,
    option: &str,
) -> Result<(), HorizonError> {
    if options.iter().any(|entry| entry.name == option) {
        Ok(())
    } else {
        Err(HorizonError::UnsupportedOption {
            option: option.to_owned(),
            api_version: version.to_owned(),
        })
    }
}

/// Runs `<program> --launch-api` and parses its one-line JSON reply.
pub async fn query<T>(program: &Path, label: &str) -> Result<T, HorizonError>
where
    T: serde::de::DeserializeOwned,
{
    let mut command = Command::new(program);
    command.arg("--launch-api");
    no_window!(command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = tokio::time::timeout(API_TIMEOUT, command.output())
        .await
        .map_err(|_| HorizonError::CommandTimedOut {
            program: program.to_owned(),
            seconds: API_TIMEOUT.as_secs(),
        })?
        .map_err(|error| HorizonError::CommandError {
            program: program.to_owned(),
            error,
        })?;

    if !output.status.success() {
        return Err(HorizonError::CommandFailed {
            program: program.to_owned(),
            status: output.status.to_string(),
            stderr: format!(
                "{label} --launch-api failed:\n{}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    serde_json::from_slice(&output.stdout).map_err(|err| {
        HorizonError::BadApiReply(format!(
            "{label} --launch-api said: {}\n({err})",
            String::from_utf8_lossy(&output.stdout).trim()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_launch_api_reply() {
        // Trimmed from a real `horizon-client --launch-api` reply.
        let reply = r#"{
            "launch_api": "0.1.0",
            "options": [
                {"name": "--resources", "value": "DIR"},
                {"name": "--game-dir", "value": "DIR"},
                {"name": "--session-stdin"}
            ],
            "content": {"mods": []}
        }"#;
        let api: LaunchApi = serde_json::from_str(reply).unwrap();
        assert_eq!(api.launch_api, "0.1.0");
        api.require("--session-stdin").unwrap();
        api.require("--game-dir").unwrap();
        assert!(api.require("--fullscreen").is_err());
    }

    #[test]
    fn parses_a_prepare_api_reply() {
        let reply = r#"{"launch_api": "0.1.0", "horizon": "0.2.1",
            "minecraft": "26.3", "java_major": 25, "options": []}"#;
        let api: PrepareApi = serde_json::from_str(reply).unwrap();
        assert_eq!(api.minecraft, "26.3");
        assert_eq!(api.java_major, 25);
    }
}
