//! Per-tool isolated environments for JavaScript CLI packages.
//!
//! npm is used only to resolve and initially materialize a dependency graph.
//! This module then records that graph, interns each package's immutable files
//! in Volta's content-addressed package store, and publishes the complete
//! environment atomically. The store never owns or interprets Node resolution
//! topology.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use log::{debug, info, warn};
use node_semver::Version;
use sha2::{Digest, Sha256};
use tempfile::{tempdir_in, TempDir};
use validate_npm_package_name::{validate, Validity};

use self::manifest::{
    DependencyEdge, DependencyKind, Executable, MaterializationSummary, NpmBins,
    NpmPackageManifest, PackageNode, RuntimeSelection, RuntimeSource, ToolManifest, MANIFEST_FILE,
};
use self::registry::{RegisteredCommand, RegisteredTool, ToolRegistry};
use super::check_shim_reachable;
use super::package::BinConfig;
use super::package_store::{PackageArtifact, PackageStore};
use crate::command::create_command;
use crate::error::{ErrorKind, Fallible};
use crate::fs::{remove_dir_if_exists, rename};
use crate::layout::volta_home;
use crate::platform::{Platform, PlatformSpec, Source};
use crate::session::Session;
use crate::shim::{self, ShimResult};
use crate::style::{progress_spinner, success_prefix, tool_version};
use crate::sync::VoltaLock;

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

/// A command resolved from the isolated-tool registry.
pub(crate) struct ResolvedToolCommand {
    pub(crate) command: String,
    pub(crate) path: PathBuf,
    pub(crate) platform: Platform,
}

/// A registry lookup that has not loaded or validated the referenced
/// environment yet. Keeping this separate lets project-local resolution use
/// the package identity without allowing a damaged global environment to
/// block a valid local executable.
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

/// One published environment and the metadata that owns its dependency graph
/// and Node resolution topology.
struct ToolEnvironment {
    root: PathBuf,
    manifest: ToolManifest,
}

impl ToolEnvironment {
    fn load(package: &str, installation: &str) -> Fallible<Self> {
        validate_registry_reference(package, installation)?;
        let root = volta_home()?.tool_installation_dir(package, installation);
        let manifest = ToolManifest::read(&root.join(MANIFEST_FILE))?;
        if manifest.package != package || manifest.installation_id != installation {
            return Err(corrupt(
                package,
                "registry and environment manifest do not agree",
            ));
        }
        validate_installation_id(&manifest)?;
        validate_executable_files(&manifest, &root)?;
        Ok(Self { root, manifest })
    }

    fn resolve(self, command: &str) -> Fallible<ResolvedToolCommand> {
        let executable = self
            .manifest
            .executables
            .iter()
            .find(|executable| executable.name == command)
            .ok_or_else(|| {
                corrupt(
                    &self.manifest.package,
                    "registered executable is absent from manifest",
                )
            })?;
        Ok(ResolvedToolCommand {
            command: command.to_owned(),
            path: self.root.join(&executable.path),
            platform: PlatformSpec {
                node: self.manifest.runtime.node,
                npm: None,
                pnpm: None,
                yarn: None,
            }
            .as_binary(),
        })
    }
}

