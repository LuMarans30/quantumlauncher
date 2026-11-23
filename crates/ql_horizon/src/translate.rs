//! Local, opt-in adapters for Horizon's content porter and Horizon-Translator.
//! Java code is never loaded by Horizon. Only validated shader or content ports
//! are installed.
use std::{
    fs,
    io::Read,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use ql_core::{Instance, no_window};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::{
    HorizonError, api, build,
    catalog::{self, Source, error},
    content,
};

type Result<T> = std::result::Result<T, HorizonError>;
const REPORT_LIMIT: u64 = 4 * 1024 * 1024;

/// Local launcher configuration only, never imported from an instance/modpack.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tools {
    pub shaderport: String,
    pub port_pack: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Status {
    InstalledShader,
    InstalledMod,
    NeedsReview,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub directory: PathBuf,
    pub status: Status,
    pub message: String,
}

pub fn root(instance: &Instance) -> PathBuf {
    instance.get_instance_path().join("horizon/conversions")
}

fn file(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    if !fs::symlink_metadata(path).map_err(error)?.is_file() {
        return Err(error(format!("Not a regular file: {}", path.display())));
    }
    fs::canonicalize(path).map_err(error)
}

fn bounded_text(path: &Path) -> Result<String> {
    let mut text = String::new();
    fs::File::open(file(path)?)
        .map_err(error)?
        .take(REPORT_LIMIT + 1)
        .read_to_string(&mut text)
        .map_err(error)?;
    if text.len() as u64 > REPORT_LIMIT {
        return Err(error("Conversion report exceeds size limit"));
    }
    Ok(text)
}

/// Validate before passing untrusted ZIPs to third-party tools. In particular,
/// Fabric's mod id is used by the converter as a directory name.
fn validate_input(path: &Path, source: Source) -> Result<()> {
    if fs::metadata(path).map_err(error)?.len() > 256 * 1024 * 1024 {
        return Err(error("Input exceeds 256 MiB"));
    }
    let mut zip = zip::ZipArchive::new(fs::File::open(path).map_err(error)?).map_err(error)?;
    if zip.len() > 30_000 {
        return Err(error("Too many archive entries"));
    }
    let mut total = 0u64;
    let mut names = std::collections::HashSet::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(error)?;
        let raw_name = entry.name().map_err(|e| error(e.to_string()))?;
        let name = raw_name.trim_end_matches('/');
        total = total.saturating_add(entry.size());
        if total > 512 * 1024 * 1024
            || name.contains(['\\', ':'])
            || name.split('/').any(|p| matches!(p, "" | "." | ".."))
            || !names.insert(name.to_owned())
            || entry
                .unix_mode()
                .is_some_and(|m| !matches!(m & 0o170000, 0 | 0o100000 | 0o040000))
        {
            return Err(error("Unsafe, duplicate or oversized archive entry"));
        }
    }
    if source == Source::Fabric {
        let entry = zip
            .by_name("fabric.mod.json")
            .map_err(|_| error("Only Fabric jars are supported by this translator"))?;
        let mut text = String::new();
        entry
            .take(256 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(error)?;
        if text.len() > 256 * 1024 {
            return Err(error("Fabric manifest is too large"));
        }
        let value: serde_json::Value = serde_json::from_str(&text).map_err(error)?;
        let id = value["id"]
            .as_str()
            .ok_or_else(|| error("Missing Fabric mod id"))?;
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
        {
            return Err(error("Invalid Fabric mod id"));
        }
    }
    Ok(())
}

pub async fn download_and_convert(
    instance: &Instance,
    build_id: &str,
    source: Source,
    id: &str,
    version: &str,
    tools: &Tools,
) -> Result<Outcome> {
    match source {
        Source::Fabric => file(&tools.port_pack).map_err(|_| {
            error("Choose the horizon-port-pack executable in Converter setup first")
        })?,
        Source::Shaders => file(&tools.shaderport).map_err(|_| {
            error("Choose the horizon-shaderport executable in Converter setup first")
        })?,
        Source::Horizon => return Err(error("Registry packages do not need translation")),
    };
    let release = catalog::modrinth_release(id, source, version).await?;
    let temp = tempfile::tempdir().map_err(error)?;
    let input = catalog::download(&release, temp.path()).await?;
    let outcome = convert(instance, build_id, source, &input, tools).await?;
    let origin = serde_json::json!({"project": release.project, "version": release.version, "file": release.file});
    fs::write(
        outcome.directory.join("origin.json"),
        serde_json::to_vec_pretty(&origin).map_err(error)?,
    )
    .map_err(error)?;
    Ok(outcome)
}

fn shader_report_ready(report: &str) -> bool {
    // Upstream fix::report / load::Eval::summary. An unknown future format is a
    // review result, NOT an installation. Exit code zero alone is insufficient.
    let summaries: Vec<_> = report
        .lines()
        .filter_map(|line| line.strip_prefix("- After: "))
        .collect();
    let [summary] = summaries.as_slice() else {
        return false;
    };
    let Some(count) = summary.strip_suffix(" programs load, 0 skipped") else {
        return false;
    };
    count.parse::<u64>().is_ok_and(|n| n > 0)
        && report.contains("Generated by horizon-shaderport.")
        && report.contains("## What Horizon loads now")
        && !report.contains("## Still skipped")
        && !report.contains("not drawn by Horizon")
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Verdict {
    Works,
    Partly,
    No,
}

#[derive(Deserialize)]
struct CarryReport {
    mod_id: String,
    loader: String,
    verdict: Verdict,
    summary: String,
    skipped: Vec<String>,
    depends: Vec<String>,
}

impl CarryReport {
    fn can_install(&self) -> bool {
        self.loader == "fabric"
            && matches!(self.verdict, Verdict::Works)
            && self.skipped.is_empty()
            && self
                .depends
                .iter()
                .all(|id| matches!(id.as_str(), "minecraft" | "java" | "fabricloader"))
            && content::valid_id(&self.mod_id)
    }
}

/// Package the content-only folder as a regular Horizon mod. Only assets and
/// data are carried over; don't install Java classes or native/Wasm code.
fn pack_carried_content(from: &Path, destination: &Path) -> Result<()> {
    if !fs::symlink_metadata(from).map_err(error)?.is_dir() {
        return Err(error("Expected a regular carry directory"));
    }
    let mut zip = zip::ZipWriter::new(fs::File::create_new(destination).map_err(error)?);
    let mut count = 0;
    let mut total = 0u64;
    let mut pending = vec![from.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).map_err(error)? {
            let entry = entry.map_err(error)?;
            let path = entry.path();
            count += 1;
            if count > 30_000 || !entry.file_name().to_str().is_some_and(catalog::safe_name) {
                return Err(error("Invalid or excessive content port paths"));
            }
            let relative = path.strip_prefix(from).map_err(error)?;
            let ty = entry.file_type().map_err(error)?;
            match relative
                .components()
                .next()
                .and_then(|p| p.as_os_str().to_str())
            {
                Some("assets" | "data") if ty.is_dir() => pending.push(path),
                Some("assets" | "data") if ty.is_file() && relative.components().count() > 1 => {
                    write_carried_file(&mut zip, relative, &path, &mut total)?;
                }
                Some("horizon-mod.toml") if ty.is_file() && relative.components().count() == 1 => {
                    let manifest: toml::Table =
                        toml::from_str(&bounded_text(&path)?).map_err(error)?;
                    if manifest.contains_key("native") || manifest.contains_key("wasm") {
                        return Err(error(
                            "A Fabric content port must not contain executable mod code",
                        ));
                    }
                    write_carried_file(&mut zip, relative, &path, &mut total)?;
                }
                _ => return Err(error("Unexpected path in the content port")),
            }
        }
    }
    zip.finish().map_err(error)?.flush().map_err(error)
}

fn write_carried_file(
    zip: &mut zip::ZipWriter<fs::File>,
    relative: &Path,
    path: &Path,
    total: &mut u64,
) -> Result<()> {
    let name = relative
        .iter()
        .map(|c| {
            c.to_str()
                .ok_or_else(|| error("Invalid content port filename"))
        })
        .collect::<Result<Vec<_>>>()?
        .join("/");
    let budget = 512 * 1024 * 1024 - *total;
    let mut input = fs::File::open(file(path)?).map_err(error)?.take(budget + 1);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file(name, options).map_err(error)?;
    let read = std::io::copy(&mut input, zip).map_err(error)?;
    if read > budget {
        return Err(error("Content port exceeds 512 MiB"));
    }
    *total += read;
    Ok(())
}

async fn install_carry(
    instance: &Instance,
    build_id: &str,
    out: &Path,
) -> Result<(Status, String)> {
    if !fs::symlink_metadata(out).map_err(error)?.is_dir() {
        return Err(error("Expected a regular converter output directory"));
    }
    let report: CarryReport =
        serde_json::from_str(&bounded_text(&out.join("check.json"))?).map_err(error)?;
    if !report.can_install() {
        return Ok((
            Status::NeedsReview,
            format!(
                "{} Nothing installed: only complete Fabric content ports without external dependencies are installed automatically.",
                report.summary
            ),
        ));
    }
    let build = build::installed_build(build_id).await?;
    let api: api::LaunchApi = api::query(&build.client(), "horizon-client").await?;
    let game = instance.get_dot_minecraft_path();
    let carry = out.join("carry");
    tokio::task::spawn_blocking(move || {
        install_checked_carry(&game, &carry, &report, api.content.as_ref())?;
        Ok((
            Status::InstalledMod,
            format!(
                "{} Installed as a Horizon content mod. Enable it in Installed mods & shaders.",
                report.summary
            ),
        ))
    })
    .await
    .map_err(error)?
}

fn install_checked_carry(
    game: &Path,
    carry: &Path,
    report: &CarryReport,
    advertised: Option<&serde_json::Value>,
) -> Result<()> {
    if !report.can_install() {
        return Err(error("Not an installable Fabric content port"));
    }
    let temp = tempfile::tempdir().map_err(error)?;
    let artifact = temp.path().join(format!("{}.hmod", report.mod_id));
    pack_carried_content(carry, &artifact)?;
    let metadata = content::inspect(&artifact, content::Kind::Mod)?;
    if metadata.id != report.mod_id || !catalog::advertises_api(advertised, "mod", &metadata.api) {
        return Err(error(
            "Ported package identity or API does not match the report and the installed game",
        ));
    }
    content::install(game, &artifact)
}

/// Keep inputs and reports outside the game directories, including on failure.
pub async fn convert(
    instance: &Instance,
    build_id: &str,
    source: Source,
    input: &Path,
    tools: &Tools,
) -> Result<Outcome> {
    if source == Source::Horizon {
        return Err(error("Registry packages do not need translation"));
    }
    let input = file(input)?;
    let filename = input
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| error("Invalid input filename"))?;
    if !catalog::safe_name(filename)
        || !filename.ends_with(if source == Source::Fabric {
            ".jar"
        } else {
            ".zip"
        })
    {
        return Err(error("Unexpected input extension/name"));
    }
    validate_input(&input, source)?;
    let root = root(instance);
    fs::create_dir_all(&root).map_err(error)?;
    let directory = tempfile::Builder::new()
        .prefix("conversion-")
        .tempdir_in(&root)
        .map_err(error)?
        .keep();
    let directory = fs::canonicalize(directory).map_err(error)?;
    let incoming = directory.join("input");
    fs::create_dir(&incoming).map_err(error)?;
    let saved = incoming.join(filename);
    fs::copy(&input, &saved).map_err(error)?;
    let out = directory.join("output");
    let log = fs::File::create(directory.join("converter.log")).map_err(error)?;
    let run = async {
        let mut command;
        let mut provenance = serde_json::json!({
            "adapter": if source == Source::Fabric { "horizon-port-pack" } else { "Horizon-Translator/1.0.10" },
            "horizon_build": build_id,
            "source_sha256": build::sha256_of_file(&saved).await?,
            "input": filename,
        });
        if source == Source::Shaders {
            let program = file(&tools.shaderport)?;
            provenance["tool_sha256"] = build::sha256_of_file(&program).await?.into();
            command = Command::new(program);
            command.arg("port").arg(&saved).arg("--out").arg(&out);
        } else {
            let program = file(&tools.port_pack).map_err(|_| error("Choose the horizon-port-pack executable in Converter setup first"))?;
            provenance["tool_sha256"] = build::sha256_of_file(&program).await?.into();
            command = Command::new(program);
            command.arg("check").arg(&saved).arg(&out);
        }
        fs::write(directory.join("provenance.json"), serde_json::to_vec_pretty(&provenance).map_err(error)?).map_err(error)?;
        no_window!(command);
        command.current_dir(&directory).stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().map_err(error)?)).stderr(Stdio::from(log))
            .kill_on_drop(true);
        // Do not pass Minecraft/MS credentials or game launch settings to tools.
        for key in ["HORIZON_SESSION", "HORIZON_SESSION_FILE", "HORIZON_HOME", "JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS"] { command.env_remove(key); }
        let mut child = command.spawn().map_err(error)?;
        let status = match tokio::time::timeout(Duration::from_secs(15 * 60), child.wait()).await {
            Ok(result) => result.map_err(error)?,
            Err(_) => { let _ = child.kill().await; return Err(error("Conversion timed out after 15 minutes")); }
        };
        if !status.success() { return Err(error(format!("Converter failed ({status}); see converter.log. License refusals are not overridden."))); }
        if source == Source::Shaders {
            let stem = filename.trim_end_matches(".zip");
            let report = bounded_text(&out.join(format!("{stem}-horizon.md")))?;
            if !shader_report_ready(&report) { return Ok((Status::NeedsReview, "Shader report is incomplete, has skipped programs, or uses an unknown format. Nothing installed.".into())); }
            let artifact = file(out.join(format!("{stem}-horizon.zip")))?;
            content::inspect(&artifact, content::Kind::ShaderPack)?;
            content::install(&instance.get_dot_minecraft_path(), &artifact)?;
            Ok((Status::InstalledShader, "Converted shader installed. Select it in Horizon's shader settings; visual/runtime compatibility is still experimental.".into()))
        } else {
            install_carry(instance, build_id, &out).await
        }
    }.await;
    let (status, message) = match run {
        Ok(result) => result,
        Err(e) => (Status::Failed, e.to_string()),
    };
    let outcome = Outcome {
        directory,
        status,
        message,
    };
    fs::write(
        outcome.directory.join("result.json"),
        serde_json::to_vec_pretty(&outcome).map_err(error)?,
    )
    .map_err(error)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn carry_report() -> CarryReport {
        serde_json::from_value(serde_json::json!({
            "mod_id": "example", "loader": "fabric", "verdict": "works",
            "summary": "Two items port", "skipped": [], "depends": ["minecraft"]
        }))
        .unwrap()
    }

    #[test]
    fn only_complete_fabric_content_is_installable() {
        let mut report = carry_report();
        assert!(report.can_install());
        report.skipped.push("mixins".into());
        assert!(!report.can_install());
        report.skipped.clear();
        report.depends.push("unported_api".into());
        assert!(!report.can_install());
        assert!(
            !CarryReport {
                verdict: Verdict::Partly,
                ..carry_report()
            }
            .can_install()
        );
        assert!(
            !CarryReport {
                loader: "forge".into(),
                ..carry_report()
            }
            .can_install()
        );
    }

    #[test]
    fn complete_port_installs_as_opt_in_hmod_without_overwriting() {
        let temp = tempfile::tempdir().unwrap();
        let carry = temp.path().join("carry");
        let game = temp.path().join("game");
        fs::create_dir_all(carry.join("data/example/recipe")).unwrap();
        fs::write(carry.join("data/example/recipe/recipe.json"), "{}").unwrap();
        fs::write(
            carry.join("horizon-mod.toml"),
            "id = 'example'\nname = 'Example'\nversion = '1'\napi = '0.2'\n",
        )
        .unwrap();
        let report = carry_report();
        let advertised = serde_json::json!({"mod": {"api": ["0.2"]}});
        assert!(install_checked_carry(&game, &carry, &report, None).is_err());
        assert!(!game.exists());
        fs::write(carry.join("mod.wasm"), "unexpected").unwrap();
        assert!(install_checked_carry(&game, &carry, &report, Some(&advertised)).is_err());
        fs::remove_file(carry.join("mod.wasm")).unwrap();
        install_checked_carry(&game, &carry, &report, Some(&advertised)).unwrap();
        let packages = content::list(&game).unwrap();
        assert_eq!(packages[0].filename, "example.hmod");
        assert!(!packages[0].enabled);
        assert!(install_checked_carry(&game, &carry, &report, Some(&advertised)).is_err());
    }

    #[tokio::test]
    #[ignore = "requires a scratch QL_DIR with an installed Nightly and the official port-pack CLI"]
    async fn official_fabric_port_smoke() {
        let temp = tempfile::tempdir().unwrap();
        let jar = temp.path().join("fabric-fixture.jar");
        let mut zip = zip::ZipWriter::new(fs::File::create(&jar).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in [
            (
                "fabric.mod.json",
                r#"{"schemaVersion":1,"id":"ql_fabric_smoke","name":"Smoke test","version":"1.0","license":"MIT"}"#,
            ),
            ("assets/ql_fabric_smoke/models/item/ruby.json", "{}"),
            (
                "data/ql_fabric_smoke/recipe/ruby.json",
                r#"{"type":"minecraft:crafting_shapeless","ingredients":["minecraft:red_dye"],"result":{"id":"ql_fabric_smoke:ruby"}}"#,
            ),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        let tools = Tools {
            port_pack: std::env::var("QL_FABRIC_SMOKE_PORT_PACK").unwrap(),
            ..Tools::default()
        };
        let instance = Instance::client("fabric-smoke");
        let game = instance.get_dot_minecraft_path();
        let package = game.join("horizon-mods/ql_fabric_smoke.hmod");
        assert!(!package.exists(), "use an empty smoke-test instance");
        let result = convert(
            &instance,
            &std::env::var("QL_HORIZON_SMOKE_BUILD").unwrap(),
            Source::Fabric,
            &jar,
            &tools,
        )
        .await
        .unwrap();
        assert_eq!(
            result.status,
            Status::InstalledMod,
            "{} (reports: {})",
            result.message,
            result.directory.display()
        );
        assert!(package.is_file());
        content::remove(&game, content::Kind::Mod, "ql_fabric_smoke.hmod").unwrap();
    }

    #[test]
    fn report_parser_fails_closed() {
        let report = |summary| {
            format!(
                "Generated by horizon-shaderport.\n- After: {summary}\n## What Horizon loads now\n"
            )
        };
        assert!(shader_report_ready(&report("12 programs load, 0 skipped")));
        for summary in [
            "0 programs load, 0 skipped",
            "12 programs load, 1 skipped",
            "12 programs load, 0 skipped, LOADER PANICKED",
            "unknown",
        ] {
            assert!(!shader_report_ready(&report(summary)));
        }
        assert!(!shader_report_ready(&format!(
            "{}- After: 1 programs load, 0 skipped\n",
            report("12 programs load, 0 skipped")
        )));
        assert!(!shader_report_ready(""));
    }
}
