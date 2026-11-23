//! Horizon package storage. This module does not extract or execute packages.
//! Horizon checks dependencies and payloads when it loads them. Call the blocking APIs off the UI thread.
//!
//! Upstream contracts: `docs/modding/src/start/installing.md`,
//! `docs/modding/src/reference/manifest.md` and `docs/api/shaders.md` in
//! <https://github.com/horizon-rust/horizon>.

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;

use crate::HorizonError;

type Result<T> = std::result::Result<T, HorizonError>;
const MANIFEST_LIMIT: u64 = 256 * 1024;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Mod,
    Shader,
    /// Iris/OptiFine-format ZIP, including Horizon-Translator's output.
    ShaderPack,
}

impl Kind {
    pub fn directory(self) -> &'static str {
        match self {
            Self::Mod => "horizon-mods",
            Self::Shader | Self::ShaderPack => "shaderpacks",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mod => "hmod",
            Self::Shader => "hshader",
            Self::ShaderPack => "zip",
        }
    }

    pub fn is_mod(self) -> bool {
        self == Self::Mod
    }

    fn manifest(self) -> &'static str {
        match self {
            Self::Mod => "horizon-mod.toml",
            Self::Shader => "horizon-shaders.toml",
            Self::ShaderPack => "", // No Horizon manifest in a GLSL pack.
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Metadata {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api: String,
    #[serde(default)]
    pub(crate) side: Option<String>,
    #[serde(default)]
    pub(crate) capabilities: Vec<String>,
    #[serde(default)]
    pub(crate) schema: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Package {
    pub filename: String,
    pub kind: Kind,
    /// Invalid packages remain visible and removable.
    pub metadata: std::result::Result<Metadata, String>,
    pub enabled: bool,
}

impl Package {
    /// Optional artwork: missing or malformed icons never invalidate a package.
    /// Read only fixed archive entries and bound both compressed input and decoded dimensions.
    pub fn thumbnail(&self, game: &Path) -> Option<image::RgbaImage> {
        self.metadata.as_ref().ok()?;
        let path = package_path(game, self.kind, &self.filename).ok()?;
        regular_file(&path).ok()?;
        let mut archive = zip::ZipArchive::new(File::open(path).ok()?).ok()?;
        let name = if self.kind == Kind::ShaderPack {
            "pack.png"
        } else {
            "icon.png"
        };
        let entry = archive.by_name(name).ok()?;
        const LIMIT: u64 = 1024 * 1024;
        if entry.size() > LIMIT || entry.compressed_size() > LIMIT {
            return None;
        }
        let mut bytes = Vec::new();
        entry.take(LIMIT + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > LIMIT {
            return None;
        }
        let mut reader =
            image::io::Reader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
        let mut limits = image::io::Limits::default();
        limits.max_image_width = Some(1024);
        limits.max_image_height = Some(1024);
        limits.max_alloc = Some(16 * 1024 * 1024);
        reader.limits(limits);
        Some(reader.decode().ok()?.thumbnail(48, 48).into_rgba8())
    }
}

fn error(message: impl std::fmt::Display) -> HorizonError {
    HorizonError::Message(message.to_string())
}

fn io<T>(result: std::io::Result<T>, path: &Path) -> Result<T> {
    result.map_err(|e| error(format!("{}: {e}", path.display())))
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

pub(crate) fn shaderpack_id(sha256: &str) -> String {
    format!("shaderpack_{sha256}")
}

pub(crate) fn kind(path: &Path) -> Result<Kind> {
    match path.extension().and_then(|s| s.to_str()) {
        Some("hmod") => Ok(Kind::Mod),
        Some("hshader") => Ok(Kind::Shader),
        Some("zip") => Ok(Kind::ShaderPack),
        _ => Err(error(
            "Expected .hmod, .hshader or a shaders-folder ZIP. For a Java mod, choose its Horizon port (.hmod) instead.",
        )),
    }
}

fn regular_file(path: &Path) -> Result<()> {
    if !io(fs::symlink_metadata(path), path)?.is_file() {
        return Err(error(format!("Not a regular file: {}", path.display())));
    }
    Ok(())
}

/// Reads bounded metadata; never extracts or executes archive contents.
pub(crate) fn inspect(path: &Path, kind: Kind) -> Result<Metadata> {
    regular_file(path)?;
    let mut archive = zip::ZipArchive::new(io(File::open(path), path)?).map_err(error)?;
    let mut seen = HashSet::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(error)?;
        let raw_name = entry.name().map_err(|e| error(e.to_string()))?;
        let name = raw_name.trim_end_matches('/');
        if name.is_empty()
            || name.contains(['\\', ':'])
            || name.split('/').any(|s| matches!(s, "" | "." | ".."))
            || !seen.insert(name.to_owned())
            || entry.unix_mode().is_some_and(|m| {
                let ty = m & 0o170000;
                ty != 0 && ty != 0o100000 && ty != 0o040000
            })
        {
            return Err(error(format!("Unsafe or duplicate archive entry: {name}")));
        }
    }
    if kind == Kind::ShaderPack {
        if !seen.iter().any(|name| {
            name.starts_with("shaders/") && (name.ends_with(".fsh") || name.ends_with(".vsh"))
        }) {
            return Err(error("ZIP has no shaders/*.fsh or shaders/*.vsh programs"));
        }
        use sha2::{Digest, Sha256};
        let mut file = io(File::open(path), path)?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = io(file.read(&mut buffer), path)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        return Ok(Metadata {
            id: shaderpack_id(&format!("{:x}", hash.finalize())),
            name: "GLSL shader pack".into(),
            version: String::new(),
            api: "Iris/OptiFine format".into(),
            side: None,
            capabilities: Vec::new(),
            schema: None,
        });
    }
    let entry = archive.by_name(kind.manifest()).map_err(error)?;
    let mut text = String::new();
    io(
        entry.take(MANIFEST_LIMIT + 1).read_to_string(&mut text),
        path,
    )?;
    if text.len() as u64 > MANIFEST_LIMIT {
        return Err(error("Package manifest is too large"));
    }
    let manifest: toml::Table = toml::from_str(&text).map_err(error)?;
    if kind == Kind::Mod && manifest.contains_key("native") {
        return Err(HorizonError::UnsupportedNativeMod);
    }
    let metadata: Metadata = manifest.try_into().map_err(error)?;
    if !valid_id(&metadata.id)
        || metadata.name.trim().is_empty()
        || metadata.version.trim().is_empty()
        || metadata.api.trim().is_empty()
    {
        return Err(error("Package needs a valid id, name, version and API"));
    }
    Ok(metadata)
}

fn content_dir(game_dir: &Path, kind: Kind) -> Result<PathBuf> {
    let dir = game_dir.join(kind.directory());
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if !metadata.is_dir() => {
            Err(error(format!("Not a regular directory: {}", dir.display())))
        }
        Ok(_) => Ok(dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(dir),
        Err(e) => Err(error(format!("{}: {e}", dir.display()))),
    }
}

fn read_settings(game_dir: &Path) -> Result<String> {
    let path = game_dir.join("horizon-mods.txt");
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(error(format!("{}: {e}", path.display()))),
        Ok(_) => {
            regular_file(&path)?;
            io(fs::read_to_string(&path), &path)
        }
    }
}

fn enabled(settings: &str, id: &str) -> bool {
    let key = format!("mod.{id}");
    settings
        .lines()
        .filter_map(|line| line.split_once(':'))
        .rfind(|(k, _)| k.trim() == key)
        .is_some_and(|(_, value)| value.trim() == "true")
}

/// Missing content directories are an empty list, not an error.
pub fn list(game_dir: &Path) -> Result<Vec<Package>> {
    let settings = read_settings(game_dir)?;
    let mut packages = Vec::new();
    for kind in [Kind::Mod, Kind::Shader, Kind::ShaderPack] {
        let dir = content_dir(game_dir, kind)?;
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(error(format!("{}: {e}", dir.display()))),
        };
        for entry in entries {
            let entry = io(entry, &dir)?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some(kind.extension()) {
                continue;
            }
            let Some(filename) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let metadata = inspect(&path, kind).map_err(|e| e.to_string());
            let enabled =
                kind.is_mod() && metadata.as_ref().is_ok_and(|m| enabled(&settings, &m.id));
            packages.push(Package {
                filename,
                kind,
                metadata,
                enabled,
            });
        }
    }
    packages.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok(packages)
}

/// Copy through a temporary file and publish without overwriting anything.
/// Mods retain Horizon's opt-in default (off unless their ID was enabled before).
pub fn install(game_dir: &Path, source: &Path) -> Result<()> {
    install_as(game_dir, source, kind(source)?)
}

pub(crate) fn install_as(game_dir: &Path, source: &Path, kind: Kind) -> Result<()> {
    regular_file(source)?;
    let filename = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| error("Package needs a UTF-8 filename"))?;
    // Use the same name checks as toggle/remove, before publishing anything.
    let destination = package_path(game_dir, kind, filename)?;
    let dir = destination
        .parent()
        .expect("package path includes a content directory");
    io(fs::create_dir_all(dir), dir)?;
    let mut pending = io(tempfile::NamedTempFile::new_in(dir), dir)?;
    let mut input = io(File::open(source), source)?;
    io(std::io::copy(&mut input, &mut pending), source)?;
    io(pending.flush(), dir)?;
    let metadata = inspect(pending.path(), kind)?;
    for installed in list(game_dir)? {
        if installed.kind == kind
            && installed
                .metadata
                .as_ref()
                .is_ok_and(|m| m.id == metadata.id)
        {
            return Err(error(format!(
                "{} is already installed; remove the old package first",
                metadata.id
            )));
        }
    }
    pending
        .persist_noclobber(&destination)
        .map_err(|e| error(format!("{}: {e}", destination.display())))?;
    Ok(())
}