/// Install or atomically replace one isolated JavaScript CLI tool.
pub fn install(spec: ToolPackageSpec, session: &mut Session) -> Fallible<InstalledTool> {
    let _lock = VoltaLock::acquire()?;
    let platform = Platform::current(session)?.ok_or(ErrorKind::NoPlatform)?;
    let runtime = RuntimeSelection {
        node: platform.node.value.clone(),
        source: runtime_source(platform.node.source),
    };
    let image = platform.checkout(session)?;

    let home = volta_home()?;
    let staging_root = home.tmp_dir().join("tool-environments");
    fs::create_dir_all(&staging_root)
        .map_err(|_| environment_metadata_error("create", &staging_root))?;
    let staging = tempdir_in(&staging_root)
        .map_err(|_| environment_metadata_error("create", &staging_root))?;

    write_environment_package_json(staging.path())?;
    run_npm_install(&spec, staging.path(), &image)?;

    let package_root = package_directory(staging.path(), spec.name());
    let root_manifest = NpmPackageManifest::read(spec.name(), &package_root)?;
    if root_manifest.name != spec.name() {
        return Err(corrupt(
            spec.name(),
            "npm resolved a package with a different identity",
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

    let integrities = read_lock_integrities(staging.path());
    let scanned = scan_installed_packages(staging.path(), &integrities, spec.name())?;
    let package_paths: BTreeSet<PathBuf> =
        scanned.iter().map(|package| package.root.clone()).collect();
    let store = PackageStore::new(
        home.package_store_dir().to_owned(),
        home.package_store_temp_dir().to_owned(),
    );
    let mut packages = Vec::with_capacity(scanned.len());

    for package in scanned {
        let artifact = store.intern(&package.root)?;
        let materialization = materialize_package(&artifact, &package.root)?;
        let dependencies = resolve_dependencies(&package, staging.path(), &package_paths);
        packages.push(PackageNode {
            path: package.relative,
            name: package.manifest.name,
            version: package.manifest.version,
            content_hash: artifact.content_hash.as_str().to_owned(),
            registry_integrity: package.registry_integrity,
            dependencies,
            materialization,
        });
    }
    packages.sort_by(|left, right| left.path.cmp(&right.path));

    let mut tool_manifest = ToolManifest {
        schema_version: 1,
        installation_id: String::new(),
        requested: spec.requested().to_owned(),
        package: spec.name().to_owned(),
        resolved_version,
        runtime,
        executables,
        packages,
    };
    tool_manifest.installation_id = installation_id(&tool_manifest)?;
    tool_manifest.write(staging.path())?;

    let final_dir = home.tool_installation_dir(spec.name(), &tool_manifest.installation_id);
    let published_new = publish_environment(staging, &final_dir, &tool_manifest)?;
    let old_installation = registry
        .tools
        .get(spec.name())
        .map(|registered| registered.installation.clone());
    let old_commands = registry
        .tools
        .get(spec.name())
        .map(|registered| registered.executables.clone())
        .unwrap_or_default();

    registry
        .commands
        .retain(|_, command| command.package != spec.name());
    let command_names = tool_manifest
        .executables
        .iter()
        .map(|executable| executable.name.clone())
        .collect::<Vec<_>>();
    registry.tools.insert(
        spec.name().to_owned(),
        RegisteredTool {
            installation: tool_manifest.installation_id.clone(),
            executables: command_names.clone(),
        },
    );
    for command in &command_names {
        registry.commands.insert(
            command.clone(),
            RegisteredCommand {
                package: spec.name().to_owned(),
                installation: tool_manifest.installation_id.clone(),
            },
        );
    }

    let mut created_shims = Vec::new();
    for command in &command_names {
        // In particular, the Windows shim writer replaces an existing .cmd
        // file and reports it as newly created. Leave an old installation's
        // shim untouched so rollback can never delete a previously working
        // command.
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
                // `shim::create` may have produced one of multiple Windows
                // launcher files before the other write failed.
                let _ = shim::delete(command);
                rollback_shims(&created_shims);
                if published_new {
                    let _ = remove_environment(&final_dir);
                }
                return Err(error);
            }
        }
    }
    if let Err(error) = registry.write(registry_path) {
        rollback_shims(&created_shims);
        if published_new {
            let _ = remove_environment(&final_dir);
        }
        return Err(error);
    }

    remove_stale_shims(&old_commands, &command_names);
    if let Some(old) = old_installation {
        if old != tool_manifest.installation_id {
            let old_dir = home.tool_installation_dir(spec.name(), &old);
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
        "{} installed {} with executables: {}",
        success_prefix(),
        tool_version(spec.name(), &tool_manifest.resolved_version),
        command_names.join(", ")
    );

    Ok(InstalledTool::from(&tool_manifest))
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
        if legacy_bin_exists(command) {
            continue;
        }
        shim::delete(command)?;
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
            ToolEnvironment::load(package, &registered.installation)
                .map(|environment| (&environment.manifest).into())
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

fn runtime_source(source: Source) -> RuntimeSource {
    match source {
        Source::Project => RuntimeSource::Project,
        Source::Default | Source::Binary => RuntimeSource::Default,
        Source::CommandLine => RuntimeSource::CommandLine,
    }
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

fn run_npm_install(
    spec: &ToolPackageSpec,
    staging: &Path,
    image: &crate::platform::Image,
) -> Fallible<()> {
    let mut command = create_command("npm");
    command.args([
        "install",
        "--global=false",
        "--loglevel=warn",
        "--no-update-notifier",
        "--no-audit",
        "--fund=false",
        "--bin-links=true",
        "--package-lock=true",
        "--save-exact",
    ]);
    command.arg("--prefix").arg(staging);
    command.arg("--");
    command.arg(spec.requested());
    command.current_dir(staging);
    command.env("PATH", image.path()?);
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
    } else if stderr.contains("code E404") {
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
            "npm did not create the requested package directory",
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
        if !target.starts_with(&package_root_canonical) {
            return Err(corrupt(
                spec.name(),
                &format!("executable '{}' escapes the package directory", name),
            ));
        }
        if !target.is_file() {
            return Err(corrupt(
                spec.name(),
                &format!("executable '{}' does not point to a file", name),
            ));
        }

        let relative = environment_bin_path(&name);
        let launcher = environment.join(&relative);
        if !launcher.is_file() {
            return Err(corrupt(
                spec.name(),
                &format!("npm did not create the '{}' executable", name),
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

#[derive(Debug)]
struct ScannedPackage {
    root: PathBuf,
    relative: String,
    manifest: NpmPackageManifest,
    registry_integrity: Option<String>,
}

fn scan_installed_packages(
    environment: &Path,
    integrities: &BTreeMap<String, String>,
    requested_package: &str,
) -> Fallible<Vec<ScannedPackage>> {
    let mut packages = Vec::new();
    scan_node_modules(
        environment,
        &environment.join("node_modules"),
        integrities,
        requested_package,
        &mut packages,
    )?;
    if !packages
        .iter()
        .any(|package| package.relative == format!("node_modules/{}", requested_package))
    {
        return Err(corrupt(
            requested_package,
            "the requested package is absent from npm's resolved dependency graph",
        ));
    }
    Ok(packages)
}

fn scan_node_modules(
    environment: &Path,
    node_modules: &Path,
    integrities: &BTreeMap<String, String>,
    requested_package: &str,
    packages: &mut Vec<ScannedPackage>,
) -> Fallible<()> {
    let mut entries = fs::read_dir(node_modules)
        .map_err(|_| corrupt(requested_package, "npm did not create node_modules"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| {
            corrupt(
                requested_package,
                "could not inspect npm's dependency graph",
            )
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|_| corrupt(requested_package, "could not inspect a dependency"))?;
        if !file_type.is_dir() {
            if file_type.is_symlink() {
                return Err(corrupt(
                    requested_package,
                    "linked npm dependencies are not supported in persistent tool environments",
                ));
            }
            continue;
        }

        if name.to_string_lossy().starts_with('@') {
            let mut scoped = fs::read_dir(entry.path())
                .map_err(|_| corrupt(requested_package, "could not inspect a package scope"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| corrupt(requested_package, "could not inspect a package scope"))?;
            scoped.sort_by_key(|entry| entry.file_name());
            for package in scoped {
                add_scanned_package(
                    environment,
                    &package.path(),
                    integrities,
                    requested_package,
                    packages,
                )?;
            }
        } else {
            add_scanned_package(
                environment,
                &entry.path(),
                integrities,
                requested_package,
                packages,
            )?;
        }
    }
    Ok(())
}

fn add_scanned_package(
    environment: &Path,
    package_root: &Path,
    integrities: &BTreeMap<String, String>,
    requested_package: &str,
    packages: &mut Vec<ScannedPackage>,
) -> Fallible<()> {
    let metadata = fs::symlink_metadata(package_root)
        .map_err(|_| corrupt(requested_package, "could not inspect a dependency package"))?;
    if !metadata.is_dir() {
        return Err(corrupt(
            requested_package,
            "linked npm dependencies are not supported in persistent tool environments",
        ));
    }
    let relative = manifest_path(environment, package_root)?;
    let manifest = NpmPackageManifest::read(&relative, package_root)?;
    packages.push(ScannedPackage {
        root: package_root.to_owned(),
        registry_integrity: integrities.get(&relative).cloned(),
        relative,
        manifest,
    });
    let nested = package_root.join("node_modules");
    if nested.is_dir() {
        scan_node_modules(
            environment,
            &nested,
            integrities,
            requested_package,
            packages,
        )?;
    }
    Ok(())
}

fn manifest_path(environment: &Path, path: &Path) -> Fallible<String> {
    let relative = path
        .strip_prefix(environment)
        .map_err(|_| environment_metadata_error("inspect", path))?;
    let parts = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| environment_metadata_error("inspect", path)),
            _ => Err(environment_metadata_error("inspect", path)),
        })
        .collect::<Fallible<Vec<_>>>()?;
    Ok(parts.join("/"))
}

fn read_lock_integrities(environment: &Path) -> BTreeMap<String, String> {
    let path = environment.join("package-lock.json");
    let Ok(file) = File::open(path) else {
        return BTreeMap::new();
    };
    let Ok(lockfile) = serde_json::from_reader::<_, serde_json::Value>(file) else {
        return BTreeMap::new();
    };
    lockfile
        .get("packages")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(path, metadata)| {
            metadata
                .get("integrity")
                .and_then(serde_json::Value::as_str)
                .map(|integrity| (path.clone(), integrity.to_owned()))
        })
        .collect()
}

fn resolve_dependencies(
    package: &ScannedPackage,
    environment: &Path,
    package_paths: &BTreeSet<PathBuf>,
) -> Vec<DependencyEdge> {
    let mut dependencies = Vec::new();
    push_dependency_edges(
        &mut dependencies,
        DependencyKind::Production,
        package.manifest.dependencies.keys(),
        package,
        environment,
        package_paths,
    );
    push_dependency_edges(
        &mut dependencies,
        DependencyKind::Optional,
        package.manifest.optional_dependencies.keys(),
        package,
        environment,
        package_paths,
    );
    push_dependency_edges(
        &mut dependencies,
        DependencyKind::Peer,
        package.manifest.peer_dependencies.keys(),
        package,
        environment,
        package_paths,
    );
    dependencies.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| format!("{:?}", left.kind).cmp(&format!("{:?}", right.kind)))
    });
    dependencies
}

fn push_dependency_edges<'a>(
    edges: &mut Vec<DependencyEdge>,
    kind: DependencyKind,
    names: impl Iterator<Item = &'a String>,
    package: &ScannedPackage,
    environment: &Path,
    package_paths: &BTreeSet<PathBuf>,
) {
    for name in names {
        let target = if validate(name).valid_for_old_packages() {
            resolve_dependency_path(&package.root, name, environment, package_paths)
                .and_then(|path| manifest_path(environment, &path).ok())
        } else {
            None
        };
        edges.push(DependencyEdge {
            name: name.clone(),
            kind,
            target,
        });
    }
}

fn resolve_dependency_path(
    package_root: &Path,
    dependency: &str,
    environment: &Path,
    package_paths: &BTreeSet<PathBuf>,
) -> Option<PathBuf> {
    let mut cursor = Some(package_root);
    while let Some(directory) = cursor {
        let candidate = directory.join("node_modules").join(dependency);
        if package_paths.contains(&candidate) {
            return Some(candidate);
        }
        if directory == environment {
            break;
        }
        cursor = directory.parent();
    }
    None
}

fn materialize_package(
    artifact: &PackageArtifact,
    package_root: &Path,
) -> Fallible<MaterializationSummary> {
    remove_package_contents(package_root)?;
    let mut summary = MaterializationSummary::default();
    materialize_directory(&artifact.path, package_root, &mut summary)?;
    Ok(summary)
}

fn remove_package_contents(package_root: &Path) -> Fallible<()> {
    for entry in fs::read_dir(package_root)
        .map_err(|_| environment_metadata_error("inspect", package_root))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| environment_metadata_error("inspect", package_root))?
    {
        if entry.file_name() == OsStr::new("node_modules") {
            continue;
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|_| environment_metadata_error("inspect", &path))?;
        if file_type.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        }
        .map_err(|_| environment_metadata_error("replace", &path))?;
    }
    Ok(())
}

