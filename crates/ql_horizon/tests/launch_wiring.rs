//! Wires a fake Horizon build through the launcher and checks that the
//! game is started exactly as Horizon's Launch API says it must be.
//!
//! The "build" is a shell script, so this runs on any Unix without
//! downloading or starting the real game.

#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use ql_core::{Instance, InstanceConfigJson, InstanceKind, json::instance_config::VersionInfo};
use ql_horizon::{HorizonConfig, HorizonError, Session};
use sha1::{Digest, Sha1};

const BUILD_ID: &str = "nightly-test";
const BUILD_ID_NO_HOME: &str = "nightly-no-home";
const BUILD_ID_PREPARE: &str = "nightly-prepare";

#[tokio::test]
async fn launch_follows_the_launch_api_contract() {
    let root = sandbox();
    let out = root.join("out");
    fs::create_dir_all(&out).unwrap();
    assert!(
        ql_horizon::config_for("missing-instance")
            .await
            .unwrap()
            .is_none()
    );
    let invalid = setup_instance(&root, "invalid-config").await;
    fs::create_dir(invalid.get_instance_path().join("config.json")).unwrap();
    assert!(ql_horizon::config_for("invalid-config").await.is_err());

    let build_dir = root.join("horizon/builds").join(BUILD_ID);
    fs::create_dir_all(&build_dir).unwrap();
    // A complete install: the marker (written last by the installer) and
    // the Minecraft pins the preparer is driven by.
    fs::write(
        build_dir.join("build.json"),
        r#"{"tag": "nightly", "asset": "fake.tar.gz", "sha256": "aaaaaaaa"}"#,
    )
    .unwrap();
    fs::write(
        build_dir.join("research-lock.json"),
        r#"{"minecraft": {"version": "26.3",
            "manifest_sha1": "11f1aefe473eefba0af62e3286c33b692807b2eb"}}"#,
    )
    .unwrap();

    // Answers `--launch-api`, and otherwise records the arguments and the
    // session JSON it is handed, then exits.
    write_script(
        &build_dir.join("horizon-client"),
        &format!(
            "#!/bin/sh\n\
             if [ \"$1\" = --launch-api ]; then\n\
               echo '{{\"launch_api\":\"0.1.0\",\"options\":[\n\
                 {{\"name\":\"--resources\",\"value\":\"DIR\"}},\n\
                 {{\"name\":\"--game-dir\",\"value\":\"DIR\"}},\n\
                 {{\"name\":\"--home\",\"value\":\"DIR\"}},\n\
                 {{\"name\":\"--width\",\"value\":\"N\"}},\n\
                 {{\"name\":\"--height\",\"value\":\"N\"}},\n\
                 {{\"name\":\"--fullscreen\"}},\n\
                 {{\"name\":\"--session-stdin\"}}],\"content\":{{}}}}'\n\
               exit 0\n\
             fi\n\
             printf '%s\\n' \"$@\" > '{args}'\ncat > '{session}'\n",
            args = out.join("args.txt").display(),
            session = out.join("session.json").display(),
        ),
    );

    // Claims its resources are ready, and answers `--launch-api`.
    write_script(
        &build_dir.join("horizon-prepare"),
        "#!/bin/sh\ncase \"$1\" in\n  --check) exit 0 ;;\n  --launch-api) \
         echo '{\"launch_api\":\"0.1.0\",\"minecraft\":\"26.3\",\"java_major\":25,\"options\":[]}' ;;\nesac\n",
    );

    fs::create_dir_all(root.join("horizon/prepared").join(BUILD_ID)).unwrap();

    // ---- signed in ----
    let instance = setup_instance(&root, "demo").await;
    let instance_config = config(&instance, BUILD_ID).await;
    let err = ql_horizon::create_instance("demo".to_owned(), ql_horizon::Reporter::Nobody)
        .await
        .expect_err("an existing instance must not be overwritten");
    assert!(matches!(err, HorizonError::InstanceAlreadyExists(_)));
    assert!(instance.get_instance_path().join("config.json").is_file());

    let process = ql_horizon::launch(
        "demo",
        &instance_config,
        instance_config.horizon.as_ref().unwrap(),
        Some(Session::new(
            "Alex".to_owned(),
            "069a79f444e94726a5befca90e38aaf5".to_owned(),
            "session-token".to_owned(),
        )),
        "Alex",
        None,
        None,
    )
    .await
    .expect("launch succeeds");
    wait(&process).await;
    // A copied Java instance can retain an irrelevant or broken details.json.
    fs::write(
        instance.get_instance_path().join("details.json"),
        "not JSON",
    )
    .unwrap();
    process.read_logs(Vec::new(), None).await.unwrap().unwrap();

    let args = fs::read_to_string(out.join("args.txt")).unwrap();
    assert!(
        args.contains(&format!(
            "--resources\n{}\n",
            root.join("horizon/prepared").join(BUILD_ID).display()
        )),
        "prepared resources are passed:\n{args}"
    );
    assert!(
        args.contains(&format!(
            "--game-dir\n{}\n",
            instance.get_dot_minecraft_path().display()
        )),
        "the game folder is the instance's:\n{args}"
    );
    assert!(args.contains("--session-stdin"), "{args}");
    assert!(
        args.contains("--fullscreen"),
        "extra game arguments are passed through:\n{args}"
    );
    assert!(
        !args.contains("--name"),
        "a signed-in launch must not pass an offline name:\n{args}"
    );
    assert!(
        !args.contains("session-token"),
        "the session must never be on the command line:\n{args}"
    );

    let session: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(out.join("session.json")).unwrap()).unwrap();
    assert_eq!(session["name"], "Alex");
    assert_eq!(session["uuid"], "069a79f444e94726a5befca90e38aaf5");
    assert_eq!(session["access_token"], "session-token");
    assert_eq!(session["user_type"], "msa");

    // ---- first launch prepares (fake files, no Mojang network) ----
    let prep_build = root.join("horizon/builds").join(BUILD_ID_PREPARE);
    fs::create_dir_all(&prep_build).unwrap();
    fs::write(
        prep_build.join("build.json"),
        r#"{"tag": "nightly", "asset": "fake.tar.gz", "sha256": "cccc"}"#,
    )
    .unwrap();

    let mc = root.join("horizon/minecraft/26.3");
    fs::create_dir_all(mc.join("versions/26.3")).unwrap();
    fs::create_dir_all(mc.join("assets/indexes")).unwrap();
    let index_text = r#"{"objects":{}}"#;
    fs::write(mc.join("assets/indexes/test-index.json"), index_text).unwrap();
    let index_sha = sha1_bytes(index_text.as_bytes());

    let client_bytes = b"fake client jar";
    fs::write(mc.join("versions/26.3/26.3.jar"), client_bytes).unwrap();
    let client_sha = sha1_bytes(client_bytes);
    let server_bytes = b"fake server jar";
    fs::write(mc.join("server.jar"), server_bytes).unwrap();
    let server_sha = sha1_bytes(server_bytes);

    let version_json = serde_json::json!({
        "assetIndex": {
            "id": "test-index",
            "sha1": index_sha,
            "size": index_text.len(),
            "totalSize": index_text.len(),
            "url": "http://127.0.0.1:9/index.json"
        },
        "assets": "test",
        "downloads": {
            "client": {
                "sha1": client_sha,
                "size": client_bytes.len(),
                "url": "http://127.0.0.1:9/client.jar"
            },
            "server": {
                "sha1": server_sha,
                "size": server_bytes.len(),
                "url": "http://127.0.0.1:9/server.jar"
            }
        },
        "id": "26.3",
        "libraries": [],
        "mainClass": "test",
        "releaseTime": "2026-01-01T00:00:00+00:00",
        "time": "2026-01-01T00:00:00+00:00",
        "type": "release"
    })
    .to_string();
    let version_path = mc.join("versions/26.3/26.3.json");
    fs::write(&version_path, &version_json).unwrap();
    let manifest_sha = sha1_bytes(version_json.as_bytes());
    fs::write(
        prep_build.join("research-lock.json"),
        serde_json::json!({
            "minecraft": {
                "version": "26.3",
                "manifest_sha1": manifest_sha,
                "client_sha1": client_sha,
                "server_sha1": server_sha
            }
        })
        .to_string(),
    )
    .unwrap();

    write_script(
        &prep_build.join("horizon-client"),
        &format!(
            "#!/bin/sh\n\
             if [ \"$1\" = --launch-api ]; then\n\
               echo '{{\"launch_api\":\"0.1.0\",\"options\":[\n\
                 {{\"name\":\"--resources\",\"value\":\"DIR\"}},\n\
                 {{\"name\":\"--game-dir\",\"value\":\"DIR\"}},\n\
                 {{\"name\":\"--home\",\"value\":\"DIR\"}},\n\
                 {{\"name\":\"--width\",\"value\":\"N\"}},\n\
                 {{\"name\":\"--height\",\"value\":\"N\"}},\n\
                 {{\"name\":\"--fullscreen\"}},\n\
                 {{\"name\":\"--session-stdin\"}}],\"content\":{{}}}}'\n\
               exit 0\n\
             fi\n\
             printf '%s\\n' \"$@\" > '{args}'\ncat > /dev/null\n",
            args = out.join("prepare-client-args.txt").display(),
        ),
    );

    // A Java that exists, so the launch does not try to install one.
    let java = root.join("java_installs/java_25/bin/java");
    fs::create_dir_all(java.parent().unwrap()).unwrap();
    write_script(&java, "#!/bin/sh\nexit 0\n");

    // `--check` reports gaps until `--prepare` runs; the progress lines
    // prove the launch path has a real sink.
    let prepare_state = out.join("prepare-state");
    let prepare_args = out.join("prepare-args.txt");
    write_script(
        &prep_build.join("horizon-prepare"),
        &format!(
            "#!/bin/sh\n\
             state='{state}'\n\
             args='{args}'\n\
             case \"$1\" in\n\
               --check)\n\
                 if [ -f \"$state\" ]; then exit 0; else echo 'missing resources'; exit 1; fi ;;\n\
               --launch-api)\n\
                 echo '{{\"launch_api\":\"0.1.0\",\"minecraft\":\"26.3\",\"java_major\":25,\"options\":[\n\
                   {{\"name\":\"--prepare\"}},{{\"name\":\"--check\"}},\n\
                   {{\"name\":\"--server-jar\"}},{{\"name\":\"--version-json\"}},\n\
                   {{\"name\":\"--client-jar\"}},{{\"name\":\"--libraries-dir\"}},\n\
                   {{\"name\":\"--assets-dir\"}},{{\"name\":\"--output\"}},\n\
                   {{\"name\":\"--complete\"}},{{\"name\":\"--progress\"}},\n\
                   {{\"name\":\"--java-home\"}}]}}' ;;\n\
               --prepare)\n\
                 printf '%s\\n' \"$@\" > \"$args\"\n\
                 echo '{{\"fraction\":0.25}}'\n\
                 echo '{{\"message\":\"halfway\"}}'\n\
                 echo '{{\"fraction\":1.0}}'\n\
                 touch \"$state\" ;;\n\
             esac\n\
             exit 0\n",
            state = prepare_state.display(),
            args = prepare_args.display(),
        ),
    );

    let prep_instance = setup_instance(&root, "demo-prepare").await;
    let prep_config = config(&prep_instance, BUILD_ID_PREPARE).await;

    // The default offline gate runs before the fake preparer is touched.
    if !ql_horizon::ALLOW_OFFLINE {
        let err = ql_horizon::launch(
            "demo-prepare",
            &prep_config,
            prep_config.horizon.as_ref().unwrap(),
            None,
            "Steve",
            None,
            None,
        )
        .await
        .expect_err("offline is refused before preparation");
        assert!(matches!(err, HorizonError::OfflineDisabled), "{err:?}");
        assert!(
            !prepare_state.exists() && !prepare_args.exists(),
            "offline launch must not run --check or --prepare"
        );
    }

    let (progress_sender, progress_receiver) = std::sync::mpsc::channel();
    let process = ql_horizon::launch(
        "demo-prepare",
        &prep_config,
        prep_config.horizon.as_ref().unwrap(),
        Some(Session::new(
            "Alex".to_owned(),
            "069a79f444e94726a5befca90e38aaf5".to_owned(),
            "session-token".to_owned(),
        )),
        "Alex",
        Some(&progress_sender),
        None,
    )
    .await
    .expect("signed-in first launch prepares and starts");
    wait(&process).await;

    let args = fs::read_to_string(&prepare_args).expect("horizon-prepare --prepare ran");
    assert!(args.contains("--prepare"), "{args}");
    assert!(
        args.contains("--java-home"),
        "the managed Java is passed to the preparer:\n{args}"
    );
    let messages: Vec<_> = progress_receiver
        .try_iter()
        .filter_map(|progress| progress.message)
        .collect();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("Preparing Horizon")),
        "launch reports preparation progress: {messages:?}"
    );

    // ---- a missing Minecraft file fails in the download step, before prepare ----
    fs::remove_file(&prepare_state).unwrap();
    fs::remove_file(&prepare_args).unwrap();
    fs::remove_file(mc.join("versions/26.3/26.3.jar")).unwrap();
    let err = ql_horizon::launch(
        "demo-prepare",
        &prep_config,
        prep_config.horizon.as_ref().unwrap(),
        Some(Session::new(
            "Alex".to_owned(),
            "069a79f444e94726a5befca90e38aaf5".to_owned(),
            "session-token".to_owned(),
        )),
        "Alex",
        None,
        None,
    )
    .await
    .expect_err("the missing client jar must fail the download");
    assert!(
        matches!(err, HorizonError::Request(_)),
        "expected a download error, got: {err:?}"
    );
    assert!(
        !prepare_args.exists(),
        "downloads finish before horizon-prepare is invoked"
    );

    // ---- incompatible preparers fail before retrying any Minecraft downloads ----
    write_script(
        &prep_build.join("horizon-prepare"),
        "#!/bin/sh\ncase \"$1\" in\n  --check) exit 1 ;;\n  --launch-api) echo '{\"launch_api\":\"test\",\"minecraft\":\"26.3\",\"java_major\":25,\"options\":[]}' ;;\nesac\n",
    );
    let installed = ql_horizon::build::installed_build(BUILD_ID_PREPARE)
        .await
        .unwrap();
    let error = ql_horizon::prepare::ensure_prepared(&installed, ql_horizon::Reporter::Nobody)
        .await
        .unwrap_err();
    assert!(
        matches!(error, HorizonError::UnsupportedOption { option, .. } if option == "--prepare")
    );
    assert!(!prepare_args.exists());

    // ---- the public API respects the locally configured offline policy ----
    let instance_config = config(&instance, BUILD_ID).await;
    let result = ql_horizon::launch(
        "demo",
        &instance_config,
        instance_config.horizon.as_ref().unwrap(),
        None,
        "Steve",
        None,
        None,
    )
    .await;
    if ql_horizon::ALLOW_OFFLINE {
        wait(&result.expect("offline play is enabled")).await;
        let args = fs::read_to_string(out.join("args.txt")).unwrap();
        assert!(args.contains("--name\nSteve\n"), "{args}");
        assert!(!args.contains("--session-stdin"), "{args}");
    } else {
        assert!(matches!(result, Err(HorizonError::OfflineDisabled)));
    }

    // The gate runs before build lookup: a bogus id still says offline.
    let bogus = config(&instance, "nightly-not-installed").await;
    let err = ql_horizon::launch(
        "demo",
        &bogus,
        bogus.horizon.as_ref().unwrap(),
        None,
        "Steve",
        None,
        None,
    )
    .await
    .expect_err("offline is refused before looking up the build");
    if ql_horizon::ALLOW_OFFLINE {
        assert!(matches!(err, HorizonError::BuildMissing(_)), "{err:?}");
    } else {
        assert!(matches!(err, HorizonError::OfflineDisabled), "{err:?}");
    }

    // And an incomplete session is refused before build lookup too.
    let err = ql_horizon::launch(
        "demo",
        &bogus,
        bogus.horizon.as_ref().unwrap(),
        Some(Session::new(
            "Alex".to_owned(),
            "069a79f444e94726a5befca90e38aaf5".to_owned(),
            String::new(),
        )),
        "Alex",
        None,
        None,
    )
    .await
    .expect_err("an empty access token is refused");
    assert!(matches!(err, HorizonError::InvalidSession), "{err:?}");

    // ---- a build that no longer promises an option we pass ----
    let other = root.join("horizon/builds").join(BUILD_ID_NO_HOME);
    fs::create_dir_all(&other).unwrap();
    fs::write(
        other.join("build.json"),
        r#"{"tag": "nightly", "asset": "fake.tar.gz", "sha256": "bbbb"}"#,
    )
    .unwrap();
    fs::write(
        other.join("research-lock.json"),
        r#"{"minecraft": {"version": "26.3",
            "manifest_sha1": "11f1aefe473eefba0af62e3286c33b692807b2eb"}}"#,
    )
    .unwrap();
    write_script(
        &other.join("horizon-client"),
        &format!(
            "#!/bin/sh\n\
             if [ \"$1\" = --launch-api ]; then\n\
               echo '{{\"launch_api\":\"0.2.0\",\"options\":[\n\
                 {{\"name\":\"--resources\",\"value\":\"DIR\"}},\n\
                 {{\"name\":\"--game-dir\",\"value\":\"DIR\"}},\n\
                 {{\"name\":\"--session-stdin\"}}],\"content\":{{}}}}'\n\
               exit 0\n\
             fi\n\
             printf '%s\\n' \"$@\" > '{args}'\n",
            args = out.join("args-removed-option.txt").display(),
        ),
    );
    write_script(
        &other.join("horizon-prepare"),
        "#!/bin/sh\ncase \"$1\" in\n  --check) exit 0 ;;\n  --launch-api) \
         echo '{\"launch_api\":\"0.2.0\",\"minecraft\":\"26.3\",\"java_major\":25,\"options\":[]}' ;;\nesac\n",
    );

    let other_instance = setup_instance(&root, "demo-no-home").await;
    let other_config = config(&other_instance, BUILD_ID_NO_HOME).await;
    let err = ql_horizon::launch(
        "demo-no-home",
        &other_config,
        other_config.horizon.as_ref().unwrap(),
        Some(Session::new(
            "Alex".to_owned(),
            "069a79f444e94726a5befca90e38aaf5".to_owned(),
            "session-token".to_owned(),
        )),
        "Alex",
        None,
        None,
    )
    .await
    .expect_err("a build that dropped --home must be refused before spawning");

    assert!(
        matches!(&err, ql_horizon::HorizonError::UnsupportedOption { option, .. } if option == "--home"),
        "expected UnsupportedOption(--home), got: {err:?}"
    );
    assert!(
        !out.join("args-removed-option.txt").exists(),
        "the game must not be spawned when the contract check fails"
    );
}

