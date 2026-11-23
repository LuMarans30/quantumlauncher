use super::*;
use zip::{ZipWriter, write::SimpleFileOptions};

fn archive(path: &Path, entries: &[(&str, &str)]) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    for (name, text) in entries {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

fn manifest(id: &str) -> String {
    format!("id = {id:?}\nname = 'Example'\nversion = '1.0.0'\napi = '0.2'\n")
}

#[test]
fn installed_thumbnail_uses_bundled_png_without_requiring_a_registry() {
    let game = tempfile::tempdir().unwrap();
    let dir = game.path().join(Kind::Mod.directory());
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("local.hmod");
    let write_icon = |bytes: &[u8]| {
        let mut zip = ZipWriter::new(File::create(&path).unwrap());
        zip.start_file("horizon-mod.toml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(manifest("local").as_bytes()).unwrap();
        zip.start_file("icon.png", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
        zip.finish().unwrap();
    };
    let png = |width, height| {
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(width, height)
            .write_to(&mut output, image::ImageOutputFormat::Png)
            .unwrap();
        output.into_inner()
    };
    write_icon(&png(96, 64));
    let package = list(game.path()).unwrap().pop().unwrap();
    assert_eq!(
        package.thumbnail(game.path()).unwrap().dimensions(),
        (48, 32)
    );
    for bad in [
        b"not an image".to_vec(),
        png(1025, 1),
        vec![0; 1024 * 1024 + 1],
    ] {
        write_icon(&bad);
        assert!(package.thumbnail(game.path()).is_none());
        assert!(list(game.path()).unwrap()[0].metadata.is_ok());
    }
}

#[test]
fn install_list_toggle_and_remove_without_touching_other_content() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    assert!(list(&game).unwrap().is_empty());
    let source = root.path().join("example.hmod");
    archive(
        &source,
        &[
            ("horizon-mod.toml", &manifest("example")),
            ("data/example/test.json", "{}"),
        ],
    );
    install(&game, &source).unwrap();
    assert!(source.exists(), "import never deletes the source");
    assert_eq!(
        fs::read(&source).unwrap(),
        fs::read(game.join("horizon-mods/example.hmod")).unwrap()
    );
    assert!(
        !game.join("mods").exists(),
        "never use the Java mods folder"
    );
    assert!(!list(&game).unwrap()[0].enabled, "new IDs are opt-in");

    let settings = game.join("horizon-mods.txt");
    fs::write(&settings, "# Keep this comment\nmod.other:true\nwasm.memory_cap_mib:2048\nmod.example:false\nmod.example:false\nunknown:value\n").unwrap();
    set_enabled(&game, Kind::Mod, "example.hmod", true).unwrap();
    let text = fs::read_to_string(&settings).unwrap();
    assert_eq!(text.matches("mod.example:").count(), 1);
    assert!(text.contains("mod.other:true\nwasm.memory_cap_mib:2048\nunknown:value\n"));
    assert!(text.starts_with("# Keep this comment\n"));
    assert!(list(&game).unwrap()[0].enabled);
    set_enabled(&game, Kind::Mod, "example.hmod", false).unwrap();
    assert!(!list(&game).unwrap()[0].enabled);

    let shader = root.path().join("lighting.hshader");
    archive(
        &shader,
        &[
            (
                "horizon-shaders.toml",
                &format!(
                    "{}schema = 1\n[sky]\nfile = 'sky.wgsl'\n",
                    manifest("lighting")
                ),
            ),
            ("sky.wgsl", "// example"),
        ],
    );
    install(&game, &shader).unwrap();
    assert!(game.join("shaderpacks/lighting.hshader").exists());
    assert_eq!(list(&game).unwrap().len(), 2);
    remove(&game, Kind::Mod, "example.hmod").unwrap();
    assert!(game.join("shaderpacks/lighting.hshader").exists());
    assert!(
        fs::read_to_string(settings)
            .unwrap()
            .contains("mod.other:true")
    );
    remove(&game, Kind::Shader, "lighting.hshader").unwrap();
    assert!(list(&game).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn refuses_imports_that_cannot_be_managed_by_filename() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    let source = root.path().join("bad:name.hmod");
    archive(&source, &[("horizon-mod.toml", &manifest("example"))]);
    assert!(install(&game, &source).is_err());
    assert!(list(&game).unwrap().is_empty());
}

#[test]
fn local_resource_pack_lifecycle_and_zip_detection() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    let path = root.path().join("custom.zip");
    let meta = ("pack.mcmeta", r#"{"pack":{"min_format":[97,1]}}"#);
    for entries in [
        vec![meta, ("data/example/test", "")],
        vec![("pack.mcmeta", "{}")],
        vec![meta, ("../outside", "")],
    ] {
        archive(&path, &entries);
        assert!(install(&game, &path).is_err());
        assert!(list(&game).unwrap().is_empty());
    }
    archive(&path, &[meta, ("assets/example/textures/test.png", "")]);
    install(&game, &path).unwrap();
    assert!(game.join("resourcepacks/custom.zip").exists());
    let renamed = root.path().join("copy.zip");
    fs::copy(&path, &renamed).unwrap();
    assert!(install(&game, &renamed).is_err());
    let package = list(&game).unwrap().pop().unwrap();
    assert_eq!(package.kind, Kind::ResourcePack);
    assert_eq!(package.metadata.unwrap().api, "97.1");
    assert!(set_enabled(&game, Kind::ResourcePack, "custom.zip", true).is_err());
    // A shader ZIP may also have pack.mcmeta. Same filename, different destination.
    archive(&path, &[meta, ("shaders/final.fsh", "void main() {}")]);
    install(&game, &path).unwrap();
    remove(&game, Kind::ResourcePack, "custom.zip").unwrap();
    let remaining = list(&game).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].kind, Kind::ShaderPack);
}

#[test]
fn refuses_replacements_duplicate_ids_and_foreign_formats() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    let first = root.path().join("one.hmod");
    archive(&first, &[("horizon-mod.toml", &manifest("one"))]);
    install(&game, &first).unwrap();
    let original = fs::read(game.join("horizon-mods/one.hmod")).unwrap();
    let second = root.path().join("renamed.hmod");
    archive(&second, &[("horizon-mod.toml", &manifest("one"))]);
    assert!(
        install(&game, &second)
            .unwrap_err()
            .to_string()
            .contains("already installed")
    );
    archive(&first, &[("horizon-mod.toml", &manifest("two"))]);
    assert!(
        install(&game, &first).is_err(),
        "same filename cannot overwrite even with another ID"
    );
    assert_eq!(
        original,
        fs::read(game.join("horizon-mods/one.hmod")).unwrap()
    );
    for extension in ["jar", "zip", "mrpack", "hnmod"] {
        let path = root.path().join(format!("test.{extension}"));
        fs::write(&path, "not supported").unwrap();
        assert!(install(&game, &path).is_err());
    }
    assert_eq!(
        fs::read_dir(game.join("horizon-mods")).unwrap().count(),
        1,
        "failed imports leave no temporary files"
    );
}