fn materialize_directory(
    store_dir: &Path,
    environment_dir: &Path,
    summary: &mut MaterializationSummary,
) -> Fallible<()> {
    fs::create_dir_all(environment_dir)
        .map_err(|_| environment_metadata_error("create", environment_dir))?;
    let mut entries = fs::read_dir(store_dir)
        .map_err(|_| environment_metadata_error("read", store_dir))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| environment_metadata_error("read", store_dir))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let source = entry.path();
        let destination = environment_dir.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|_| environment_metadata_error("inspect", &source))?;
        if file_type.is_dir() {
            materialize_directory(&source, &destination, summary)?;
        } else if file_type.is_file() {
            #[cfg(unix)]
            match fs::hard_link(&source, &destination) {
                Ok(()) => summary.hard_links += 1,
                Err(_) => {
                    copy_environment_file(&source, &destination)?;
                    summary.copies += 1;
                }
            }
            // A Windows read-only attribute belongs to the shared file behind
            // all of its hard links. Clearing it to uninstall an environment
            // would therefore make the store entry writable too, so Windows
            // environments use independent writable copies.
            #[cfg(windows)]
            {
                copy_environment_file(&source, &destination)?;
                summary.copies += 1;
            }
        } else if file_type.is_symlink() {
            materialize_symlink(&source, &destination)?;
            summary.symlinks += 1;
        } else {
            return Err(environment_metadata_error("materialize", &source));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn materialize_symlink(source: &Path, destination: &Path) -> Fallible<()> {
    let target = fs::read_link(source).map_err(|_| environment_metadata_error("read", source))?;
    std::os::unix::fs::symlink(target, destination)
        .map_err(|_| environment_metadata_error("link", destination))
}

#[cfg(windows)]
fn materialize_symlink(source: &Path, destination: &Path) -> Fallible<()> {
    let target = fs::read_link(source).map_err(|_| environment_metadata_error("read", source))?;
    let resolved = source
        .parent()
        .expect("stored symlink has a parent")
        .join(&target);
    let result = if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(&target, destination)
    } else {
        std::os::windows::fs::symlink_file(&target, destination)
    };
    if result.is_ok() {
        return Ok(());
    }

    // Windows may disallow symlink creation without Developer Mode. Dereference
    // only the already-validated in-package target and make a plain copy.
    if resolved.is_dir() {
        let mut ignored = MaterializationSummary::default();
        materialize_directory(&resolved, destination, &mut ignored)
    } else {
        copy_environment_file(&resolved, destination)
    }
}

