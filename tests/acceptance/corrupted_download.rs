use crate::support::sandbox::{
    sandbox, DistroFixture, DistroMetadata, NodeFixture, NpmFixture, PnpmFixture, Yarn1Fixture,
    YarnBerryFixture,
};
use hamcrest2::assert_that;
use hamcrest2::prelude::*;
use node_semver::Version;
use test_support::matchers::execs;

use std::fs;
use volta_core::error::ExitCode;

const NODE_VERSION_INFO: &str = r#"[
{"version":"v10.99.1040","npm":"6.2.26","lts": "Dubnium","files":["linux-x64","osx-x64-tar","win-x64-zip","win-x86-zip", "linux-arm64"]},
{"version":"v0.0.1","npm":"0.0.2","lts": "Sure","files":["linux-x64","osx-x64-tar","win-x64-zip","win-x86-zip", "linux-arm64"]}
]
"#;

const NODE_VERSION_FIXTURES: [DistroMetadata; 2] = [
    DistroMetadata {
        version: "0.0.1",
        uncompressed_size: Some(0x0028_0000),
    },
    DistroMetadata {
        version: "10.99.1040",
        uncompressed_size: Some(0x0028_0000),
    },
];

const PNPM_VERSION_INFO: &str = r#"
{
    "name":"pnpm",
    "dist-tags": { "latest":"7.7.1" },
    "versions": {
        "0.0.1": { "version":"0.0.1", "dist": { "shasum":"", "tarball":"" }},
        "7.7.1": { "version":"7.7.1", "dist": { "shasum":"", "tarball":"" }}
    }
}
"#;

const PNPM_VERSION_FIXTURES: [DistroMetadata; 2] = [
    DistroMetadata {
        version: "0.0.1",
        uncompressed_size: Some(0x0028_0000),
    },
    DistroMetadata {
        version: "7.7.1",
        uncompressed_size: Some(0x0028_0000),
    },
];

