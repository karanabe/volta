//! Tests that global package operations are owned by the selected package manager.

use std::path::PathBuf;

use crate::support::sandbox::{sandbox, Sandbox};
use cfg_if::cfg_if;
use hamcrest2::assert_that;
use hamcrest2::prelude::*;
use test_support::matchers::execs;

const PLATFORM: &str = r#"{
  "node": {
    "runtime": "11.10.1",
    "npm": "6.7.0"
  },
  "pnpm": "7.7.1",
  "yarn": "1.23.483"
}"#;

fn package_manager_bin(name: &str) -> String {
    cfg_if! {
        if #[cfg(target_os = "windows")] {
            format!(
                r#"@echo off
echo {name} args: %*
"#
            )
        } else {
            format!(
                r#"#!/bin/sh
echo "{name} args: $@"
"#
            )
        }
    }
}

fn test_sandbox() -> Sandbox {
    sandbox()
        .layout_file("v5")
        .platform(PLATFORM)
        .setup_node_binary("11.10.1", "6.7.0", &package_manager_bin("node"))
        .setup_npm_binary("6.7.0", &package_manager_bin("npm"))
        .setup_pnpm_binary("7.7.1", &package_manager_bin("pnpm"))
        .setup_yarn_binary("1.23.483", &package_manager_bin("yarn"))
        .add_dir_to_path(PathBuf::from("/bin"))
        .env("VOLTA_LOGLEVEL", "info")
        .build()
}

#[test]
fn npm_global_commands_pass_through() {
    let s = test_sandbox();

    assert_that!(
        s.npm("install --global typescript@latest"),
        execs()
            .with_status(0)
            .with_stdout_contains("npm args: install --global typescript@latest")
            .with_stdout_does_not_contain("[..]using Volta[..]")
    );
    assert_that!(
        s.npm("update --global typescript"),
        execs()
            .with_status(0)
            .with_stdout_contains("npm args: update --global typescript")
    );
    assert_that!(
        s.npm("uninstall --global typescript"),
        execs()
            .with_status(0)
            .with_stdout_contains("npm args: uninstall --global typescript")
    );
    assert_that!(
        s.npm("link"),
        execs()
            .with_status(0)
            .with_stdout_contains("npm args: link")
    );

    assert!(!Sandbox::package_config_exists("typescript"));
}

#[test]
fn pnpm_global_commands_pass_through() {
    let s = test_sandbox();

    assert_that!(
        s.pnpm("add --global typescript@latest"),
        execs()
            .with_status(0)
            .with_stdout_contains("pnpm args: add --global typescript@latest")
    );
    assert_that!(
        s.pnpm("update --global typescript"),
        execs()
            .with_status(0)
            .with_stdout_contains("pnpm args: update --global typescript")
    );
    assert_that!(
        s.pnpm("remove --global typescript"),
        execs()
            .with_status(0)
            .with_stdout_contains("pnpm args: remove --global typescript")
    );

    assert!(!Sandbox::package_config_exists("typescript"));
}

#[test]
fn yarn_global_commands_pass_through() {
    let s = test_sandbox();

    assert_that!(
        s.yarn("global add typescript@latest"),
        execs()
            .with_status(0)
            .with_stdout_contains("yarn args: global add typescript@latest")
            .with_stdout_does_not_contain("[..]using Volta[..]")
    );
    assert_that!(
        s.yarn("global upgrade typescript"),
        execs()
            .with_status(0)
            .with_stdout_contains("yarn args: global upgrade typescript")
    );
    assert_that!(
        s.yarn("global remove typescript"),
        execs()
            .with_status(0)
            .with_stdout_contains("yarn args: global remove typescript")
    );

    assert!(!Sandbox::package_config_exists("typescript"));
}