fn copy_environment_file(source: &Path, destination: &Path) -> Fallible<()> {
    fs::copy(source, destination).map_err(|_| environment_metadata_error("copy", destination))?;
    #[cfg(windows)]
    {
        let mut permissions = fs::metadata(destination)
            .map_err(|_| environment_metadata_error("inspect", destination))?
            .permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(destination, permissions)
            .map_err(|_| environment_metadata_error("update permissions for", destination))?;
    }
    Ok(())
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
            &manifest.package,
            "environment manifest does not match its installation identifier",
        ))
    }
}

fn publish_environment(
    staging: TempDir,
    destination: &Path,
    expected: &ToolManifest,
) -> Fallible<bool> {
    let parent = destination
        .parent()
        .expect("tool installation path has a parent");
    fs::create_dir_all(parent).map_err(|_| environment_metadata_error("create", parent))?;

    if destination.exists() {
        let existing = ToolManifest::read(&destination.join(MANIFEST_FILE))?;
        validate_manifest_identity(&existing, expected)?;
        validate_installation_id(&existing)?;
        validate_executable_files(&existing, destination)?;
        return Ok(false);
    }

    let staging_path = staging.keep();
    rename(&staging_path, destination)
        .map_err(|_| environment_metadata_error("publish", destination))?;
    Ok(true)
}

