use super::*;

#[tokio::test]
async fn horizon_main_menu_routes_content_and_captures_launch_stage() {
    // LAUNCHER_DIR is cached process-wide. Set QL_DIR before the test binary
    // starts rather than racing other tests (or an already initialized cache).
    const SANDBOX: &str = "QL_HORIZON_UI_TEST_SANDBOX";
    let Some(root) = std::env::var_os(SANDBOX) else {
        let sandbox = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::horizon_tests::horizon_main_menu_routes_content_and_captures_launch_stage",
            ])
            .env("QL_DIR", sandbox.path())
            .env(SANDBOX, sandbox.path())
            .status()
            .unwrap();
        assert!(status.success());
        // Also catch an exact filter that no longer matches any test.
        assert!(
            sandbox
                .path()
                .join("instances/horizon/config.json")
                .is_file()
        );
        return;
    };
    let instance = Instance::client("horizon");
    let instances = std::fs::canonicalize(root).unwrap().join("instances");
    assert_eq!(
        std::fs::canonicalize(&*LAUNCHER_DIR)
            .unwrap()
            .join("instances"),
        instances
    );
    std::fs::create_dir_all(instances).unwrap();
    std::fs::create_dir(instance.get_instance_path()).unwrap();
    let mut config = ql_core::InstanceConfigJson::new(
        InstanceKind::Client,
        false,
        ql_core::json::instance_config::VersionInfo::new("horizon"),
    );
    config.horizon = Some(ql_horizon::HorizonConfig::new(
        "nightly".into(),
        "nightly-test".into(),
        "26.3".into(),
    ));
    config.save(&instance).await.unwrap();

    let mut launcher = Launcher::with_error("Could not get launcher dir (test fixture)");
    launcher.selected_instance = Some(instance.clone());
    let _ = launcher.go_to_main_menu(None);
    assert!(
        matches!(&launcher.state, State::Launch(menu) if menu.is_horizon && menu.edit_instance.is_none())
    );
    // No details.json exists; a Java-shaped mod path would fail here.
    let _ = launcher.go_to_edit_mods_menu(None);
    assert!(matches!(&launcher.state, State::HorizonContent(menu) if menu.instance == instance));
    let _ = launcher.go_to_main_menu(None);

    let _ = launcher.launch_game(None);
    assert_eq!(
        launcher.java_recv.as_ref().unwrap().launch_stage,
        LaunchProgressStage::HorizonPreparation
    );
    let _ = launcher.go_to_main_menu(None);
    assert!(matches!(&launcher.state, State::Launch(menu) if menu.is_horizon));

    // Switching to a Java-shaped configuration restores the button, but cannot
    // change the stage of a launch that was already dispatched.
    config.horizon = None;
    config.save(&instance).await.unwrap();
    let _ = launcher.on_selecting_instance();
    assert!(matches!(&launcher.state, State::Launch(menu) if !menu.is_horizon));
    assert_eq!(
        launcher.java_recv.as_ref().unwrap().launch_stage,
        LaunchProgressStage::HorizonPreparation
    );
    let _ = launcher.launch_game(None);
    assert_eq!(
        launcher.java_recv.as_ref().unwrap().launch_stage,
        LaunchProgressStage::JavaInstallation
    );
}

#[test]
fn progress_text_does_not_select_the_launch_stage() {
    for stage in [
        LaunchProgressStage::JavaInstallation,
        LaunchProgressStage::HorizonPreparation,
    ] {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut progress = ProgressBar::with_recv(receiver);
        progress.launch_stage = stage;
        for message in [None, Some("Downloading Java".into()), Some(String::new())] {
            sender
                .send(GenericProgress {
                    message,
                    ..Default::default()
                })
                .unwrap();
            assert!(progress.tick());
            assert_eq!(progress.launch_stage, stage);
        }
    }
}
