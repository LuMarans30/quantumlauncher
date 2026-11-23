//! Catalog metadata and verified, staged downloads. Never uses Java's install path.
//! Registry packages require hashes, revocation checks and any advertised minisign
//! signature.
use std::{path::Path, time::Duration};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};

use crate::{HorizonError, api, build, content};

pub const REGISTRY: &str = "https://horizon-rust.github.io/horizon-mods/index.json";
const MAX_FILE: u64 = 256 * 1024 * 1024;

type Result<T> = std::result::Result<T, HorizonError>;
pub(crate) fn error(e: impl std::fmt::Display) -> HorizonError {
    HorizonError::Message(e.to_string())
}

#[derive(Debug, Clone)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub description: Option<String>,
    pub screenshots: Vec<String>,
    pub kind: ProjectKind,
    pub downloads: u64,
    pub icon_url: Option<String>,
    pub blocked: Option<String>,
    /// Installed GLSL ZIPs have content-derived IDs instead of a manifest project ID.
    pub shaderpack_ids: Option<Vec<String>>,
}

impl Project {
    pub fn matches_package(&self, package: &content::Package) -> bool {
        use content::Kind;
        package.metadata.as_ref().is_ok_and(|m| {
            match (self.kind, package.kind, &self.shaderpack_ids) {
                (ProjectKind::Shader, Kind::ShaderPack, Some(ids)) => ids.contains(&m.id),
                (ProjectKind::Shader, Kind::Shader, None) | (ProjectKind::Mod, Kind::Mod, None) => {
                    self.id == m.id
                }
                _ => false,
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    Mod,
    Shader,
}

impl ProjectKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mod => "Mod",
            Self::Shader => "Shader",
        }
    }
}

pub const PAGE_SIZE: usize = 20;

#[derive(Debug, Clone)]
pub struct Page {
    pub projects: Vec<Project>,
    pub has_next: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Registry(Vec<Project>);

impl Registry {
    pub fn append_next(&self, results: &mut Page, query: &str, kind: Option<ProjectKind>) {
        if results.has_next {
            let next = self.page(query, kind, results.projects.len());
            results.projects.extend(next.projects);
            results.has_next = next.has_next;
        }
    }

