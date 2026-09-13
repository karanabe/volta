//! End-to-end coverage for isolated JavaScript CLI tool environments.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use crate::support::sandbox::{sandbox, PackageBinInfo, Sandbox};
use hamcrest2::assert_that;
use hamcrest2::prelude::*;
use serde_json::Value;
use test_support::matchers::execs;

const PLATFORM: &str = r#"{
  "node": {
    "runtime": "11.10.1",
    "npm": "6.7.0"
  },
  "pnpm": null,
  "yarn": null
}"#;

const NODE: &str = r#"#!/bin/sh
echo "node args: $@"
"#;

// This fixture stands in for npm's dependency solver and materializer. It
// intentionally produces a conventional npm node_modules topology so the test
// remains deterministic and does not need the public registry.
const NPM: &str = r#"#!/bin/sh
spec=
for arg in "$@"; do
  spec=$arg
done

case "$spec" in
  alpha|alpha@1)
    name=alpha
    version=1.0.0
    command=alpha
    shared=1.0.0
    ;;
  alpha@2)
    name=alpha
    version=2.0.0
    command=alpha
    shared=1.0.0
    ;;
  alpha@broken)
    exit 42
    ;;
  beta)
    name=beta
    version=1.0.0
    command=beta
    shared=1.0.0
    ;;
  gamma)
    name=gamma
    version=1.0.0
    command=gamma
    shared=2.0.0
    ;;
  multi)
    name=multi
    version=1.0.0
    command=first
    shared=
    ;;
  @scope/scoped)
    name=@scope/scoped
    version=1.2.3
    command=scoped
    shared=
    ;;
  no-bin)
    name=no-bin
    version=1.0.0
    command=
    shared=
    ;;
  conflict-a)
    name=conflict-a
    version=1.0.0
    command=dupe
    shared=
    ;;
  conflict-b)
    name=conflict-b
    version=1.0.0
    command=dupe
    shared=
    ;;
  *)
    echo "unsupported fixture package: $spec" >&2
    exit 2
    ;;
esac

mkdir -p "node_modules/$name" node_modules/.bin
if [ -n "$shared" ]; then
  cat > "node_modules/$name/package.json" <<EOF
{"name":"$name","version":"$version","bin":{"$command":"cli.sh"},"dependencies":{"shared":"$shared"}}
EOF
  mkdir -p node_modules/shared
  cat > node_modules/shared/package.json <<EOF
{"name":"shared","version":"$shared"}
EOF
  echo "shared $shared" > node_modules/shared/index.js
else
  if [ -n "$command" ]; then
    cat > "node_modules/$name/package.json" <<EOF
{"name":"$name","version":"$version","bin":{"$command":"cli.sh"}}
EOF
  else
    cat > "node_modules/$name/package.json" <<EOF
{"name":"$name","version":"$version"}
EOF
  fi
fi

if [ "$name" = multi ]; then
  cat > "node_modules/$name/package.json" <<EOF
{"name":"multi","version":"1.0.0","bin":{"first":"cli.sh","second":"cli.sh"}}
EOF
fi

if [ -n "$command" ]; then
  cat > "node_modules/$name/cli.sh" <<EOF
#!/bin/sh
echo "$name@$version args: \$*"
EOF
  chmod +x "node_modules/$name/cli.sh"
  ln -s "../$name/cli.sh" "node_modules/.bin/$command"
fi
if [ "$name" = multi ]; then
  ln -s ../multi/cli.sh node_modules/.bin/second
fi

cat > package-lock.json <<EOF
{"name":"volta-tool-environment","lockfileVersion":3,"packages":{"node_modules/$name":{"integrity":"sha512-root-$name-$version"},"node_modules/shared":{"integrity":"sha512-shared-$shared"}}}
EOF
exit 0
"#;

fn test_sandbox() -> Sandbox {
    sandbox()
        .layout_file("v4")
        .platform(PLATFORM)
        .setup_node_binary("11.10.1", "6.7.0", NODE)
        .setup_npm_binary("6.7.0", NPM)
        .add_dir_to_path(PathBuf::from("/bin"))
        .env("VOLTA_LOGLEVEL", "info")
        .build()
}

fn store_entry_count() -> usize {
    fs::read_dir(test_support::paths::home().join(".volta/store/packages"))
        .expect("package store exists")
        .count()
}

fn installed_environment(package: &str) -> PathBuf {
    let environments = test_support::paths::home().join(".volta/tools/environments");
    let registry: Value = serde_json::from_reader(
        fs::File::open(environments.join("registry.json"))
            .expect("isolated tool registry must exist"),
    )
    .expect("registry must be valid JSON");
    let installation = registry["tools"][package]["installation"]
        .as_str()
        .expect("tool installation id");
    environments
        .join("installed")
        .join(package)
        .join("installations")
        .join(installation)
}