/// A launcher data folder for this test process, so nothing real is touched.
/// (One test per process, so setting the variable once is safe.)
fn sandbox() -> PathBuf {
    let root = std::env::temp_dir().join(format!("ql-horizon-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    unsafe { std::env::set_var("QL_DIR", &root) };
    root
}

async fn setup_instance(root: &Path, name: &str) -> Instance {
    let _ = root;
    let instance = Instance::client(name);
    fs::create_dir_all(instance.get_instance_path()).unwrap();
    instance
}

async fn config(instance: &Instance, build_id: &str) -> InstanceConfigJson {
    let mut config = InstanceConfigJson::new(
        InstanceKind::Client,
        false,
        VersionInfo::new("horizon-nightly-test"),
    );
    config.horizon = Some(HorizonConfig::new(
        "nightly".to_owned(),
        build_id.to_owned(),
        "26.3".to_owned(),
    ));
    config.game_args = Some(vec!["--fullscreen".to_owned()]);
    config.save(instance).await.unwrap();
    config
}

async fn wait(process: &ql_core::LaunchedProcess) {
    let mut child = process.child.lock().await;
    child.wait().await.unwrap();
}

fn sha1_bytes(bytes: &[u8]) -> String {
    let digest = Sha1::digest(bytes);
    let mut hex = String::with_capacity(40);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn write_script(path: &Path, contents: &str) -> PathBuf {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
    path.to_owned()
}