const YARN_1_VERSION_INFO: &str = r#"{
    "name":"yarn",
    "dist-tags": { "latest": "1.2.42" },
    "versions": {
        "0.0.1": { "version":"0.0.1", "dist": { "shasum":"", "tarball":"" }},
        "1.2.42": { "version":"1.2.42", "dist": { "shasum:"", "tarball":"" }}
    }
}"#;

const YARN_1_VERSION_FIXTURES: [DistroMetadata; 2] = [
    DistroMetadata {
        version: "0.0.1",
        uncompressed_size: Some(0x0028_0000),
    },
    DistroMetadata {
        version: "1.2.42",
        uncompressed_size: Some(0x0028_0000),
    },
];

#[test]
fn install_corrupted_node_leaves_inventory_unchanged() {
    let s = sandbox()
        .node_available_versions(NODE_VERSION_INFO)
        .distro_mocks::<NodeFixture>(&NODE_VERSION_FIXTURES)
        .build();

    assert_that!(
        s.volta("install node@0.0.1"),
        execs().with_status(ExitCode::UnknownError as i32)
    );

    assert!(!s.node_inventory_archive_exists(&Version::parse("0.0.1").unwrap()));
}

#[test]
fn install_valid_node_saves_to_inventory() {
    let s = sandbox()
        .node_available_versions(NODE_VERSION_INFO)
        .distro_mocks::<NodeFixture>(&NODE_VERSION_FIXTURES)
        .build();

    assert_that!(
        s.volta("install node@10.99.1040"),
        execs().with_status(ExitCode::Success as i32)
    );

    assert!(s.node_inventory_archive_exists(&Version::parse("10.99.1040").unwrap()));
}

#[test]
fn install_corrupted_pnpm_leaves_inventory_unchanged() {
    let s = sandbox()
        .node_available_versions(NODE_VERSION_INFO)
        .pnpm_available_versions(PNPM_VERSION_INFO)
        .distro_mocks::<NodeFixture>(&NODE_VERSION_FIXTURES)
        .distro_mocks::<PnpmFixture>(&PNPM_VERSION_FIXTURES)
        .build();

    assert_that!(
        s.volta("install pnpm@0.0.1"),
        execs().with_status(ExitCode::UnknownError as i32)
    );

    assert!(!s.pnpm_inventory_archive_exists("0.0.1"));
}

#[test]
fn install_valid_pnpm_saves_to_inventory() {
    let s = sandbox()
        .platform(r#"{ "node": { "runtime": "1.2.3", "npm": null }, "yarn": null }"#)
        .node_available_versions(NODE_VERSION_INFO)
        .pnpm_available_versions(PNPM_VERSION_INFO)
        .distro_mocks::<NodeFixture>(&NODE_VERSION_FIXTURES)
        .distro_mocks::<PnpmFixture>(&PNPM_VERSION_FIXTURES)
        .build();

    assert_that!(
        s.volta("install pnpm@7.7.1"),
        execs().with_status(ExitCode::Success as i32)
    );

    assert!(s.pnpm_inventory_archive_exists("7.7.1"));
}

#[test]
fn install_corrupted_yarn_leaves_inventory_unchanged() {
    let s = sandbox()
        .node_available_versions(NODE_VERSION_INFO)
        .yarn_1_available_versions(YARN_1_VERSION_INFO)
        .distro_mocks::<NodeFixture>(&NODE_VERSION_FIXTURES)
        .distro_mocks::<Yarn1Fixture>(&YARN_1_VERSION_FIXTURES)
        .build();

    assert_that!(
        s.volta("install yarn@0.0.1"),
        execs().with_status(ExitCode::UnknownError as i32)
    );

    assert!(!s.yarn_inventory_archive_exists("0.0.1"));
}

#[test]
fn install_valid_yarn_saves_to_inventory() {
    let s = sandbox()
        .platform(r#"{ "node": { "runtime": "1.2.3", "npm": null }, "yarn": null }"#)
        .node_available_versions(NODE_VERSION_INFO)
        .yarn_1_available_versions(YARN_1_VERSION_INFO)
        .distro_mocks::<NodeFixture>(&NODE_VERSION_FIXTURES)
        .distro_mocks::<Yarn1Fixture>(&YARN_1_VERSION_FIXTURES)
        .build();

    assert_that!(
        s.volta("install yarn@1.2.42"),
        execs().with_status(ExitCode::Success as i32)
    );

    assert!(s.yarn_inventory_archive_exists("1.2.42"));
}

fn rejects_mismatched_archive<T: DistroFixture>(tool: &str, version: &'static str) {
    let fixture = T::from(DistroMetadata {
        version,
        uncompressed_size: None,
    });
    let mut s = sandbox().build();
    let _archive = s
        .mock("GET", fixture.server_path().as_str())
        .with_body_from_file(fixture.fixture_path())
        .create();
    let (path, body) = fixture
        .integrity_metadata(b"different published bytes")
        .unwrap();
    let _metadata = s.mock("GET", path.as_str()).with_body(body).create();

    assert_that!(
        s.volta(&format!("fetch {tool}@{version}")),
        execs()
            .with_status(ExitCode::NetworkError as i32)
            .with_stderr_contains("[..]Could not verify the archive[..]")
    );
    let home = test_support::paths::home().join(".volta");
    assert!(!home.join(format!("tools/image/{tool}/{version}")).exists());
    let inventory = home.join(format!("tools/inventory/{tool}"));
    assert!(fs::read_dir(inventory).unwrap().next().is_none());
    assert!(!home
        .join(format!("tools/inventory/node/node-v{version}-npm"))
        .exists());
}

#[test]
fn rejects_node_checksum_mismatch_before_extraction() {
    rejects_mismatched_archive::<NodeFixture>("node", "10.99.1040");
}

#[test]
fn rejects_npm_checksum_mismatch_before_extraction() {
    rejects_mismatched_archive::<NpmFixture>("npm", "1.2.3");
}

#[test]
fn rejects_pnpm_checksum_mismatch_before_extraction() {
    rejects_mismatched_archive::<PnpmFixture>("pnpm", "7.7.1");
}

#[test]
fn rejects_yarn_classic_checksum_mismatch_before_extraction() {
    rejects_mismatched_archive::<Yarn1Fixture>("yarn", "1.2.42");
}

#[test]
fn rejects_yarn_berry_checksum_mismatch_before_extraction() {
    rejects_mismatched_archive::<YarnBerryFixture>("yarn", "3.2.42");
}

#[test]
fn reuses_verified_archives_offline_and_rejects_modified_cache_contents() {
    let fixture = NodeFixture::from(DistroMetadata {
        version: "10.99.1040",
        uncompressed_size: None,
    });
    let mut s = sandbox().build();
    let archive = s
        .mock("GET", fixture.server_path().as_str())
        .with_body_from_file(fixture.fixture_path())
        .create();
    let (path, body) = fixture
        .integrity_metadata(&fs::read(fixture.fixture_path()).unwrap())
        .unwrap();
    let metadata = s.mock("GET", path.as_str()).with_body(body).create();
    assert_that!(s.volta("fetch node@10.99.1040"), execs().with_status(0));
    archive.remove();
    let home = test_support::paths::home().join(".volta");
    let image = home.join("tools/image/node/10.99.1040");
    let filename = volta_core::tool::Node::archive_filename(&Version::parse("10.99.1040").unwrap());
    let cache = home.join("tools/inventory/node").join(&filename);
    let checksum = cache.with_file_name(format!("{filename}.integrity"));

    // Old inventories have no sidecar. Verify them against published metadata
    // once without downloading the archive again.
    fs::remove_file(&checksum).unwrap();
    fs::remove_dir_all(&image).unwrap();
    assert_that!(s.volta("fetch node@10.99.1040"), execs().with_status(0));
    assert!(checksum.is_file());
    metadata.remove();
    let no_network = s
        .mock("GET", mockito::Matcher::Any)
        .with_status(503)
        .expect(0)
        .create();
    fs::remove_dir_all(&image).unwrap();
    assert_that!(s.volta("fetch node@10.99.1040"), execs().with_status(0));
    assert!(image.is_dir());
    no_network.assert();

    fs::remove_dir_all(&image).unwrap();
    fs::write(&cache, b"modified cached archive").unwrap();
    assert_that!(
        s.volta("fetch node@10.99.1040"),
        execs().with_status(ExitCode::NetworkError as i32)
    );
    assert!(!image.exists());

    no_network.remove();
    let _archive = s
        .mock("GET", fixture.server_path().as_str())
        .with_body_from_file(fixture.fixture_path())
        .create();
    let (path, body) = fixture
        .integrity_metadata(&fs::read(fixture.fixture_path()).unwrap())
        .unwrap();
    let _metadata = s.mock("GET", path.as_str()).with_body(body).create();
    assert_that!(s.volta("fetch node@10.99.1040"), execs().with_status(0));
    assert!(image.is_dir());

    assert_that!(s.volta("uninstall node@10.99.1040"), execs().with_status(0));
    assert!(!cache.exists());
    assert!(!checksum.exists());
}

#[test]
fn legacy_npm_checksums_work_but_missing_or_invalid_integrity_is_rejected() {
    for (dist, expected_status) in [
        (
            serde_json::json!({"shasum": "10d224f266a6fda6cc0acfc60a7ee41e68c70f64"}),
            0,
        ),
        (serde_json::json!({}), ExitCode::NetworkError as i32),
        (
            serde_json::json!({"integrity": "sha512-invalid", "shasum": "10d224f266a6fda6cc0acfc60a7ee41e68c70f64"}),
            ExitCode::NetworkError as i32,
        ),
    ] {
        let mut s = sandbox().build();
        let _archive = s
            .mock("GET", "/npm/-/npm-1.2.3.tgz")
            .with_body_from_file("tests/fixtures/npm-1.2.3.tgz")
            .create();
        let _metadata = s
            .mock("GET", "/npm/1.2.3")
            .with_body(
                serde_json::json!({"name": "npm", "version": "1.2.3", "dist": dist}).to_string(),
            )
            .create();
        assert_that!(
            s.volta("fetch npm@1.2.3"),
            execs().with_status(expected_status)
        );
        assert_eq!(
            test_support::paths::home()
                .join(".volta/tools/image/npm/1.2.3")
                .is_dir(),
            expected_status == 0
        );
    }
}