#[test]
fn refuses_native_hmod_with_an_explanation() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    let source = root.path().join("native.hmod");
    archive(
        &source,
        &[(
            "horizon-mod.toml",
            &format!("{}[native]\ngame = '26.3'\n", manifest("one")),
        )],
    );
    let error = install(&game, &source).unwrap_err();
    assert!(matches!(error, HorizonError::UnsupportedNativeMod));
    assert!(error.to_string().contains("content and WebAssembly"));
    assert!(list(&game).unwrap().is_empty());
    assert!(source.is_file());
}

#[test]
fn rejects_bad_archives_and_unsafe_names() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    let source = root.path().join("bad.hmod");
    fs::write(&source, "not a zip").unwrap();
    assert!(install(&game, &source).is_err());
    archive(&source, &[("nested/horizon-mod.toml", &manifest("one"))]);
    assert!(install(&game, &source).is_err());
    archive(&source, &[("horizon-mod.toml", "id = !!!")]);
    assert!(install(&game, &source).is_err());
    for id in ["", "../one", "has space", "a\nmod.other:true", "UpperCase"] {
        archive(&source, &[("horizon-mod.toml", &manifest(id))]);
        assert!(install(&game, &source).is_err(), "{id}");
    }
    for name in [
        "../outside",
        "/absolute",
        "C:/drive",
        "a\\b",
        "a/./b",
        "a//b",
    ] {
        archive(
            &source,
            &[("horizon-mod.toml", &manifest("one")), (name, "payload")],
        );
        assert!(install(&game, &source).is_err(), "{name}");
    }
    let oversized = format!(
        "{}#{}",
        manifest("one"),
        "x".repeat(MANIFEST_LIMIT as usize)
    );
    archive(&source, &[("horizon-mod.toml", &oversized)]);
    assert!(install(&game, &source).is_err());
    for filename in [
        "../bad.hmod",
        "/bad.hmod",
        "a/b.hmod",
        "a\\b.hmod",
        "C:bad.hmod",
        "bad.jar",
        "",
    ] {
        assert!(remove(&game, Kind::Mod, filename).is_err());
        assert!(set_enabled(&game, Kind::Mod, filename, true).is_err());
    }
}

