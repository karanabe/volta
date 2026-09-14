//! Per-tool isolated environments for JavaScript CLI packages.
//!
//! pnpm owns dependency resolution, its content-addressed store, and the
//! `node_modules` link topology. Volta owns the immutable environment receipt,
//! exact Node and pnpm selections, command registration, and atomic publish.

use std::fs::{self, File};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use log::{debug, info, warn};
use node_semver::Version;
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};
use validate_npm_package_name::{validate, Validity};

use self::manifest::{
    Executable, InstallSettings, InstallerKind, InstallerSelection, NpmBins, NpmPackageManifest,
    PackageSelection, RuntimeSelection, ToolManifest, LOCKFILE, MANIFEST_FILE, RUNTIME_LINK,
};
use self::registry::{RegisteredCommand, RegisteredTool, ToolRegistry};
use super::check_shim_reachable;
use super::package::BinConfig;
use super::{Node, Pnpm};
use crate::command::create_command;
use crate::error::{ErrorKind, Fallible};
use crate::fs::{remove_dir_if_exists, rename, symlink_dir};
use crate::inventory::node_available;
use crate::layout::volta_home;
use crate::platform::Sourced;
use crate::session::Session;
use crate::shim::{self, ShimResult};
use crate::style::{progress_spinner, success_prefix, tool_version};
use crate::sync::VoltaLock;
use crate::version::VersionSpec;

mod manifest;
mod registry;

pub use manifest::InstalledTool;

const RESERVED_COMMANDS: &[&str] = &[
    "node",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "yarnpkg",
    "volta",
    "volta-migrate",
    "volta-shim",
];

/// A validated npm-style package specification for an isolated CLI tool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolPackageSpec {
    raw: String,
    name: String,
}

