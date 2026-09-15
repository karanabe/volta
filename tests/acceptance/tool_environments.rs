//! End-to-end coverage for isolated JavaScript CLI tool environments.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Stdio};

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

const PNPM_VERSION_INFO: &str = r#"{
  "name": "pnpm",
  "dist-tags": { "latest": "7.7.1" },
  "versions": {
    "7.7.1": { "version": "7.7.1", "dist": { "shasum": "", "tarball": "" } }
  }
}"#;

// This fixture stands in for pnpm's solver and materializer. It intentionally
// creates pnpm's linked node_modules topology and a pnpm lockfile.
const PNPM: &str = r#"#!/bin/sh
spec=
allow_build_esbuild=
store_dir=
seen_add=
for arg in "$@"; do
  case "$arg" in
    add) seen_add=1 ;;
    --allow-build=esbuild)
      [ -n "$seen_add" ] || exit 2
      allow_build_esbuild=1
      ;;
    --node-linker=*|--virtual-store-dir=*) exit 2 ;;
    --store-dir=*) store_dir=${arg#--store-dir=} ;;
  esac
  spec=$arg
done
: "${store_dir:?missing --store-dir}"
[ "$pnpm_config_node_linker" = isolated ] || exit 2
[ "$pnpm_config_virtual_store_dir" = node_modules/.pnpm ] || exit 2
[ "$pnpm_config_enable_global_virtual_store" = false ] || exit 2

case "$spec" in
  alpha)
    name=alpha
    if [ -f "$store_dir/fail-alpha" ]; then exit 42; fi
    marker="$store_dir/alpha-upgraded"
    if [ -f "$marker" ]; then version=2.0.0; else version=1.0.0; fi
    mkdir -p "$store_dir"
    : > "$marker"
    if [ -n "$allow_build_esbuild" ]; then
      : > "$store_dir/allow-build-esbuild"
    fi
    command=alpha
    ;;
  alpha@1)
    name=alpha
    version=1.0.0
    command=alpha
    ;;
  alpha@2)
    name=alpha
    version=2.0.0
    command=alpha
    ;;
  alpha@broken)
    exit 42
    ;;
  beta)
    name=beta
    version=1.0.0
    command=beta
    ;;
  gamma)
    name=gamma
    version=1.0.0
    command=gamma
    ;;
  multi)
    name=multi
    version=1.0.0
    command=first
    ;;
  @scope/scoped)
    name=@scope/scoped
    version=1.2.3
    command=scoped
    ;;
  no-bin)
    name=no-bin
    version=1.0.0
    command=
    ;;
  conflict-a)
    name=conflict-a
    version=1.0.0
    command=dupe
    ;;
  conflict-b)
    name=conflict-b
    version=1.0.0
    command=dupe
    ;;
  *)
    echo "unsupported fixture package: $spec" >&2
    exit 2
    ;;
esac

