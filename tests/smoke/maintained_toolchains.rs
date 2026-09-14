use std::env;
use std::path::Path;

use crate::support::temp_project::{temp_project, TempProject};

use hamcrest2::assert_that;
use hamcrest2::prelude::*;
use test_support::matchers::execs;
use test_support::process::ProcessBuilder;

const PACKAGE_JSON: &str = r#"{
    "name": "volta-maintained-toolchains-smoke",
    "private": true
}"#;

fn pnpm_global_command(project: &TempProject, pnpm_home: &Path, args: &str) -> ProcessBuilder {
    let mut command = project.exec_shim("pnpm", args);
    let inherited_path = command
        .get_env("PATH")
        .expect("smoke test commands should have a PATH");
    let path = env::join_paths(
        std::iter::once(pnpm_home.join("bin")).chain(env::split_paths(&inherited_path)),
    )
    .expect("temporary pnpm bin directory should be valid in PATH");
    command.env("PATH", path);
    command
}

#[test]
fn current_node_install_pin_and_run() {
    let p = temp_project().package_json(PACKAGE_JSON).build();

    assert_that!(p.volta("install node@latest"), execs().with_status(0));
    assert_that!(
        p.volta("run --node latest node --version"),
        execs().with_status(0)
    );

    assert_that!(p.volta("pin node@latest"), execs().with_status(0));
    assert_that!(p.node("--version"), execs().with_status(0));
}

#[test]
fn isolated_tools_survive_publication_upgrade_and_installer_removal() {
    let p = temp_project().package_json(PACKAGE_JSON).build();

    assert_that!(p.volta("install node@lts"), execs().with_status(0));
    assert_that!(p.volta("tool install cowsay@1.6.0"), execs().with_status(0));
    assert_that!(
        p.volta("which cowsay"),
        execs().with_status(0).with_stdout_contains(
            "[..]tools/environments/installed/cowsay/installations/[..]/node_modules/.bin/cowsay"
        )
    );
    assert_that!(
        p.exec_shim("cowsay", "published-tool"),
        execs()
            .with_status(0)
            .with_stdout_contains("[..]published-tool[..]")
    );
    assert_that!(
        p.volta("tool run cowsay -- receipt-tool"),
        execs()
            .with_status(0)
            .with_stdout_contains("[..]receipt-tool[..]")
    );
    assert_that!(p.volta("tool upgrade cowsay"), execs().with_status(0));
    assert_that!(
        p.exec_shim("cowsay", "upgraded-tool"),
        execs()
            .with_status(0)
            .with_stdout_contains("[..]upgraded-tool[..]")
    );

    assert_that!(
        p.volta("tool install esbuild@0.25.0 --allow-build esbuild"),
        execs().with_status(0)
    );
    assert_that!(
        p.exec_shim("esbuild", "--version"),
        execs().with_status(0).with_stdout("0.25.0\n")
    );

    assert_that!(p.volta("install pnpm@latest"), execs().with_status(0));
    assert_that!(p.volta("uninstall pnpm"), execs().with_status(0));
    assert_that!(
        p.exec_shim("cowsay", "without-installer"),
        execs()
            .with_status(0)
            .with_stdout_contains("[..]without-installer[..]")
    );
    assert_that!(p.volta("tool upgrade cowsay"), execs().with_status(0));
    assert_that!(p.volta("tool uninstall cowsay"), execs().with_status(0));
    assert!(!p.shim_exists("cowsay"));
    assert!(!p.shim_exists("cowthink"));
    assert_that!(
        p.exec_shim("esbuild", "--version"),
        execs().with_status(0).with_stdout("0.25.0\n")
    );
}

#[test]
fn lts_node_and_current_package_managers() {
    let builder = temp_project().package_json(PACKAGE_JSON);
    let pnpm_home = builder.root().join("pnpm-home");
    let p = builder
        .env(
            "PNPM_HOME",
            pnpm_home
                .to_str()
                .expect("temporary pnpm home should be valid UTF-8"),
        )
        .build();

    assert_that!(p.volta("install node@lts"), execs().with_status(0));
    assert_that!(
        p.volta("run --node lts node --version"),
        execs().with_status(0)
    );

    assert_that!(p.volta("pin node@lts"), execs().with_status(0));
    assert_that!(p.node("--version"), execs().with_status(0));
    assert_that!(p.npm("--version"), execs().with_status(0));
    assert_that!(p.exec_shim("npx", "--version"), execs().with_status(0));

    assert_that!(p.volta("install npm@latest"), execs().with_status(0));
    assert_that!(p.volta("pin npm@latest"), execs().with_status(0));
    assert_that!(p.npm("--version"), execs().with_status(0));
    assert_that!(
        p.volta("run --npm latest npm --version"),
        execs().with_status(0)
    );

    assert_that!(p.volta("install yarn@latest"), execs().with_status(0));
    assert_that!(p.volta("pin yarn@latest"), execs().with_status(0));
    assert_that!(p.yarn("--version"), execs().with_status(0));
    assert_that!(
        p.volta("run --yarn latest yarn --version"),
        execs().with_status(0)
    );

    assert_that!(p.volta("fetch pnpm@latest"), execs().with_status(0));
    assert_that!(p.volta("install pnpm@latest"), execs().with_status(0));
    assert_that!(p.volta("pin pnpm@latest"), execs().with_status(0));
    assert_that!(p.exec_shim("pnpm", "--version"), execs().with_status(0));
    assert_that!(
        p.volta("run --pnpm latest pnpm --version"),
        execs().with_status(0)
    );
    assert_that!(
        p.exec_shim("pnpm", "exec node --version"),
        execs().with_status(0)
    );
    assert_that!(
        p.exec_shim("pnpm", "dlx cowsay@1.6.0 smoke"),
        execs().with_status(0)
    );
    assert_that!(
        pnpm_global_command(&p, &pnpm_home, "add --global cowsay@1.6.0"),
        execs().with_status(0)
    );
    assert_that!(
        pnpm_global_command(&p, &pnpm_home, "list --global --depth 0"),
        execs()
            .with_status(0)
            .with_stdout_contains("[..]cowsay@1.6.0[..]")
    );
    assert_that!(
        pnpm_global_command(&p, &pnpm_home, "remove --global cowsay"),
        execs().with_status(0)
    );
}