fn validate_manifest_identity(existing: &ToolManifest, expected: &ToolManifest) -> Fallible<()> {
    if existing.package == expected.package && existing.installation_id == expected.installation_id
    {
        Ok(())
    } else {
        Err(corrupt(
            &expected.package,
            "an installation identifier points to different metadata",
        ))
    }
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
            &manifest.package,
            executable.path.to_string_lossy().as_ref(),
        )? != executable.path
        {
            return Err(corrupt(
                &manifest.package,
                "manifest contains an unsafe executable path",
            ));
        }
        if !root.join(&executable.path).is_file() {
            return Err(corrupt(
                &manifest.package,
                &format!("executable '{}' is missing", executable.name),
            ));
        }
        if executable_integrity(&manifest.package, &root.join(&executable.path))?
            != executable.integrity
        {
            return Err(corrupt(
                &manifest.package,
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
    use std::fs;

    use tempfile::tempdir;

    use super::*;

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
    fn resolves_nested_and_hoisted_dependencies_by_node_topology() {
        let temp = tempdir().expect("temp directory");
        let root = temp.path();
        let package = root.join("node_modules/tool");
        let nested = package.join("node_modules/dependency");
        let hoisted = root.join("node_modules/hoisted");
        for path in [&package, &nested, &hoisted] {
            fs::create_dir_all(path).expect("package directory");
        }
        let paths = [package.clone(), nested.clone(), hoisted.clone()]
            .into_iter()
            .collect();

        assert_eq!(
            resolve_dependency_path(&package, "dependency", root, &paths),
            Some(nested)
        );
        assert_eq!(
            resolve_dependency_path(&package, "hoisted", root, &paths),
            Some(hoisted)
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