    pub fn page(&self, query: &str, kind: Option<ProjectKind>, offset: usize) -> Page {
        let query = query.to_lowercase();
        let words: Vec<_> = query.split_whitespace().collect();
        let mut matching = self
            .0
            .iter()
            .filter(|project| {
                kind.map_or_else(|| project.blocked.is_none(), |kind| project.kind == kind)
                    && (words.is_empty() || {
                        let text = format!("{} {} {}", project.name, project.id, project.summary)
                            .to_lowercase();
                        words.iter().all(|word| text.contains(word))
                    })
            })
            .skip(offset);
        let projects = matching.by_ref().take(PAGE_SIZE).cloned().collect();
        Page {
            projects,
            has_next: matching.next().is_some(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileSpec {
    pub name: String,
    pub url: String,
    pub size: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub sha512: String,
    #[serde(default)]
    pub signature: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub project: String,
    pub version: String,
    pub file: FileSpec,
    pub key: Option<String>,
    api: Option<String>,
    side: Option<String>,
    capabilities: Vec<String>,
    schema: Option<u64>,
}

#[derive(Deserialize)]
struct Index {
    schema: u32,
    projects: Vec<RegistryProject>,
    // Missing revocation information is not treated as an empty list.
    revoked: Vec<Revoked>,
}
#[derive(Deserialize)]
struct RegistryProject {
    id: String,
    name: String,
    summary: String,
    description: Option<String>,
    #[serde(default)]
    screenshots: Vec<String>,
    kind: String,
    key: Option<String>,
    #[serde(default)]
    downloads: u64,
    icon: Option<String>,
    versions: Vec<RegistryVersion>,
}
#[derive(Deserialize)]
struct RegistryVersion {
    version: String,
    api: String,
    game: String,
    side: Option<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    file: FileSpec,
}
#[derive(Deserialize)]
struct Revoked {
    id: String,
    sha256: Option<String>,
}

pub(crate) fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.ends_with(['.', ' '])
        && !name.contains(['/', '\\', ':', '\0', '<', '>', '"', '|', '?', '*'])
        && !name.chars().any(char::is_control)
        && !matches!(
            name.split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase()
                .as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
}

fn allowed_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some(
                "horizon-rust.github.io"
                    | "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}

/// Bounded bodies and timeouts, including redirects. No arbitrary mirror URLs.
async fn get(url: &str, limit: u64) -> Result<Vec<u8>> {
    let url = reqwest::Url::parse(url).map_err(error)?;
    if !allowed_url(&url) {
        return Err(error("Untrusted catalog/download URL"));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .user_agent("QuantumLauncher Horizon content manager")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || !allowed_url(attempt.url()) {
                attempt.error("Untrusted redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(error)?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(error)?
        .error_for_status()
        .map_err(error)?;
    if response.content_length().is_some_and(|n| n > limit) {
        return Err(error("Download exceeds size limit"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(error)? {
        if bytes.len() as u64 + chunk.len() as u64 > limit {
            return Err(error("Download exceeds size limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn index() -> Result<Index> {
    let index: Index =
        serde_json::from_slice(&get(REGISTRY, 16 * 1024 * 1024).await?).map_err(error)?;
    if index.schema != 1 {
        return Err(error(
            "Unsupported Horizon registry schema; update the launcher",
        ));
    }
    Ok(index)
}

pub async fn browse_registry() -> Result<Registry> {
    Ok(registry_for_browsing(index().await?))
}

fn registry_for_browsing(index: Index) -> Registry {
    use std::collections::{HashMap, HashSet};
    let mut ids = HashMap::new();
    for project in &index.projects {
        *ids.entry(project.id.as_str()).or_insert(0) += 1;
    }
    let revoked: HashSet<_> = index
        .revoked
        .iter()
        .filter(|r| r.sha256.is_none())
        .map(|r| r.id.as_str())
        .collect();
    let mut projects: Vec<_> = index
        .projects
        .iter()
        .filter_map(|p| {
            if !content::valid_id(&p.id)
                || ids[p.id.as_str()] != 1
                || revoked.contains(p.id.as_str())
            {
                return None;
            }
            let kind = match p.kind.as_str() {
                "mod" => ProjectKind::Mod,
                "shader" | "shaderpack" => ProjectKind::Shader,
                _ => return None,
            };
            Some(Project {
                id: p.id.clone(),
                name: p.name.clone(),
                summary: p.summary.clone(),
                description: p.description.clone(),
                screenshots: p
                    .screenshots
                    .iter()
                    .filter_map(|url| {
                        icon_url(
                            Some(url.clone()),
                            "horizon-rust.github.io",
                            "/horizon-mods/screenshots/",
                        )
                    })
                    .collect(),
                kind,
                downloads: p.downloads,
                icon_url: icon_url(
                    p.icon.clone(),
                    "horizon-rust.github.io",
                    "/horizon-mods/icons/",
                ),
                blocked: None,
                shaderpack_ids: (p.kind == "shaderpack").then(|| {
                    p.versions
                        .iter()
                        .map(|v| content::shaderpack_id(&v.file.sha256))
                        .collect()
                }),
            })
        })
        .collect();
    projects.sort_by(|a, b| b.downloads.cmp(&a.downloads).then_with(|| a.id.cmp(&b.id)));
    Registry(projects)
}

fn icon_url(url: Option<String>, host: &str, path: &str) -> Option<String> {
    let url = url?;
    let parsed = reqwest::Url::parse(&url).ok()?;
    (allowed_url(&parsed) && parsed.host_str() == Some(host) && parsed.path().starts_with(path))
        .then_some(url)
}

/// Resolve again at click time, not from stale search results.
pub async fn registry_release(id: &str, config: &crate::HorizonConfig) -> Result<Release> {
    let index = index().await?;
    let build = build::installed_build(&config.build_id).await?;
    let api: api::LaunchApi = api::query(&build.client(), "horizon-client").await?;
    let content = api
        .content
        .as_ref()
        .ok_or_else(|| error("This Horizon build does not advertise content compatibility"))?;
    select_registry(&index, id, &config.minecraft_version, content)
}

pub(crate) fn advertises_api(
    content: Option<&serde_json::Value>,
    kind: &str,
    expected: &str,
) -> bool {
    content
        .and_then(|c| c.get(kind))
        .and_then(|c| c.get("api"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|apis| apis.iter().any(|a| a.as_str() == Some(expected)))
}

fn select_registry(
    index: &Index,
    id: &str,
    game: &str,
    content: &serde_json::Value,
) -> Result<Release> {
    if index.schema != 1
        || id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
    {
        return Err(error("Unsupported registry schema or invalid project ID"));
    }
    let matches: Vec<_> = index.projects.iter().filter(|p| p.id == id).collect();
    let [project] = matches.as_slice() else {
        return Err(error("Registry project is missing or duplicated"));
    };
    let field = project.kind.as_str();
    let kind = match field {
        "mod" => content::Kind::Mod,
        "shader" => content::Kind::Shader,
        "shaderpack" => content::Kind::ShaderPack,
        _ => return Err(error("This registry content kind is not supported yet")),
    };
    let version = project
        .versions
        .iter()
        .find(|v| {
            v.game == game
                && matches!(v.side.as_deref(), None | Some("both" | "client"))
                && (if kind == content::Kind::ShaderPack {
                    // GLSL ZIPs have no Horizon manifest or versioned shader API.
                    v.api.is_empty()
                } else {
                    advertises_api(Some(content), field, &v.api)
                })
                && !index.revoked.iter().any(|r| {
                    r.id == project.id
                        && r.sha256.as_ref().is_none_or(|hash| hash == &v.file.sha256)
                })
        })
        .ok_or_else(|| {
            error("No non-revoked release compatible with this instance's Horizon API/game version")
        })?;
    let url = reqwest::Url::parse(&version.file.url).map_err(error)?;
    let parts: Vec<_> = url.path().split('/').collect();
    // /owner/repo/releases/download/tag/file (including the leading empty component).
    if url.host_str() != Some("github.com")
        || parts.len() != 7
        || parts[3..5] != ["releases", "download"]
        || parts[1..].iter().any(|p| p.is_empty())
    {
        return Err(error("Registry file is not a GitHub release asset"));
    }
    if version.file.size > 64 * 1024 * 1024 || version.file.sha256.len() != 64 {
        return Err(error("Registry file exceeds 64 MiB or has no SHA-256"));
    }
    if content::kind(Path::new(&version.file.name)).ok() != Some(kind)
        || project.key.is_some() != version.file.signature.is_some()
    {
        return Err(error("Unsupported package format or missing signature/key"));
    }
    Ok(Release {
        project: id.into(),
        version: version.version.clone(),
        file: version.file.clone(),
        key: project.key.clone(),
        api: Some(version.api.clone()),
        side: version.side.clone(),
        capabilities: version.capabilities.clone(),
        schema: content
            .get(field)
            .and_then(|c| c.get("schema"))
            .and_then(|s| s.as_u64()),
    })
}

fn verify(spec: &FileSpec, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 != spec.size {
        return Err(error("Downloaded size does not match catalog"));
    }
    let hex = |s: &str, n| {
        s.len() == n
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    };
    if spec.sha512.is_empty() && spec.sha256.is_empty() {
        return Err(error("Download has no hash"));
    }
    if !spec.sha512.is_empty()
        && (!hex(&spec.sha512, 128) || format!("{:x}", Sha512::digest(bytes)) != spec.sha512)
    {
        return Err(error("Downloaded SHA-512 does not match catalog"));
    }
    if !spec.sha256.is_empty()
        && (!hex(&spec.sha256, 64) || format!("{:x}", Sha256::digest(bytes)) != spec.sha256)
    {
        return Err(error("Downloaded SHA-256 does not match catalog"));
    }
    Ok(())
}

fn verify_signature(key: Option<&str>, signature: Option<&str>, bytes: &[u8]) -> Result<()> {
    match (key, signature) {
        (None, None) => Ok(()),
        (Some(key), Some(signature)) => {
            let key = minisign_verify::PublicKey::from_base64(key).map_err(error)?;
            let signature = minisign_verify::Signature::decode(signature).map_err(error)?;
            // Registry schema 1 allows both legacy and prehashed minisign signatures.
            key.verify(bytes, &signature, true).map_err(error)
        }
        _ => Err(error("Missing registry signature or public key")),
    }
}

fn remember_key(instance: &ql_core::Instance, release: &Release) -> Result<()> {
    use std::io::Write;
    let dir = instance
        .get_instance_path()
        .join("horizon/registry-receipts");
    std::fs::create_dir_all(&dir).map_err(error)?;
    let path = dir.join(format!("{}.json", release.project));
    #[derive(Deserialize, Serialize)]
    struct Receipt {
        key: Option<String>,
    }
    match std::fs::symlink_metadata(&path) {
        Ok(m) => {
            if !m.is_file() || m.len() > 64 * 1024 {
                return Err(error("Invalid registry key receipt"));
            }
            let old: Receipt =
                serde_json::from_slice(&std::fs::read(&path).map_err(error)?).map_err(error)?;
            if old.key.is_some() && old.key != release.key {
                return Err(error(
                    "Registry author key changed or was removed. Refusing to silently replace the previously trusted signing key.",
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(error(e)),
    }
    let mut file = tempfile::NamedTempFile::new_in(&dir).map_err(error)?;
    file.write_all(
        &serde_json::to_vec_pretty(&Receipt {
            key: release.key.clone(),
        })
        .map_err(error)?,
    )
    .map_err(error)?;
    file.persist(&path).map_err(error)?;
    Ok(())
}

pub async fn download(release: &Release, dir: &Path) -> Result<std::path::PathBuf> {
    let spec = &release.file;
    if !safe_name(&spec.name) || spec.size == 0 || spec.size > MAX_FILE {
        return Err(error("Invalid download metadata"));
    }
    let bytes = get(&spec.url, spec.size).await?;
    verify(spec, &bytes)?;
    verify_signature(release.key.as_deref(), spec.signature.as_deref(), &bytes)?;
    let path = dir.join(&spec.name);
    use std::io::Write;
    let mut pending = tempfile::NamedTempFile::new_in(dir).map_err(error)?;
    pending.write_all(&bytes).map_err(error)?;
    pending.persist_noclobber(&path).map_err(error)?;
    Ok(path)
}

pub async fn install_registry(
    id: &str,
    instance: &ql_core::Instance,
    config: &crate::HorizonConfig,
) -> Result<()> {
    let release = registry_release(id, config).await?;
    let temp = tempfile::tempdir().map_err(error)?;
    let path = download(&release, temp.path()).await?;
    let kind = content::kind(&path)?;
    validate_registry_package(&release, &path, kind)?;
    remember_key(instance, &release)?;
    content::install_as(&instance.get_dot_minecraft_path(), &path, kind)
}

fn validate_registry_package(release: &Release, path: &Path, kind: content::Kind) -> Result<()> {
    let metadata = content::inspect(path, kind)?;
    if kind == content::Kind::ShaderPack {
        return if metadata.id == content::shaderpack_id(&release.file.sha256) {
            Ok(())
        } else {
            Err(error("Shader ZIP does not match registry digest"))
        };
    }
    let api_matches = release.api.as_deref() == Some(&metadata.api);
    if metadata.id != release.project
        || metadata.version != release.version
        || !api_matches
        || (kind == content::Kind::Mod
            && (metadata.side.as_deref().unwrap_or("both")
                != release.side.as_deref().unwrap_or("both")
                || metadata
                    .capabilities
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    != release.capabilities.iter().collect()))
        || (kind == content::Kind::Shader
            && (release.schema.is_none() || metadata.schema != release.schema))
    {
        return Err(error(
            "Package identity, API, side, capabilities or schema does not match registry/game",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Index {
        serde_json::from_value(serde_json::json!({
            "schema": 1, "revoked": [], "projects": [{
                "id": "example", "name": "Example", "summary": "Test", "kind": "mod",
                "versions": [{"version": "1.0", "api": "0.2", "game": "26.3", "side": "both",
                    "file": {"name": "example.hmod", "url": "https://github.com/owner/repo/releases/download/v1/example.hmod", "size": 3, "sha256": "a".repeat(64)}}]
            }]
        })).unwrap()
    }

    #[test]
    fn native_projects_are_ignored_by_browsing_search_and_release_selection() {
        let mut index = fixture();
        index.projects.push(RegistryProject {
            id: "native_example".into(),
            name: "Native Example".into(),
            kind: "native-mod".into(),
            downloads: 100,
            ..fixture().projects.pop().unwrap()
        });
        let api = serde_json::json!({"mod": {"api": ["0.2"]}});
        let err = select_registry(&index, "native_example", "26.3", &api).unwrap_err();
        assert!(matches!(err, HorizonError::Message(message) if
            message == "This registry content kind is not supported yet"));

        let registry = registry_for_browsing(index);
        assert_eq!(registry.page("", None, 0).projects[0].id, "example");
        for (kind, count) in [
            (None, 1),
            (Some(ProjectKind::Mod), 1),
            (Some(ProjectKind::Shader), 0),
        ] {
            assert_eq!(registry.page("", kind, 0).projects.len(), count);
            assert!(registry.page("native", kind, 0).projects.is_empty());
        }
    }

    #[test]
    fn browsing_uses_download_counts_and_filters_without_losing_pagination() {
        let mut index = fixture();
        index.projects.clear();
        for i in 0..=PAGE_SIZE {
            let project = serde_json::from_value(serde_json::json!({
                "id": format!("project_{i}"), "name": format!("Project {i}"), "summary": "Night Sky",
                "kind": if i == PAGE_SIZE { "shader" } else { "mod" },
                "downloads": i, "versions": [], "description": "# Night Sky",
                "screenshots": ["https://horizon-rust.github.io/horizon-mods/screenshots/sky/1.png", "file:///invalid"],
                "icon": "https://horizon-rust.github.io/horizon-mods/icons/project.png?v=fixture"
            })).unwrap();
            index.projects.push(project);
        }
        // A popular but revoked entry must never show up in browse results.
        index.projects.push(serde_json::from_value(serde_json::json!({
            "id": "revoked", "name": "Revoked", "summary": "", "kind": "mod", "downloads": 9999, "versions": []
        })).unwrap());
        index.revoked.push(Revoked {
            id: "revoked".into(),
            sha256: None,
        });
        let registry = registry_for_browsing(index);
        let mut page = registry.page("", None, 0);
        assert_eq!(page.projects.len(), PAGE_SIZE);
        assert!(page.has_next);
        assert_eq!(page.projects[0].downloads, PAGE_SIZE as u64);
        assert!(page.projects[0].icon_url.is_some());
        assert_eq!(page.projects[0].description.as_deref(), Some("# Night Sky"));
        assert_eq!(
            page.projects[0].screenshots,
            ["https://horizon-rust.github.io/horizon-mods/screenshots/sky/1.png"]
        );
        registry.append_next(&mut page, "", None);
        assert!(!page.has_next);
        registry.append_next(&mut page, "", None);
        assert_eq!(page.projects.len(), PAGE_SIZE + 1);
        assert_eq!(page.projects[0].downloads, PAGE_SIZE as u64);
        assert_eq!(page.projects[PAGE_SIZE].id, "project_0");
        let shaders = registry.page("  SKY project  ", Some(ProjectKind::Shader), 0);
        assert_eq!(shaders.projects.len(), 1);
        assert!(!shaders.has_next);
        assert!(registry.page("does not match", None, 0).projects.is_empty());
        assert!(
            icon_url(
                Some("https://github.com/horizon-mods/icons/fake.svg".into()),
                "horizon-rust.github.io",
                "/horizon-mods/icons/"
            )
            .is_none()
        );
    }

    #[test]
    fn registry_filters_game_api_side_revocations_and_duplicates() {
        let api = serde_json::json!({"mod": {"api": ["0.2"]}});
        let mut index = fixture();
        assert!(select_registry(&index, "example", "26.3", &api).is_ok());
        assert!(select_registry(&index, "example", "1.21.1", &api).is_err());
        assert!(select_registry(&index, "example", "26.3", &serde_json::json!({})).is_err());
        index.projects[0].versions[0].side = Some("server".into());
        assert!(select_registry(&index, "example", "26.3", &api).is_err());
        index.projects[0].versions[0].side = None;
        index.revoked.push(Revoked {
            id: "example".into(),
            sha256: Some("b".repeat(64)),
        });
        assert!(select_registry(&index, "example", "26.3", &api).is_ok());
        index.revoked[0].sha256 = Some("a".repeat(64));
        assert!(select_registry(&index, "example", "26.3", &api).is_err());
        index.revoked[0].sha256 = None;
        assert!(select_registry(&index, "example", "26.3", &api).is_err());
        index.revoked.clear();
        index.projects.extend(fixture().projects);
        assert!(select_registry(&index, "example", "26.3", &api).is_err());
    }

    #[test]
    fn registry_glsl_zip_uses_game_and_digest_not_a_manifest_api() {
        use std::io::Write;
        let mut index = fixture();
        let project = &mut index.projects[0];
        project.kind = "shaderpack".into();
        let version = &mut project.versions[0];
        version.api.clear();
        let content = serde_json::json!({"shader": {"api": ["0.2"], "schema": 1}});
        assert!(select_registry(&index, "example", "26.3", &content).is_err()); // wrong extension

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("example.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file(
            "shaders/world1/final.fsh",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"void main() {}").unwrap();
        zip.finish().unwrap();
        let digest = format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap()));
        let file = &mut index.projects[0].versions[0].file;
        file.name = "example.zip".into();
        file.url = file.url.replace(".hmod", ".zip");
        file.sha256 = digest.clone();
        file.size = path.metadata().unwrap().len();
        let release = select_registry(&index, "example", "26.3", &content).unwrap();
        validate_registry_package(&release, &path, content::Kind::ShaderPack).unwrap();
        assert!(select_registry(&index, "example", "other", &content).is_err());
        index.revoked.push(Revoked {
            id: "example".into(),
            sha256: Some(digest.clone()),
        });
        assert!(select_registry(&index, "example", "26.3", &content).is_err());
        index.revoked.clear();
        let registry = registry_for_browsing(index);
        for filter in [None, Some(ProjectKind::Shader)] {
            let page = registry.page("", filter, 0);
            assert!(page.projects[0].blocked.is_none());
            assert_eq!(
                page.projects[0].shaderpack_ids,
                Some(vec![content::shaderpack_id(&digest)])
            );
        }
    }

    #[test]
    fn registry_refuses_missing_signatures_and_unsupported_formats() {
        let api = serde_json::json!({"mod": {"api": ["0.2"]}});
        let mut index = fixture();
        index.projects[0].key = Some("key".into());
        assert!(select_registry(&index, "example", "26.3", &api).is_err());
        index.projects[0].versions[0].file.signature = Some("signature".into());
        assert!(select_registry(&index, "example", "26.3", &api).is_ok());
        index.projects[0].versions[0].file.name = "example.hnmod".into();
        assert!(select_registry(&index, "example", "26.3", &api).is_err());
        index.projects[0].versions[0].file.name = "example.hmod".into();
        index.projects[0].versions[0].file.url =
            "https://github.com/owner/repo/raw/main/example.hmod".into();
        assert!(select_registry(&index, "example", "26.3", &api).is_err());
        index.schema = 2;
        assert!(select_registry(&index, "example", "26.3", &api).is_err());
    }

    #[tokio::test]
    #[ignore = "requires network and a scratch QL_DIR with an installed Nightly"]
    async fn live_registry_smoke() {
        let config = crate::HorizonConfig::new(
            "nightly".into(),
            std::env::var("QL_HORIZON_SMOKE_BUILD").unwrap(),
            "26.3".into(),
        );
        let instance = ql_core::Instance::client("registry-smoke");
        let registry = browse_registry().await.unwrap();
        let popular = registry.page("", None, 0);
        assert!(!popular.projects.is_empty());
        assert!(
            popular
                .projects
                .windows(2)
                .all(|pair| pair[0].downloads >= pair[1].downloads)
        );
        for (id, kind) in [
            ("additional_lights", content::Kind::Mod),
            ("daybreak", content::Kind::Shader),
            ("fluxxy_shader", content::Kind::ShaderPack),
        ] {
            install_registry(id, &instance, &config)
                .await
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            let game = instance.get_dot_minecraft_path();
            let package = content::list(&game)
                .unwrap()
                .into_iter()
                .find(|p| {
                    registry
                        .0
                        .iter()
                        .any(|project| project.id == id && project.matches_package(p))
                })
                .unwrap();
            assert_eq!(package.kind, kind);
            assert!(!package.enabled);
            content::remove(&game, kind, &package.filename).unwrap();
        }
    }

    #[test]
    fn verifies_author_signature_and_detects_tampering() {
        use base64::Engine;
        use ring::signature::{Ed25519KeyPair, KeyPair};
        let pair = Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap();
        let prefix = b"Ed12345678";
        let mut key = prefix.to_vec();
        key.extend_from_slice(pair.public_key().as_ref());
        let key = base64::engine::general_purpose::STANDARD.encode(key);
        let data = b"signed package";
        let sig = pair.sign(data);
        let mut encoded = prefix.to_vec();
        encoded.extend_from_slice(sig.as_ref());
        let comment = "timestamp:123";
        let mut global = sig.as_ref().to_vec();
        global.extend_from_slice(comment.as_bytes());
        let signature = format!(
            "untrusted comment: test\n{}\ntrusted comment: {comment}\n{}\n",
            base64::engine::general_purpose::STANDARD.encode(encoded),
            base64::engine::general_purpose::STANDARD.encode(pair.sign(&global).as_ref())
        );
        assert!(verify_signature(Some(&key), Some(&signature), data).is_ok());
        assert!(verify_signature(Some(&key), Some(&signature), b"tampered").is_err());
        assert!(
            verify_signature(
                Some(&key),
                Some(&signature.replace("timestamp:123", "timestamp:456")),
                data
            )
            .is_err()
        );
    }

    #[test]
    fn signatures_cannot_be_silently_ignored() {
        assert!(verify_signature(None, None, b"test").is_ok());
        assert!(verify_signature(Some("bad key"), None, b"test").is_err());
        assert!(verify_signature(None, Some("bad signature"), b"test").is_err());
        assert!(verify_signature(Some("bad key"), Some("bad signature"), b"test").is_err());
    }

    #[test]
    fn downloads_require_exact_sizes_and_hashes() {
        let mut spec = FileSpec {
            name: "a.hmod".into(),
            url: String::new(),
            size: 3,
            sha256: format!("{:x}", Sha256::digest(b"abc")),
            sha512: String::new(),
            signature: None,
        };
        assert!(verify(&spec, b"abc").is_ok());
        assert!(verify(&spec, b"xyz").is_err());
        assert!(verify(&spec, b"ab").is_err());
        spec.sha512 = format!("{:x}", Sha512::digest(b"abc"));
        assert!(verify(&spec, b"abc").is_ok());
        spec.sha512.clear();
        spec.sha256.clear();
        assert!(verify(&spec, b"abc").is_err());
    }
    #[test]
    fn rejects_path_and_host_tricks() {
        for name in ["../a", "a/b", "a\\b", "C:foo", ".", "foo\nbar", "foo."] {
            assert!(!safe_name(name));
        }
        for url in [
            "http://cdn.modrinth.com/a",
            "https://cdn.modrinth.com.evil.test/a",
            "https://user@github.com/a",
            "https://github.com:123/a",
            "file:///tmp/a",
        ] {
            assert!(!allowed_url(&reqwest::Url::parse(url).unwrap()));
        }
    }
}