#[test]
fn wasm_metadata_is_supported_and_reinstall_retains_enablement() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    let source = root.path().join("wasm.hmod");
    // Payload/API validation belongs to Horizon, not this metadata reader.
    archive(
        &source,
        &[
            (
                "horizon-mod.toml",
                &format!("{}[wasm]\ntoolchain = 'test'\n", manifest("wasm_example")),
            ),
            ("mod.wasm", "\0asm"),
        ],
    );
    install(&game, &source).unwrap();
    set_enabled(&game, Kind::Mod, "wasm.hmod", true).unwrap();
    remove(&game, Kind::Mod, "wasm.hmod").unwrap();
    install(&game, &source).unwrap();
    assert!(list(&game).unwrap()[0].enabled);
}

#[test]
fn broken_files_are_visible_and_removable() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path();
    fs::create_dir(game.join("horizon-mods")).unwrap();
    fs::write(game.join("horizon-mods/broken.hmod"), b"broken").unwrap();
    let packages = list(game).unwrap();
    assert_eq!(packages.len(), 1);
    assert!(packages[0].metadata.is_err());
    assert!(!packages[0].enabled);
    assert!(set_enabled(game, Kind::Mod, "broken.hmod", true).is_err());
    remove(game, Kind::Mod, "broken.hmod").unwrap();
}

#[cfg(unix)]
#[test]
fn refuses_symlink_directories_packages_and_settings() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    let outside = root.path().join("outside");
    fs::create_dir(&game).unwrap();
    fs::create_dir(&outside).unwrap();
    let source = root.path().join("one.hmod");
    archive(&source, &[("horizon-mod.toml", &manifest("one"))]);
    symlink(&outside, game.join("horizon-mods")).unwrap();
    assert!(install(&game, &source).is_err());
    assert!(list(&game).is_err());
    assert!(remove(&game, Kind::Mod, "one.hmod").is_err());
    fs::remove_file(game.join("horizon-mods")).unwrap();
    fs::create_dir(game.join("horizon-mods")).unwrap();
    symlink(&source, game.join("horizon-mods/one.hmod")).unwrap();
    assert!(list(&game).unwrap()[0].metadata.is_err());
    assert!(set_enabled(&game, Kind::Mod, "one.hmod", true).is_err());
    remove(&game, Kind::Mod, "one.hmod").unwrap();
    assert!(source.exists());
    install(&game, &source).unwrap();
    let settings = outside.join("settings");
    fs::write(&settings, "untouched").unwrap();
    symlink(&settings, game.join("horizon-mods.txt")).unwrap();
    assert!(set_enabled(&game, Kind::Mod, "one.hmod", true).is_err());
    assert_eq!(fs::read_to_string(settings).unwrap(), "untouched");
}