fn installed_manifest(package: &str) -> Value {
    serde_json::from_reader(
        fs::File::open(installed_environment(package).join("volta-tool.json"))
            .expect("tool manifest must exist"),
    )
    .expect("tool manifest must be valid JSON")
}

fn package_content_hash<'a>(manifest: &'a Value, package: &str) -> &'a str {
    manifest["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .find(|entry| entry["name"] == package)
        .and_then(|entry| entry["content_hash"].as_str())
        .expect("package content hash")
}

#[test]
fn installs_independent_tools_reuses_content_and_uninstalls_independently() {
    let s = test_sandbox();

    assert_that!(s.volta("tool install alpha"), execs().with_status(0));
    assert_that!(
        s.exec_shim("alpha", "one two"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@1.0.0 args: one two")
    );
    assert_that!(s.volta("tool install beta"), execs().with_status(0));

    // alpha, beta, and their one identical shared dependency produce three
    // content entries rather than four.
    assert_eq!(store_entry_count(), 3);
    let alpha = installed_manifest("alpha");
    let beta = installed_manifest("beta");
    assert_eq!(
        package_content_hash(&alpha, "shared"),
        package_content_hash(&beta, "shared")
    );
    let shared_hash = package_content_hash(&alpha, "shared");
    let environment_file = installed_environment("alpha").join("node_modules/shared/index.js");
    let store_file = test_support::paths::home()
        .join(".volta/store/packages")
        .join(shared_hash)
        .join("index.js");
    let environment_metadata = fs::metadata(environment_file).expect("environment shared file");
    let store_metadata = fs::metadata(store_file).expect("stored shared file");
    assert_eq!(environment_metadata.dev(), store_metadata.dev());
    assert_eq!(environment_metadata.ino(), store_metadata.ino());
    let alpha_package = alpha["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .find(|entry| entry["name"] == "alpha")
        .expect("alpha package node");
    assert_eq!(
        alpha_package["dependencies"][0]["target"],
        "node_modules/shared"
    );
    assert_that!(
        s.volta("tool list"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@1.0.0 (node@11.10.1) [alpha]")
            .with_stdout_contains("beta@1.0.0 (node@11.10.1) [beta]")
    );
    assert_that!(
        s.volta("tool which alpha"),
        execs().with_status(0).with_stdout_contains(
            "[..]tools/environments/installed/alpha/installations/[..]/node_modules/.bin/alpha"
        )
    );

    assert_that!(s.volta("tool uninstall alpha"), execs().with_status(0));
    assert!(!Sandbox::shim_exists("alpha"));
    assert_that!(
        s.exec_shim("beta", "still-works"),
        execs()
            .with_status(0)
            .with_stdout_contains("beta@1.0.0 args: still-works")
    );
}

#[test]
fn keeps_conflicting_versions_isolated_and_supports_scopes_and_multiple_bins() {
    let s = test_sandbox();

    assert_that!(s.volta("tool install alpha"), execs().with_status(0));
    assert_that!(s.volta("tool install gamma"), execs().with_status(0));
    // alpha + gamma + two different shared dependency contents.
    assert_eq!(store_entry_count(), 4);
    let alpha = installed_manifest("alpha");
    let gamma = installed_manifest("gamma");
    assert_ne!(
        package_content_hash(&alpha, "shared"),
        package_content_hash(&gamma, "shared")
    );

    assert_that!(
        s.volta("tool install @scope/scoped"),
        execs().with_status(0)
    );
    assert_that!(
        s.exec_shim("scoped", "ok"),
        execs()
            .with_status(0)
            .with_stdout_contains("@scope/scoped@1.2.3 args: ok")
    );

    assert_that!(s.volta("tool install multi"), execs().with_status(0));
    assert_that!(
        s.volta("tool run multi"),
        execs()
            .with_status(3)
            .with_stderr_contains("[..]exposes multiple executables: first, second[..]")
    );
    assert_that!(
        s.exec_shim("first", "a"),
        execs()
            .with_status(0)
            .with_stdout_contains("multi@1.0.0 args: a")
    );
    assert_that!(
        s.volta("tool run second b"),
        execs()
            .with_status(0)
            .with_stdout_contains("multi@1.0.0 args: b")
    );
}

#[test]
fn command_conflicts_and_failed_updates_leave_the_working_tool_unchanged() {
    let s = test_sandbox();

    assert_that!(s.volta("tool install conflict-a"), execs().with_status(0));
    assert_that!(
        s.volta("tool install conflict-b"),
        execs()
            .with_status(7)
            .with_stderr_contains("[..]Executable 'dupe' is already installed by conflict-a[..]")
    );
    assert_that!(
        s.exec_shim("dupe", "survives"),
        execs()
            .with_status(0)
            .with_stdout_contains("conflict-a@1.0.0 args: survives")
    );

    assert_that!(s.volta("tool install alpha"), execs().with_status(0));
    assert_that!(s.volta("tool install alpha@broken"), execs().with_status(1));
    assert_that!(
        s.exec_shim("alpha", "old"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@1.0.0 args: old")
    );
    assert_that!(s.volta("tool install alpha@2"), execs().with_status(0));
    assert_that!(
        s.exec_shim("alpha", "new"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@2.0.0 args: new")
    );
}

#[test]
fn rejects_non_cli_packages_and_reports_incomplete_environments() {
    let s = test_sandbox();

    assert_that!(
        s.volta("tool install no-bin"),
        execs()
            .with_status(3)
            .with_stderr_contains("[..]does not expose a command-line executable[..]")
    );

    assert_that!(s.volta("tool install alpha"), execs().with_status(0));
    let registry_path = test_support::paths::home().join(".volta/tools/environments/registry.json");
    let registry: Value = serde_json::from_reader(
        fs::File::open(registry_path).expect("isolated tool registry must exist"),
    )
    .expect("registry must be valid JSON");
    let installation = registry["tools"]["alpha"]["installation"]
        .as_str()
        .expect("alpha installation id");
    let executable = test_support::paths::home()
        .join(".volta/tools/environments/installed/alpha/installations")
        .join(installation)
        .join("node_modules/.bin/alpha");
    fs::remove_file(&executable).expect("remove tool executable");

    assert_that!(
        s.volta("tool which alpha"),
        execs()
            .with_status(8)
            .with_stderr_contains("[..]environment for 'alpha' is incomplete or corrupt[..]")
    );

    fs::write(&executable, "#!/bin/sh\necho replaced\n").expect("replace tool executable");
    assert_that!(
        s.volta("tool which alpha"),
        execs()
            .with_status(8)
            .with_stderr_contains("[..]executable 'alpha' failed integrity verification[..]")
    );
}

#[test]
fn reports_environment_manifest_tampering() {
    let s = test_sandbox();

    assert_that!(s.volta("tool install alpha"), execs().with_status(0));
    let environments = test_support::paths::home().join(".volta/tools/environments");
    let registry: Value = serde_json::from_reader(
        fs::File::open(environments.join("registry.json"))
            .expect("isolated tool registry must exist"),
    )
    .expect("registry must be valid JSON");
    let installation = registry["tools"]["alpha"]["installation"]
        .as_str()
        .expect("alpha installation id");
    let manifest_path = environments
        .join("installed/alpha/installations")
        .join(installation)
        .join("volta-tool.json");
    let mut manifest: Value =
        serde_json::from_reader(fs::File::open(&manifest_path).expect("tool manifest must exist"))
            .expect("tool manifest must be valid JSON");
    manifest["requested"] = Value::String("alpha@tampered".to_owned());
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize changed manifest"),
    )
    .expect("tamper with tool manifest");

    assert_that!(
        s.volta("tool list"),
        execs()
            .with_status(8)
            .with_stderr_contains("[..]manifest does not match its installation identifier[..]")
    );
}

#[test]
fn project_local_command_keeps_precedence_over_an_isolated_global_tool() {
    let s = sandbox()
        .layout_file("v4")
        .platform(PLATFORM)
        .package_json(r#"{"name":"project","dependencies":{"alpha":"1.0.0"}}"#)
        .project_bins(vec![PackageBinInfo {
            name: "alpha".to_owned(),
            contents: "#!/bin/sh\necho project-local\n".to_owned(),
        }])
        .setup_node_binary("11.10.1", "6.7.0", NODE)
        .setup_npm_binary("6.7.0", NPM)
        .add_dir_to_path(PathBuf::from("/bin"))
        .build();

    assert_that!(s.volta("tool install alpha"), execs().with_status(0));
    assert_that!(
        s.exec_shim("alpha", ""),
        execs()
            .with_status(0)
            .with_stdout_contains("project-local")
            .with_stdout_does_not_contain("alpha@1.0.0")
    );
}

#[test]
fn persists_the_current_project_node_runtime_at_install_time() {
    let s = sandbox()
        .layout_file("v4")
        .platform(PLATFORM)
        .package_json(r#"{"name":"project","volta":{"node":"10.99.1040","npm":"6.7.0"}}"#)
        .setup_node_binary("11.10.1", "6.7.0", NODE)
        .setup_node_binary("10.99.1040", "6.7.0", NODE)
        .setup_npm_binary("6.7.0", NPM)
        .add_dir_to_path(PathBuf::from("/bin"))
        .build();

    assert_that!(s.volta("tool install alpha"), execs().with_status(0));
    assert_that!(
        s.volta("tool list"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@1.0.0 (node@10.99.1040) [alpha]")
    );
}