impl ToolPackageSpec {
    pub fn parse(raw: impl Into<String>) -> Fallible<Self> {
        let raw = raw.into();
        let (name, version) = split_package_spec(&raw)?;
        validate_package_name(name)?;
        if version.is_some_and(str::is_empty) {
            return Err(ErrorKind::ParseToolSpecError { tool_spec: raw }.into());
        }
        Ok(Self {
            name: name.to_owned(),
            raw,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn requested(&self) -> &str {
        &self.raw
    }
}

/// Settings that affect pnpm's materialization of an isolated tool.
#[derive(Debug, Default)]
pub struct InstallOptions {
    pub node: Option<VersionSpec>,
    pub allow_builds: Vec<String>,
}

/// A command resolved from the isolated-tool registry.
pub(crate) struct ResolvedToolCommand {
    pub(crate) path: PathBuf,
    pub(crate) runtime_bin: PathBuf,
}

/// A registry lookup that has not loaded or validated its environment yet.
pub(crate) struct ToolCommandRegistration {
    command: String,
    package: String,
    installation: String,
}

impl ToolCommandRegistration {
    pub(crate) fn package(&self) -> &str {
        &self.package
    }

    pub(crate) fn resolve(self) -> Fallible<ResolvedToolCommand> {
        ToolEnvironment::load(&self.package, &self.installation)?.resolve(&self.command)
    }
}

struct ToolEnvironment {
    root: PathBuf,
    manifest: ToolManifest,
}

impl ToolEnvironment {
    fn load(package: &str, installation: &str) -> Fallible<Self> {
        let environment = Self::load_receipt(package, installation)?;
        validate_lockfile(&environment.manifest, &environment.root)?;
        validate_executable_files(&environment.manifest, &environment.root)?;
        validate_runtime_reference(&environment.manifest, &environment.root)?;
        Ok(environment)
    }

    // Rebuilding only needs the receipt: missing launchers, lockfiles, or runtime
    // links must not prevent an upgrade from repairing an installation.
    fn load_receipt(package: &str, installation: &str) -> Fallible<Self> {
        validate_registry_reference(package, installation)?;
        let root = volta_home()?.tool_installation_dir(package, installation);
        let manifest = ToolManifest::read(&root.join(MANIFEST_FILE))?;
        if manifest.package.name != package || manifest.installation_id != installation {
            return Err(corrupt(
                package,
                "registry and environment manifest do not agree",
            ));
        }
        validate_installation_id(&manifest)?;
        Ok(Self { root, manifest })
    }

    fn runtime_available(&self) -> Fallible<bool> {
        node_available(&self.manifest.runtime.resolved)
    }

    fn resolve(self, command: &str) -> Fallible<ResolvedToolCommand> {
        if !self.runtime_available()? {
            return Err(ErrorKind::ToolRuntimeMissing {
                package: self.manifest.package.name.clone(),
                version: self.manifest.runtime.resolved.to_string(),
            }
            .into());
        }
        let executable = self
            .manifest
            .executables
            .iter()
            .find(|executable| executable.name == command)
            .ok_or_else(|| {
                corrupt(
                    &self.manifest.package.name,
                    "registered executable is absent from manifest",
                )
            })?;
        Ok(ResolvedToolCommand {
            path: self.root.join(&executable.path),
            runtime_bin: self.runtime_bin(),
        })
    }

    fn runtime_bin(&self) -> PathBuf {
        let runtime = self.root.join(RUNTIME_LINK);
        if cfg!(unix) {
            runtime.join("bin")
        } else {
            runtime
        }
    }
}

/// Install or atomically replace one isolated JavaScript CLI tool.
pub fn install(
    spec: ToolPackageSpec,
    options: InstallOptions,
    session: &mut Session,
) -> Fallible<InstalledTool> {
    let runtime = resolve_new_runtime(options.node, session)?;
    let pnpm_version = super::pnpm::resolve(VersionSpec::None, session)?;
    install_resolved(
        spec,
        runtime,
        InstallerSelection {
            kind: InstallerKind::Pnpm,
            version: pnpm_version,
        },
        InstallSettings {
            allow_builds: normalized_allow_builds(options.allow_builds)?,
        },
        "installed",
        session,
    )
}

/// Rebuild an installed tool from its durable receipt.
pub fn upgrade(
    package: &str,
    node: Option<VersionSpec>,
    session: &mut Session,
) -> Fallible<InstalledTool> {
    validate_package_name(package)?;
    let _lock = VoltaLock::acquire()?;
    let home = volta_home()?;
    let registry = ToolRegistry::read(home.tool_registry_file())?;
    let registered = registry
        .tools
        .get(package)
        .ok_or_else(|| ErrorKind::ToolNotInstalled {
            tool: package.to_owned(),
        })?;
    let current = ToolEnvironment::load_receipt(package, &registered.installation)?;
    let spec = ToolPackageSpec::parse(current.manifest.package.requested.clone())?;
    let runtime = match node {
        Some(requested) => resolve_runtime(requested, session)?,
        None => current.manifest.runtime.clone(),
    };
    install_resolved(
        spec,
        runtime,
        current.manifest.installer.clone(),
        current.manifest.settings.clone(),
        "upgraded",
        session,
    )
}

/// Return all package identities registered as isolated tools.
pub fn installed_package_names() -> Fallible<Vec<String>> {
    let home = volta_home()?;
    Ok(ToolRegistry::read(home.tool_registry_file())?
        .tools
        .keys()
        .cloned()
        .collect())
}

/// Return isolated tools whose receipts refer to an exact Node version.
pub fn tools_using_node(version: &Version) -> Fallible<Vec<String>> {
    let home = volta_home()?;
    let registry = ToolRegistry::read(home.tool_registry_file())?;
    registry
        .tools
        .iter()
        .filter_map(|(package, registered)| {
            match ToolEnvironment::load_receipt(package, &registered.installation) {
                Ok(environment) if environment.manifest.runtime.resolved == *version => {
                    Some(Ok(package.clone()))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}

fn install_resolved(
    spec: ToolPackageSpec,
    runtime: RuntimeSelection,
    installer: InstallerSelection,
    settings: InstallSettings,
    action: &str,
    session: &mut Session,
) -> Fallible<InstalledTool> {
    let _lock = VoltaLock::acquire()?;
    Node::new(runtime.resolved.clone()).ensure_fetched(session)?;
    match installer.kind {
        InstallerKind::Pnpm => Pnpm::new(installer.version.clone()).ensure_fetched(session)?,
    }

    let home = volta_home()?;
    let staging_root = home.tmp_dir().join("tool-environments");
    fs::create_dir_all(&staging_root)
        .map_err(|_| environment_metadata_error("create", &staging_root))?;
    let staging = Builder::new()
        .prefix("tool-")
        .rand_bytes(16)
        .tempdir_in(&staging_root)
        .map_err(|_| environment_metadata_error("create", &staging_root))?;

    write_environment_package_json(staging.path())?;
    create_runtime_reference(staging.path(), &runtime.resolved)?;
    run_pnpm_install(
        &spec,
        &runtime.resolved,
        &installer.version,
        &settings,
        staging.path(),
    )?;

    let package_root = package_directory(staging.path(), spec.name());
    let root_manifest = NpmPackageManifest::read(spec.name(), &package_root)?;
    if root_manifest.name != spec.name() {
        return Err(corrupt(
            spec.name(),
            "pnpm resolved a package with a different identity",
        ));
    }
    let resolved_version = Version::parse(&root_manifest.version).map_err(|_| {
        ErrorKind::PackageManifestParseError {
            package: spec.name().to_owned(),
        }
    })?;
    let executables = discover_executables(&spec, staging.path(), &package_root, &root_manifest)?;

    let registry_path = home.tool_registry_file();
    let mut registry = ToolRegistry::read(registry_path)?;
    validate_command_conflicts(spec.name(), &executables, &registry)?;

    let mut manifest = ToolManifest {
        schema_version: 2,
        installation_id: String::new(),
        generation: Some(
            staging
                .path()
                .file_name()
                .expect("staging directory has a generated name")
                .to_string_lossy()
                .into_owned(),
        ),
        package: PackageSelection {
            requested: spec.requested().to_owned(),
            name: spec.name().to_owned(),
            resolved: resolved_version,
        },
        runtime,
        installer,
        settings,
        executables,
        lockfile_integrity: file_integrity(spec.name(), &staging.path().join(LOCKFILE))?,
    };
    manifest.installation_id = installation_id(&manifest)?;
    manifest.write(staging.path())?;

    publish_and_register(
        spec.name(),
        staging,
        manifest,
        &mut registry,
        registry_path,
        action,
    )
}

fn publish_and_register(
    package: &str,
    staging: TempDir,
    manifest: ToolManifest,
    registry: &mut ToolRegistry,
    registry_path: &Path,
    action: &str,
) -> Fallible<InstalledTool> {
    let home = volta_home()?;
    let final_dir = home.tool_installation_dir(package, &manifest.installation_id);
    publish_environment(staging, &final_dir)?;
    let old_installation = registry
        .tools
        .get(package)
        .map(|registered| registered.installation.clone());
    let old_commands = registry
        .tools
        .get(package)
        .map(|registered| registered.executables.clone())
        .unwrap_or_default();

    registry
        .commands
        .retain(|_, command| command.package != package);
    let command_names = manifest
        .executables
        .iter()
        .map(|executable| executable.name.clone())
        .collect::<Vec<_>>();
    registry.tools.insert(
        package.to_owned(),
        RegisteredTool {
            installation: manifest.installation_id.clone(),
            executables: command_names.clone(),
        },
    );
    for command in &command_names {
        registry.commands.insert(
            command.clone(),
            RegisteredCommand {
                package: package.to_owned(),
                installation: manifest.installation_id.clone(),
            },
        );
    }

    let mut created_shims = Vec::new();
    for command in &command_names {
        if fs::symlink_metadata(home.shim_file(command)).is_ok() {
            continue;
        }
        match shim::create(command) {
            Ok(ShimResult::Created) => created_shims.push(command.clone()),
            Ok(ShimResult::AlreadyExists) => {}
            Ok(ShimResult::Deleted | ShimResult::DoesntExist) => {
                unreachable!("shim creation cannot report deletion")
            }
            Err(error) => {
                let _ = shim::delete(command);
                rollback_shims(&created_shims);
                let _ = remove_environment(&final_dir);
                return Err(error);
            }
        }
    }
    if let Err(error) = registry.write(registry_path) {
        rollback_shims(&created_shims);
        let _ = remove_environment(&final_dir);
        return Err(error);
    }

    remove_stale_shims(&old_commands, &command_names);
    if let Some(old) = old_installation {
        if old != manifest.installation_id {
            let old_dir = home.tool_installation_dir(package, &old);
            if let Err(error) = remove_environment(&old_dir) {
                warn!(
                    "Unable to remove superseded tool environment at {}: {}",
                    old_dir.display(),
                    error
                );
            }
        }
    }

    for command in &command_names {
        check_shim_reachable(command);
    }
    info!(
        "{} {} {} with executables: {}",
        success_prefix(),
        action,
        tool_version(package, &manifest.package.resolved),
        command_names.join(", ")
    );
    let environment = ToolEnvironment {
        root: final_dir,
        manifest,
    };
    Ok(InstalledTool::from_manifest(
        &environment.manifest,
        environment.runtime_available()?,
    ))
}

/// Uninstall a current isolated tool environment without touching legacy installs.
pub fn uninstall(package: &str) -> Fallible<()> {
    validate_package_name(package)?;
    let _lock = VoltaLock::acquire()?;
    let home = volta_home()?;
    let registry_path = home.tool_registry_file();
    let mut registry = ToolRegistry::read(registry_path)?;
    let registered = registry
        .tools
        .remove(package)
        .ok_or_else(|| ErrorKind::ToolNotInstalled {
            tool: package.to_owned(),
        })?;
    registry
        .commands
        .retain(|_, command| command.package != package);
    registry.write(registry_path)?;

    for command in &registered.executables {
        if !legacy_bin_exists(command) {
            shim::delete(command)?;
        }
    }
    remove_environment(&home.tool_environment_dir(package))?;
    info!("{} tool '{}' uninstalled", success_prefix(), package);
    Ok(())
}

/// Return every installed isolated tool, sorted by package identity.
pub fn list() -> Fallible<Vec<InstalledTool>> {
    let home = volta_home()?;
    let registry = ToolRegistry::read(home.tool_registry_file())?;
    registry
        .tools
        .iter()
        .map(|(package, registered)| {
            let environment = ToolEnvironment::load(package, &registered.installation)?;
            let runtime_available = environment.runtime_available()?;
            Ok(InstalledTool::from_manifest(
                &environment.manifest,
                runtime_available,
            ))
        })
        .collect()
}

/// Resolve an installed command and return its concrete environment launcher path.
pub fn which(command: &str) -> Fallible<PathBuf> {
    resolve_command(command)?
        .map(|resolved| resolved.path)
        .ok_or_else(|| {
            ErrorKind::ToolNotInstalled {
                tool: command.to_owned(),
            }
            .into()
        })
}

pub(crate) fn resolve_command(command: &str) -> Fallible<Option<ResolvedToolCommand>> {
    lookup_command(command)?
        .map(ToolCommandRegistration::resolve)
        .transpose()
}

pub(crate) fn lookup_command(command: &str) -> Fallible<Option<ToolCommandRegistration>> {
    let home = volta_home()?;
    let registry = ToolRegistry::read(home.tool_registry_file())?;
    let command = command_key(command);
    let Some(registered) = registry.commands.get(&command) else {
        return Ok(None);
    };
    Ok(Some(ToolCommandRegistration {
        command,
        package: registered.package.clone(),
        installation: registered.installation.clone(),
    }))
}

pub(crate) fn resolve_selector(selector: &str) -> Fallible<ResolvedToolCommand> {
    let home = volta_home()?;
    let registry = ToolRegistry::read(home.tool_registry_file())?;

    if let Some(tool) = registry.tools.get(selector) {
        let command = if tool.executables.len() == 1 {
            tool.executables[0].clone()
        } else {
            let conventional = command_key(unscoped_name(selector));
            match tool
                .executables
                .iter()
                .find(|command| command.as_str() == conventional.as_str())
            {
                Some(command) => command.clone(),
                None => {
                    return Err(ErrorKind::ToolRunAmbiguous {
                        package: selector.to_owned(),
                        executables: tool.executables.clone(),
                    }
                    .into());
                }
            }
        };
        let registered = registry
            .commands
            .get(&command)
            .ok_or_else(|| corrupt(selector, "command registry entry is missing"))?;
        return resolve_registered_command(&command, registered);
    }

    let command = command_key(selector);
    match registry.commands.get(&command) {
        Some(registered) => resolve_registered_command(&command, registered),
        None => Err(ErrorKind::ToolNotInstalled {
            tool: selector.to_owned(),
        }
        .into()),
    }
}

fn resolve_new_runtime(
    requested: Option<VersionSpec>,
    session: &mut Session,
) -> Fallible<RuntimeSelection> {
    match requested {
        Some(requested) => resolve_runtime(requested, session),
        None => {
            let resolved = session
                .default_platform()?
                .map(|platform| platform.node.clone())
                .ok_or(ErrorKind::NoPlatform)?;
            Ok(RuntimeSelection {
                requested: resolved.to_string(),
                resolved,
            })
        }
    }
}

fn resolve_runtime(requested: VersionSpec, session: &mut Session) -> Fallible<RuntimeSelection> {
    let request = requested.to_string();
    let resolved = super::node::resolve(requested, session)?;
    Ok(RuntimeSelection {
        requested: request,
        resolved,
    })
}

fn normalized_allow_builds(mut packages: Vec<String>) -> Fallible<Vec<String>> {
    for package in &packages {
        validate_package_name(package)?;
    }
    packages.sort();
    packages.dedup();
    Ok(packages)
}

fn create_runtime_reference(environment: &Path, version: &Version) -> Fallible<()> {
    let home = volta_home()?;
    let link = environment.join(RUNTIME_LINK);
    let parent = link.parent().expect("runtime link has a parent");
    fs::create_dir_all(parent).map_err(|_| environment_metadata_error("create", parent))?;
    symlink_dir(home.node_image_dir(&version.to_string()), &link)
        .map_err(|_| environment_metadata_error("link", &link))
}

fn write_environment_package_json(root: &Path) -> Fallible<()> {
    let path = root.join("package.json");
    let file = File::create(&path).map_err(|_| environment_metadata_error("write", &path))?;
    serde_json::to_writer_pretty(
        file,
        &serde_json::json!({
            "name": "volta-tool-environment",
            "private": true,
            "version": "0.0.0"
        }),
    )
    .map_err(|_| environment_metadata_error("serialize", &path))
}

fn run_pnpm_install(
    spec: &ToolPackageSpec,
    node: &Version,
    pnpm: &Version,
    settings: &InstallSettings,
    staging: &Path,
) -> Fallible<()> {
    let home = volta_home()?;
    let image = crate::platform::Image {
        node: Sourced::with_binary(node.clone()),
        npm: None,
        pnpm: Some(Sourced::with_binary(pnpm.clone())),
        yarn: None,
    };
    let store = home.pnpm_store_dir();
    let pnpm_home = home.pnpm_home_dir();
    fs::create_dir_all(pnpm_home).map_err(|_| environment_metadata_error("create", pnpm_home))?;

    let mut command = create_command("pnpm");
    command.arg("--reporter=append-only");
    command.arg(format!("--store-dir={}", store.display()));
    command.arg("add");
    for package in &settings.allow_builds {
        command.arg(format!("--allow-build={package}"));
    }
    command.args(["--save-exact", "--ignore-workspace-root-check"]);
    command.arg(spec.requested());
    command.current_dir(staging);
    command.env("PATH", image.path()?);
    command.env("PNPM_HOME", pnpm_home);
    // pnpm 10 reads npm_config_* while newer pnpm versions prefer
    // pnpm_config_*. Set both so the isolated layout does not depend on user
    // configuration; the supported store-dir CLI flag remains authoritative.
    command.env("npm_config_store_dir", store);
    command.env("pnpm_config_store_dir", store);
    command.env("npm_config_node_linker", "isolated");
    command.env("pnpm_config_node_linker", "isolated");
    command.env("npm_config_virtual_store_dir", "node_modules/.pnpm");
    command.env("pnpm_config_virtual_store_dir", "node_modules/.pnpm");
    command.env("npm_config_enable_global_virtual_store", "false");
    command.env("pnpm_config_enable_global_virtual_store", "false");
    command.env("pnpm_config_manage_package_manager_versions", "false");
    command.env("COREPACK_ENABLE_PROJECT_SPEC", "0");
    command.env_remove("npm_config_global");
    command.env_remove("npm_config_location");
    command.env_remove("npm_config_prefix");

    debug!("Installing isolated tool with command: {:?}", command);
    let spinner = progress_spinner(format!("Installing {}", spec.requested()));
    let output = command
        .output()
        .map_err(|_| ErrorKind::PackageInstallFailed {
            package: spec.requested().to_owned(),
        })?;
    spinner.finish_and_clear();

    let stderr = String::from_utf8_lossy(&output.stderr);
    debug!("[tool install stderr]\n{}", stderr);
    debug!(
        "[tool install stdout]\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    if output.status.success() {
        Ok(())
    } else if stderr.contains("ERR_PNPM_FETCH_404") || stderr.contains("404 Not Found") {
        Err(ErrorKind::PackageNotFound {
            package: spec.requested().to_owned(),
        }
        .into())
    } else {
        Err(ErrorKind::PackageInstallFailed {
            package: spec.requested().to_owned(),
        }
        .into())
    }
}

fn package_directory(environment: &Path, package: &str) -> PathBuf {
    environment.join("node_modules").join(package)
}

fn discover_executables(
    spec: &ToolPackageSpec,
    environment: &Path,
    package_root: &Path,
    manifest: &NpmPackageManifest,
) -> Fallible<Vec<Executable>> {
    let bins = match &manifest.bin {
        None => Vec::new(),
        Some(NpmBins::Single(path)) => vec![(unscoped_name(&manifest.name).to_owned(), path)],
        Some(NpmBins::Multiple(bins)) => bins
            .iter()
            .map(|(name, path)| (name.clone(), path))
            .collect(),
    };
    if bins.is_empty() {
        return Err(ErrorKind::ToolHasNoExecutables {
            package: spec.name().to_owned(),
        }
        .into());
    }

    let package_root_canonical = fs::canonicalize(package_root).map_err(|_| {
        corrupt(
            spec.name(),
            "pnpm did not create the requested package directory",
        )
    })?;
    let mut executables = Vec::with_capacity(bins.len());
    for (name, declared_path) in bins {
        validate_bin_name(spec.name(), &name)?;
        let declared = safe_relative_path(spec.name(), declared_path)?;
        let target = fs::canonicalize(package_root.join(declared)).map_err(|_| {
            corrupt(
                spec.name(),
                &format!("executable '{}' points to a missing file", name),
            )
        })?;
        if !target.starts_with(&package_root_canonical) || !target.is_file() {
            return Err(corrupt(
                spec.name(),
                &format!("executable '{}' escapes the package directory", name),
            ));
        }

        let relative = environment_bin_path(&name);
        let launcher = environment.join(&relative);
        if !launcher.is_file() {
            return Err(corrupt(
                spec.name(),
                &format!("pnpm did not create the '{}' executable", name),
            ));
        }
        executables.push(Executable {
            name: command_key(&name),
            path: relative,
            integrity: executable_integrity(spec.name(), &launcher)?,
        });
    }
    executables.sort_by(|left, right| left.name.cmp(&right.name));
    executables.dedup_by(|left, right| left.name == right.name);
    Ok(executables)
}

fn validate_bin_name(package: &str, name: &str) -> Fallible<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(&['/', '\\'][..])
        || name.chars().any(char::is_control)
    {
        Err(corrupt(
            package,
            "package contains an invalid executable name",
        ))
    } else {
        Ok(())
    }
}

fn safe_relative_path<'a>(package: &str, path: &'a str) -> Fallible<&'a Path> {
    let path = Path::new(path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        Err(corrupt(
            package,
            "package contains an unsafe executable path",
        ))
    } else {
        Ok(path)
    }
}

fn environment_bin_path(name: &str) -> PathBuf {
    let path = PathBuf::from("node_modules").join(".bin");
    #[cfg(windows)]
    return path.join(format!("{}.cmd", name));
    #[cfg(unix)]
    return path.join(name);
}

fn file_integrity(package: &str, path: &Path) -> Fallible<String> {
    let mut file = File::open(path).map_err(|_| {
        corrupt(
            package,
            &format!("required file '{}' is missing", path.display()),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"volta-tool-file-v1\0");
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| corrupt(package, "could not read an environment file"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn executable_integrity(package: &str, path: &Path) -> Fallible<String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| corrupt(package, "could not inspect an executable launcher"))?;
    let mut hasher = Sha256::new();
    hasher.update(b"volta-tool-executable-v1\0");

    if metadata.file_type().is_symlink() {
        hasher.update(b"symlink\0");
        let target = fs::read_link(path)
            .map_err(|_| corrupt(package, "could not inspect an executable launcher"))?;
        let target = target
            .to_str()
            .ok_or_else(|| corrupt(package, "executable link target is not valid UTF-8"))?;
        hasher.update((target.len() as u64).to_le_bytes());
        hasher.update(target.as_bytes());
    } else if metadata.is_file() {
        hasher.update(b"file\0");
        hasher.update(metadata.len().to_le_bytes());
        let mut file = File::open(path)
            .map_err(|_| corrupt(package, "could not read an executable launcher"))?;
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|_| corrupt(package, "could not read an executable launcher"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        #[cfg(unix)]
        hasher.update([u8::from(metadata.permissions().mode() & 0o111 != 0)]);
    } else {
        return Err(corrupt(
            package,
            "executable launcher has an unsupported filesystem type",
        ));
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn installation_id(manifest: &ToolManifest) -> Fallible<String> {
    let mut identity = manifest.clone();
    identity.installation_id.clear();
    let bytes = serde_json::to_vec(&identity).map_err(|_| {
        environment_metadata_error(
            "serialize",
            Path::new("isolated tool installation identifier"),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_installation_id(manifest: &ToolManifest) -> Fallible<()> {
    if installation_id(manifest)? == manifest.installation_id {
        Ok(())
    } else {
        Err(corrupt(
            &manifest.package.name,
            "environment manifest does not match its installation identifier",
        ))
    }
}

fn validate_lockfile(manifest: &ToolManifest, root: &Path) -> Fallible<()> {
    if file_integrity(&manifest.package.name, &root.join(LOCKFILE))? == manifest.lockfile_integrity
    {
        Ok(())
    } else {
        Err(corrupt(
            &manifest.package.name,
            "pnpm lockfile failed integrity verification",
        ))
    }
}

fn validate_runtime_reference(manifest: &ToolManifest, root: &Path) -> Fallible<()> {
    let link = root.join(RUNTIME_LINK);
    fs::symlink_metadata(&link)
        .map_err(|_| corrupt(&manifest.package.name, "runtime reference is missing"))?;

    #[cfg(unix)]
    {
        let expected = volta_home()?
            .node_image_dir(&manifest.runtime.resolved.to_string())
            .to_owned();
        let actual = fs::read_link(&link)
            .map_err(|_| corrupt(&manifest.package.name, "runtime reference is not a link"))?;
        if actual != expected {
            return Err(corrupt(
                &manifest.package.name,
                "runtime reference points to a different Node version",
            ));
        }
    }
    Ok(())
}

fn publish_environment(staging: TempDir, destination: &Path) -> Fallible<()> {
    let parent = destination
        .parent()
        .expect("tool installation path has a parent");
    fs::create_dir_all(parent).map_err(|_| environment_metadata_error("create", parent))?;

    // Each build has its own generation, so publication must never reuse or
    // replace another environment. Keep TempDir armed to clean up failed renames.
    if fs::symlink_metadata(destination).is_ok() {
        return Err(environment_metadata_error("publish", destination));
    }
    rename(staging.path(), destination)
        .map_err(|_| environment_metadata_error("publish", destination))
}

fn validate_command_conflicts(
    package: &str,
    executables: &[Executable],
    registry: &ToolRegistry,
) -> Fallible<()> {
    let home = volta_home()?;
    for executable in executables {
        let command = &executable.name;
        if RESERVED_COMMANDS.contains(&command.as_str()) {
            return Err(binary_conflict(command, "a Volta runtime command", package));
        }
        if let Some(existing) = registry.commands.get(command) {
            if existing.package != package {
                return Err(binary_conflict(command, &existing.package, package));
            }
            continue;
        }
        if let Some(legacy) = BinConfig::from_file_if_exists(home.default_tool_bin_config(command))?
        {
            return Err(binary_conflict(
                command,
                &format!("legacy package {}", legacy.package),
                package,
            ));
        }
        if fs::symlink_metadata(home.shim_file(command)).is_ok() {
            return Err(binary_conflict(command, "an existing shim", package));
        }
    }
    Ok(())
}

fn binary_conflict(command: &str, existing: &str, package: &str) -> crate::error::VoltaError {
    ErrorKind::BinaryAlreadyInstalled {
        bin_name: command.to_owned(),
        existing_package: existing.to_owned(),
        new_package: package.to_owned(),
    }
    .into()
}

fn rollback_shims(commands: &[String]) {
    for command in commands {
        let _ = shim::delete(command);
    }
}

fn remove_stale_shims(old: &[String], new: &[String]) {
    for command in old {
        if !new.contains(command) && !legacy_bin_exists(command) {
            if let Err(error) = shim::delete(command) {
                warn!("Unable to remove stale shim '{}': {}", command, error);
            }
        }
    }
}

fn remove_environment(path: &Path) -> Fallible<()> {
    #[cfg(windows)]
    make_tree_writable(path)?;
    remove_dir_if_exists(path)
}

#[cfg(windows)]
fn make_tree_writable(path: &Path) -> Fallible<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path)
        .map_err(|_| environment_metadata_error("inspect", path))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| environment_metadata_error("inspect", path))?
    {
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path)
            .map_err(|_| environment_metadata_error("inspect", &entry_path))?;
        if metadata.is_dir() {
            make_tree_writable(&entry_path)?;
        } else if metadata.is_file() {
            let mut permissions = metadata.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(&entry_path, permissions)
                .map_err(|_| environment_metadata_error("update permissions for", &entry_path))?;
        }
    }
    Ok(())
}

fn legacy_bin_exists(command: &str) -> bool {
    volta_home()
        .ok()
        .and_then(|home| BinConfig::from_file_if_exists(home.default_tool_bin_config(command)).ok())
        .flatten()
        .is_some()
}

fn validate_executable_files(manifest: &ToolManifest, root: &Path) -> Fallible<()> {
    for executable in &manifest.executables {
        if safe_relative_path(
            &manifest.package.name,
            executable.path.to_string_lossy().as_ref(),
        )? != executable.path
        {
            return Err(corrupt(
                &manifest.package.name,
                "manifest contains an unsafe executable path",
            ));
        }
        if !root.join(&executable.path).is_file() {
            return Err(corrupt(
                &manifest.package.name,
                &format!("executable '{}' is missing", executable.name),
            ));
        }
        if executable_integrity(&manifest.package.name, &root.join(&executable.path))?
            != executable.integrity
        {
            return Err(corrupt(
                &manifest.package.name,
                &format!(
                    "executable '{}' failed integrity verification",
                    executable.name
                ),
            ));
        }
    }
    Ok(())
}

fn resolve_registered_command(
    command: &str,
    registered: &RegisteredCommand,
) -> Fallible<ResolvedToolCommand> {
    ToolEnvironment::load(&registered.package, &registered.installation)?.resolve(command)
}

fn validate_registry_reference(package: &str, installation: &str) -> Fallible<()> {
    let valid_package = validate(package).valid_for_old_packages();
    let valid_installation = installation.len() == 64
        && installation
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid_package && valid_installation {
        Ok(())
    } else {
        Err(corrupt(
            "tool registry",
            "registry contains an unsafe environment reference",
        ))
    }
}

fn split_package_spec(raw: &str) -> Fallible<(&str, Option<&str>)> {
    if raw.is_empty() {
        return Err(ErrorKind::ParseToolSpecError {
            tool_spec: raw.to_owned(),
        }
        .into());
    }
    if raw.starts_with('@') {
        let slash = raw.find('/').ok_or_else(|| ErrorKind::ParseToolSpecError {
            tool_spec: raw.to_owned(),
        })?;
        if let Some(relative_at) = raw[slash + 1..].rfind('@') {
            let at = slash + 1 + relative_at;
            return Ok((&raw[..at], Some(&raw[at + 1..])));
        }
        Ok((raw, None))
    } else if let Some((name, version)) = raw.rsplit_once('@') {
        Ok((name, Some(version)))
    } else {
        Ok((raw, None))
    }
}

fn validate_package_name(name: &str) -> Fallible<()> {
    match validate(name) {
        Validity::Valid | Validity::ValidForOldPackages { .. } => Ok(()),
        Validity::Invalid { errors, .. } => Err(ErrorKind::InvalidToolName {
            name: name.to_owned(),
            errors,
        }
        .into()),
    }
}

fn unscoped_name(package: &str) -> &str {
    package.rsplit_once('/').map_or(package, |(_, name)| name)
}

#[cfg(unix)]
fn command_key(command: &str) -> String {
    command.to_owned()
}

#[cfg(windows)]
fn command_key(command: &str) -> String {
    command.to_ascii_lowercase()
}

fn environment_metadata_error(operation: &str, path: &Path) -> crate::error::VoltaError {
    ErrorKind::ToolMetadataError {
        operation: operation.to_owned(),
        path: path.to_owned(),
    }
    .into()
}

fn corrupt(package: &str, reason: &str) -> crate::error::VoltaError {
    ErrorKind::ToolEnvironmentCorrupt {
        package: package.to_owned(),
        reason: reason.to_owned(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipts_without_a_generation_keep_their_original_identity() {
        let legacy = concat!(
            r#"{"schema_version":2,"installation_id":"","package":{"requested":"alpha@1","name":"alpha","resolved":"1.0.0"},"#,
            r#""runtime":{"requested":"22.0.0","resolved":"22.0.0"},"installer":{"kind":"pnpm","version":"10.0.0"},"#,
            r#""settings":{},"executables":[],"lockfile_integrity":"sha256:example"}"#,
        );
        let mut manifest: ToolManifest = serde_json::from_str(legacy).expect("legacy receipt");
        assert!(manifest.generation.is_none());
        assert_eq!(serde_json::to_string(&manifest).unwrap(), legacy);
        manifest.installation_id = format!("{:x}", Sha256::digest(legacy.as_bytes()));
        validate_installation_id(&manifest).expect("original receipt identity must remain valid");
    }

    #[test]
    fn parses_unscoped_and_scoped_specs() {
        assert_eq!(
            ToolPackageSpec::parse("eslint@9").expect("valid spec"),
            ToolPackageSpec {
                raw: "eslint@9".into(),
                name: "eslint".into(),
            }
        );
        assert_eq!(
            ToolPackageSpec::parse("@scope/package@1.2.3").expect("valid spec"),
            ToolPackageSpec {
                raw: "@scope/package@1.2.3".into(),
                name: "@scope/package".into(),
            }
        );
        assert_eq!(
            ToolPackageSpec::parse("@scope/package").expect("valid spec"),
            ToolPackageSpec {
                raw: "@scope/package".into(),
                name: "@scope/package".into(),
            }
        );
    }

    #[test]
    fn rejects_empty_versions_and_invalid_names() {
        assert!(ToolPackageSpec::parse("eslint@").is_err());
        assert!(ToolPackageSpec::parse("../eslint").is_err());
        assert!(ToolPackageSpec::parse("@scope").is_err());
    }

    #[test]
    fn normalizes_allowed_build_packages() {
        assert_eq!(
            normalized_allow_builds(vec!["zod".into(), "esbuild".into(), "zod".into()])
                .expect("valid packages"),
            vec!["esbuild", "zod"]
        );
    }

    #[test]
    fn executable_path_uses_the_platform_launcher_format() {
        #[cfg(unix)]
        assert_eq!(
            environment_bin_path("eslint"),
            PathBuf::from("node_modules/.bin/eslint")
        );
        #[cfg(windows)]
        assert_eq!(
            environment_bin_path("eslint"),
            PathBuf::from(r"node_modules\.bin\eslint.cmd")
        );
    }
}
