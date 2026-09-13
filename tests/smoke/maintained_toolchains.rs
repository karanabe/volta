use crate::support::temp_project::temp_project;

use hamcrest2::assert_that;
use hamcrest2::prelude::*;
use test_support::matchers::execs;

const PACKAGE_JSON: &str = r#"{
    "name": "volta-maintained-toolchains-smoke",
    "private": true
}"#;

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
fn lts_node_and_current_package_managers() {
    let p = temp_project()
        .package_json(PACKAGE_JSON)
        .env("VOLTA_FEATURE_PNPM", "1")
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
}