physical="node_modules/.pnpm/root-$version/node_modules/$name"
mkdir -p "$physical" node_modules/.bin
case "$name" in
  @*/*)
    scope=${name%%/*}
    mkdir -p "node_modules/$scope"
    ln -s "../.pnpm/root-$version/node_modules/$name" "node_modules/$name"
    ;;
  *)
    ln -s ".pnpm/root-$version/node_modules/$name" "node_modules/$name"
    ;;
esac
if [ -n "$command" ]; then
  cat > "$physical/package.json" <<EOF
{"name":"$name","version":"$version","bin":{"$command":"cli.sh"}}
EOF
else
  cat > "$physical/package.json" <<EOF
{"name":"$name","version":"$version"}
EOF
fi

if [ "$name" = multi ]; then
  cat > "$physical/package.json" <<EOF
{"name":"multi","version":"1.0.0","bin":{"first":"cli.sh","second":"cli.sh"}}
EOF
fi

if [ -n "$command" ]; then
  cat > "$physical/cli.sh" <<EOF
#!/bin/sh
echo "$name@$version args: \$*"
if [ "\$1" = "--node-path" ]; then
  command -v node
fi
if [ "\$1" = "--wait-for-update" ]; then
  read -r reply || exit 0
  "\$0" after-update || exit 1
  node --version
fi
EOF
  chmod +x "$physical/cli.sh"
  ln -s "../$name/cli.sh" "node_modules/.bin/$command"
fi
if [ "$name" = multi ]; then
  ln -s ../multi/cli.sh node_modules/.bin/second
fi

cat > pnpm-lock.yaml <<EOF
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      $name:
        version: $version
EOF
exit 0
"#;

fn test_sandbox() -> Sandbox {
    sandbox()
        .layout_file("v4")
        .platform(PLATFORM)
        .setup_node_binary("11.10.1", "6.7.0", NODE)
        .pnpm_available_versions(PNPM_VERSION_INFO)
        .setup_pnpm_binary("7.7.1", PNPM)
        .add_dir_to_path(PathBuf::from("/bin"))
        .env("VOLTA_LOGLEVEL", "info")
        .build()
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

#[test]
fn installs_linked_tools_and_uninstalls_independently() {
    let s = test_sandbox();

    assert_that!(s.volta("tool install alpha"), execs().with_status(0));
    assert_that!(
        s.exec_shim("alpha", "one two"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@1.0.0 args: one two")
    );
    assert_that!(
        s.exec_shim("alpha", "--node-path"),
        execs().with_status(0).with_stdout_contains(
            "[..]tools/environments/installed/alpha/installations/[..]/runtime/node/bin/node"
        )
    );
    assert_that!(s.volta("tool install beta"), execs().with_status(0));
    assert_that!(s.volta("uninstall pnpm@7.7.1"), execs().with_status(0));
    assert_that!(
        s.exec_shim("alpha", "without-installer"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@1.0.0 args: without-installer")
    );

    let alpha = installed_manifest("alpha");
    assert_eq!(alpha["schema_version"], 2);
    assert_eq!(alpha["package"]["requested"], "alpha");
    assert_eq!(alpha["runtime"]["resolved"], "11.10.1");
    assert_eq!(alpha["installer"]["kind"], "pnpm");
    assert_eq!(alpha["installer"]["version"], "7.7.1");
    assert!(installed_environment("alpha")
        .join("pnpm-lock.yaml")
        .is_file());
    assert!(
        fs::symlink_metadata(installed_environment("alpha").join("runtime/node"))
            .expect("runtime reference")
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(installed_environment("alpha").join("node_modules/alpha"))
            .expect("pnpm package link")
            .file_type()
            .is_symlink()
    );
    assert_that!(
        s.volta("tool list"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@1.0.0 (node@11.10.1, installed by pnpm@7.7.1) [alpha]")
            .with_stdout_contains("beta@1.0.0 (node@11.10.1, installed by pnpm@7.7.1) [beta]")
    );
    assert_that!(
        s.volta("tool which alpha"),
        execs().with_status(0).with_stdout_contains(
            "[..]tools/environments/installed/alpha/installations/[..]/node_modules/.bin/alpha"
        )
    );
    assert_that!(
        s.volta("which alpha"),
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
    manifest["package"]["requested"] = Value::String("alpha@tampered".to_owned());
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
        .setup_npm_binary("6.7.0", NODE)
        .pnpm_available_versions(PNPM_VERSION_INFO)
        .setup_pnpm_binary("7.7.1", PNPM)
        .add_dir_to_path(PathBuf::from("/bin"))
        .build();

    assert_that!(s.volta("tool install alpha"), execs().with_status(0));
    // Damage the global environment: resolving a direct project dependency
    // must still succeed without loading that environment's files.
    fs::remove_file(installed_environment("alpha").join("pnpm-lock.yaml"))
        .expect("remove global lockfile");
    assert_that!(
        s.exec_shim("alpha", ""),
        execs()
            .with_status(0)
            .with_stdout_contains("project-local")
            .with_stdout_does_not_contain("alpha@1.0.0")
    );
    assert_that!(
        s.volta("which alpha"),
        execs()
            .with_status(0)
            .with_stdout_contains("[..]node_modules/.bin/alpha")
            .with_stdout_does_not_contain("tools/environments")
    );
}

#[test]
fn defaults_to_global_node_and_allows_an_explicit_tool_runtime() {
    let s = sandbox()
        .layout_file("v4")
        .platform(PLATFORM)
        .package_json(r#"{"name":"project","volta":{"node":"10.99.1040","npm":"6.7.0"}}"#)
        .setup_node_binary("11.10.1", "6.7.0", NODE)
        .setup_node_binary("10.99.1040", "6.7.0", NODE)
        .pnpm_available_versions(PNPM_VERSION_INFO)
        .setup_pnpm_binary("7.7.1", PNPM)
        .add_dir_to_path(PathBuf::from("/bin"))
        .build();

    assert_that!(s.volta("tool install alpha"), execs().with_status(0));
    assert_that!(
        s.volta("tool list"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@1.0.0 (node@11.10.1, installed by pnpm@7.7.1) [alpha]")
    );

    assert_that!(
        s.volta("tool install beta --node 10.99.1040"),
        execs().with_status(0)
    );
    assert_that!(
        s.volta("tool list"),
        execs()
            .with_status(0)
            .with_stdout_contains("beta@1.0.0 (node@10.99.1040, installed by pnpm@7.7.1) [beta]")
    );
}

#[test]
fn which_reports_the_yarn_launcher_when_project_commands_delegate_to_yarn() {
    let s = sandbox()
        .layout_file("v4")
        .platform(PLATFORM)
        .package_json(r#"{"name":"project","dependencies":{"alpha":"1.0.0"},"volta":{"node":"11.10.1","yarn":"1.22.0"}}"#)
        .project_file(".pnp.js", "")
        .setup_node_binary("11.10.1", "6.7.0", NODE)
        .setup_yarn_binary("1.22.0", "#!/bin/sh\necho yarn-project-command\n")
        .pnpm_available_versions(PNPM_VERSION_INFO)
        .setup_pnpm_binary("7.7.1", PNPM)
        .add_dir_to_path(PathBuf::from("/bin"))
        .build();

    assert_that!(s.volta("tool install alpha@1"), execs().with_status(0));
    assert_that!(
        s.exec_shim("alpha", ""),
        execs()
            .with_status(0)
            .with_stdout_contains("yarn-project-command")
    );
    assert_that!(
        s.volta("which alpha"),
        execs()
            .with_status(0)
            .with_stdout_contains("[..]tools/image/yarn/1.22.0/bin/yarn")
    );
}

#[test]
fn upgrades_from_the_receipt_and_preserves_the_previous_install_on_failure() {
    let s = test_sandbox();

    assert_that!(
        s.volta("tool install alpha --allow-build esbuild"),
        execs().with_status(0)
    );
    assert_eq!(
        installed_manifest("alpha")["settings"]["allow_builds"][0],
        "esbuild"
    );
    assert!(test_support::paths::home()
        .join(".volta/store/pnpm/allow-build-esbuild")
        .is_file());
    assert_that!(s.volta("tool upgrade alpha"), execs().with_status(0));
    assert_that!(
        s.exec_shim("alpha", "upgraded"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@2.0.0 args: upgraded")
    );

    let fail_marker = test_support::paths::home().join(".volta/store/pnpm/fail-alpha");
    fs::write(fail_marker, "fail").expect("failure marker");
    assert_that!(s.volta("tool upgrade alpha"), execs().with_status(1));
    assert_that!(
        s.exec_shim("alpha", "still-current"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@2.0.0 args: still-current")
    );
}

#[test]
fn reinstall_replaces_package_contents_even_when_the_receipt_inputs_match() {
    let s = test_sandbox();

    assert_that!(s.volta("tool install alpha@1"), execs().with_status(0));
    let old_environment = installed_environment("alpha");
    fs::write(
        old_environment.join("node_modules/alpha/cli.sh"),
        "#!/bin/sh\necho stale-content\n",
    )
    .expect("modify installed package content without changing its launcher link");

    assert_that!(s.volta("tool install alpha@1"), execs().with_status(0));
    assert_that!(
        s.exec_shim("alpha", "rebuilt"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@1.0.0 args: rebuilt")
    );
    assert_ne!(old_environment, installed_environment("alpha"));
    assert!(!old_environment.exists());
}

// Closing stdin releases the fixture's wait even if an assertion panics.
struct RunningTool(Child);

impl Drop for RunningTool {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.wait();
    }
}

#[test]
fn updates_keep_running_tools_usable_and_collect_them_after_exit() {
    let s = test_sandbox();

    for update in ["tool install alpha@2", "tool upgrade alpha"] {
        for launch in ["shim", "tool run alpha", "run alpha"] {
            assert_that!(s.volta("tool install alpha@1"), execs().with_status(0));
            let old_environment = installed_environment("alpha");
            let command = if launch == "shim" {
                s.exec_shim("alpha", "--wait-for-update")
            } else {
                s.volta(&format!("{launch} --wait-for-update"))
            };
            let processes = (0..2)
                .map(|_| {
                    let mut running = RunningTool(
                        command
                            .build_command()
                            .stdin(Stdio::piped())
                            .stdout(Stdio::piped())
                            .spawn()
                            .expect("start installed tool"),
                    );
                    let mut output = BufReader::new(running.0.stdout.take().expect("tool stdout"));
                    let mut ready = String::new();
                    output.read_line(&mut ready).expect("tool ready output");
                    assert_eq!(ready.trim(), "alpha@1.0.0 args: --wait-for-update");
                    (running, output)
                })
                .collect::<Vec<_>>();

            assert_that!(s.volta(update), execs().with_status(0));
            assert_ne!(old_environment, installed_environment("alpha"));
            assert!(
                old_environment.exists(),
                "running tool's files must survive"
            );
            let version = installed_manifest("alpha")["package"]["resolved"]
                .as_str()
                .expect("current version")
                .to_owned();
            assert_that!(
                s.exec_shim("alpha", "new-process"),
                execs()
                    .with_status(0)
                    .with_stdout_contains(format!("alpha@{version} args: new-process"))
            );

            for (mut running, output) in processes {
                assert_that!(s.volta(update), execs().with_status(0));
                assert!(
                    old_environment.exists(),
                    "remaining process still needs its files"
                );
                writeln!(running.0.stdin.as_mut().expect("tool stdin"), "continue")
                    .expect("resume installed tool");
                let lines = output
                    .lines()
                    .collect::<Result<Vec<_>, _>>()
                    .expect("tool output");
                assert!(running.0.wait().expect("tool exit").success());
                assert_eq!(
                    lines,
                    ["alpha@1.0.0 args: after-update", "node args: --version"]
                );
            }

            assert_that!(s.volta(update), execs().with_status(0));
            assert!(
                !old_environment.exists(),
                "unused environment must be collected"
            );
        }
    }
}

#[test]
fn updates_preserve_environments_created_before_execution_tracking() {
    let s = test_sandbox();

    assert_that!(s.volta("tool install alpha@1"), execs().with_status(0));
    let old_environment = installed_environment("alpha");
    fs::remove_file(old_environment.join("volta-tool.lock"))
        .expect("simulate an installation created before execution tracking");
    assert_that!(
        s.exec_shim("alpha", "older-install"),
        execs().with_status(0)
    );
    assert_that!(s.volta("tool install alpha@2"), execs().with_status(0));
    assert_that!(s.volta("tool upgrade alpha"), execs().with_status(0));
    assert!(old_environment.exists());
    assert_that!(s.volta("tool uninstall alpha"), execs().with_status(0));
    assert!(!old_environment.exists());
}

#[test]
fn upgrade_repairs_missing_environment_files_from_the_intact_receipt() {
    let s = test_sandbox();

    assert_that!(s.volta("tool install alpha@1"), execs().with_status(0));
    let old_environment = installed_environment("alpha");
    fs::remove_file(old_environment.join("node_modules/.bin/alpha"))
        .expect("remove installed launcher");
    fs::remove_file(old_environment.join("pnpm-lock.yaml")).expect("remove installed lockfile");
    fs::remove_file(old_environment.join("runtime/node")).expect("remove runtime link");

    assert_that!(s.volta("tool upgrade alpha"), execs().with_status(0));
    assert_that!(
        s.exec_shim("alpha", "repaired"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@1.0.0 args: repaired")
    );
    assert_ne!(old_environment, installed_environment("alpha"));
    assert!(!old_environment.exists());
}

#[test]
fn runtime_removal_checks_receipts_even_when_environment_files_are_missing() {
    let s = test_sandbox();

    assert_that!(s.volta("tool install alpha@1"), execs().with_status(0));
    let environment = installed_environment("alpha");
    fs::remove_file(environment.join("pnpm-lock.yaml")).expect("remove installed lockfile");
    assert_that!(
        s.volta("uninstall node@11.10.1"),
        execs()
            .with_status(8)
            .with_stderr_contains("[..]used by isolated tools: alpha[..]")
    );

    fs::remove_file(environment.join("volta-tool.json")).expect("remove installed receipt");
    assert_that!(
        s.volta("uninstall node@11.10.1 --force"),
        execs().with_status(0)
    );
}

#[test]
fn upgrade_all_and_forced_node_removal_have_explicit_results() {
    let s = sandbox()
        .layout_file("v4")
        .platform(PLATFORM)
        .setup_node_binary("11.10.1", "6.7.0", NODE)
        .setup_node_binary("10.99.1040", "6.7.0", NODE)
        .pnpm_available_versions(PNPM_VERSION_INFO)
        .setup_pnpm_binary("7.7.1", PNPM)
        .add_dir_to_path(PathBuf::from("/bin"))
        .build();

    assert_that!(s.volta("tool install alpha"), execs().with_status(0));
    assert_that!(s.volta("tool install beta"), execs().with_status(0));
    assert_that!(
        s.volta("tool upgrade --all --node 10.99.1040"),
        execs().with_status(0)
    );
    assert_that!(
        s.volta("tool list"),
        execs()
            .with_status(0)
            .with_stdout_contains("alpha@2.0.0 (node@10.99.1040, installed by pnpm@7.7.1) [alpha]")
            .with_stdout_contains("beta@1.0.0 (node@10.99.1040, installed by pnpm@7.7.1) [beta]")
    );

    assert_that!(
        s.volta("uninstall node@10.99.1040"),
        execs()
            .with_status(8)
            .with_stderr_contains("[..]used by isolated tools: alpha, beta[..]")
    );
    assert_that!(
        s.volta("uninstall node@10.99.1040 --force"),
        execs().with_status(0)
    );
    assert_that!(
        s.volta("tool list"),
        execs().with_status(0).with_stdout_contains(
            "alpha@2.0.0 (node@10.99.1040, installed by pnpm@7.7.1) [alpha] BROKEN: missing Node runtime"
        )
    );
    assert_that!(
        s.volta("tool run alpha"),
        execs().with_status(8).with_stderr_contains(
            "[..]requires node@10.99.1040, but that runtime is not installed[..]"
        )
    );
}
