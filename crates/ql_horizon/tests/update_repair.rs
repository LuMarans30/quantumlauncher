//! Same-ID repairs must run the verified installer, not short-circuit on freshness.
#![cfg(unix)]

use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
};

use ql_core::{Instance, InstanceConfigJson, InstanceKind, json::instance_config::VersionInfo};
use ql_horizon::{
    HorizonConfig, Reporter,
    build::{self, HorizonBuild},
};
use sha2::{Digest, Sha256};

#[tokio::test]
async fn repairs_current_build_without_recreating_instance() {
    // This integration-test binary has one test and its own launcher root.
    let root = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("QL_DIR", root.path()) };

    let archive = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(archive);
    for (name, bytes) in [
        ("horizon-client", b"#!/bin/sh\nexit 0\n".as_slice()),
        ("horizon-prepare", b"#!/bin/sh\nexit 0\n".as_slice()),
        ("research-lock.json", br#"{"minecraft":{"version":"26.3","manifest_sha1":"11f1aefe473eefba0af62e3286c33b692807b2eb"}}"#.as_slice()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes).unwrap();
    }
    let archive = tar.into_inner().unwrap().finish().unwrap();
    let sha256 = format!("{:x}", Sha256::digest(&archive));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/build.tar.gz", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        // Both missing and incomplete installations must download the archive.
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).unwrap() > 0);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                archive.len()
            )
            .unwrap();
            stream.write_all(&archive).unwrap();
        }
    });
    let latest = HorizonBuild {
        tag: "nightly".into(),
        asset: "horizon-linux-x86_64.tar.gz".into(),
        asset_url: url,
        sha256,
    };
    let instance = Instance::client("repair-me");
    fs::create_dir_all(instance.get_dot_minecraft_path()).unwrap();
    let world = instance.get_dot_minecraft_path().join("world-sentinel");
    fs::write(&world, "keep my world").unwrap();
    let mut config =
        InstanceConfigJson::new(InstanceKind::Client, false, VersionInfo::new("horizon"));
    config.horizon = Some(HorizonConfig::new(
        "nightly".into(),
        latest.id(),
        "26.3".into(),
    ));
    config.game_args = Some(vec!["--fullscreen".into()]);
    config.save(&instance).await.unwrap();

    for _ in 0..2 {
        let repaired = ql_horizon::update_instance(
            "repair-me",
            config.clone(),
            latest.clone(),
            Reporter::Nobody,
        )
        .await
        .unwrap();
        assert_eq!(repaired.build_id, latest.id());
        let installed = build::installed_build(&repaired.build_id).await.unwrap();
        assert_eq!(fs::read_to_string(&world).unwrap(), "keep my world");
        let saved = InstanceConfigJson::read(&instance).await.unwrap();
        assert_eq!(saved.game_args, config.game_args);
        assert_eq!(saved.horizon.unwrap().build_id, latest.id());
        fs::remove_file(installed.client()).unwrap();
    }
    server.join().unwrap();
}
