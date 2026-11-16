use super::*;

#[tokio::test]
async fn horizon_main_menu_guards_mods_and_captures_launch_stage() {
    // The other launcher unit tests are pure state tests and do not access QL_DIR.
    let root = std::env::temp_dir().join(format!("ql-horizon-ui-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    unsafe { std::env::set_var("QL_DIR", &root) };
    let instance = Instance::client("horizon");
    std::fs::create_dir_all(instance.get_instance_path()).unwrap();
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
    assert!(matches!(&launcher.state, State::Launch(menu) if menu.is_horizon));

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

    std::fs::remove_dir_all(root).unwrap();
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