fn package_path(game_dir: &Path, kind: Kind, filename: &str) -> Result<PathBuf> {
    let path = Path::new(filename);
    if filename.contains(['/', '\\', ':'])
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || path.components().count() != 1
        || path.extension().and_then(|s| s.to_str()) != Some(kind.extension())
    {
        return Err(error("Invalid package filename"));
    }
    Ok(content_dir(game_dir, kind)?.join(path))
}

pub fn remove(game_dir: &Path, kind: Kind, filename: &str) -> Result<()> {
    let path = package_path(game_dir, kind, filename)?;
    // remove_file unlinks a symlink, never its target; directories cannot be removed.
    io(fs::remove_file(&path), &path)
}

pub fn set_enabled(game_dir: &Path, kind: Kind, filename: &str, value: bool) -> Result<()> {
    if !kind.is_mod() {
        return Err(error("Only mods can be enabled here"));
    }
    let metadata = inspect(&package_path(game_dir, kind, filename)?, kind)?;
    let settings = read_settings(game_dir)?;
    let key = format!("mod.{}", metadata.id);
    let mut output = String::new();
    for line in settings.lines() {
        if line.split_once(':').is_none_or(|(k, _)| k.trim() != key) {
            output.push_str(line);
            output.push('\n');
        }
    }
    output.push_str(&format!("{key}:{value}\n"));
    let path = game_dir.join("horizon-mods.txt");
    let mut pending = io(tempfile::NamedTempFile::new_in(game_dir), game_dir)?;
    io(pending.write_all(output.as_bytes()), &path)?;
    io(pending.flush(), &path)?;
    pending
        .persist(&path)
        .map_err(|e| error(format!("{}: {e}", path.display())))?;
    Ok(())
}
