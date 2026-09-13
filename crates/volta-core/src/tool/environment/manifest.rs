use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use node_semver::Version;

use crate::error::{ErrorKind, Fallible};
use crate::version::version_serde;

pub(super) const MANIFEST_FILE: &str = "volta-tool.json";
pub(super) const LOCKFILE: &str = "pnpm-lock.yaml";
pub(super) const RUNTIME_LINK: &str = "runtime/node";

/// The durable receipt for one immutable JavaScript CLI environment.
///
/// Package topology belongs to pnpm and is persisted in `pnpm-lock.yaml`.
/// Volta records only the inputs needed to recreate the environment and the
/// runtime and executable references needed to execute it safely.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct ToolManifest {
    pub(super) schema_version: u32,
    pub(super) installation_id: String,
    pub(super) package: PackageSelection,
    pub(super) runtime: RuntimeSelection,
    pub(super) installer: InstallerSelection,
    pub(super) settings: InstallSettings,
    pub(super) executables: Vec<Executable>,
    pub(super) lockfile_integrity: String,
}

impl ToolManifest {
    pub(super) fn read(path: &Path) -> Fallible<Self> {
        let file = File::open(path).map_err(|_| metadata_error("read", path))?;
        let manifest: Self =
            serde_json::from_reader(file).map_err(|_| metadata_error("parse", path))?;
        if manifest.schema_version != 2 {
            return Err(ErrorKind::ToolEnvironmentCorrupt {
                package: manifest.package.name,
                reason: format!(
                    "unsupported tool manifest schema {} (Volta 3 requires schema 2)",
                    manifest.schema_version
                ),
            }
            .into());
        }
        Ok(manifest)
    }

    pub(super) fn write(&self, root: &Path) -> Fallible<()> {
        let path = root.join(MANIFEST_FILE);
        let file = File::create(&path).map_err(|_| metadata_error("write", &path))?;
        serde_json::to_writer_pretty(file, self).map_err(|_| metadata_error("serialize", &path))
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct PackageSelection {
    /// The complete npm package selector supplied at install time.
    pub(super) requested: String,
    pub(super) name: String,
    #[serde(with = "version_serde")]
    pub(super) resolved: Version,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct RuntimeSelection {
    /// The user-provided Node request, or the exact default selected at install time.
    pub(super) requested: String,
    #[serde(with = "version_serde")]
    pub(super) resolved: Version,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct InstallerSelection {
    pub(super) kind: InstallerKind,
    #[serde(with = "version_serde")]
    pub(super) version: Version,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum InstallerKind {
    Pnpm,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub(super) struct InstallSettings {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) allow_builds: Vec<String>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct Executable {
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) integrity: String,
}

/// Stable, presentation-oriented information returned by `volta tool list`.
#[derive(Clone, Debug)]
pub struct InstalledTool {
    pub package: String,
    pub requested: String,
    pub version: Version,
    pub node: Version,
    pub installer: Version,
    pub executables: Vec<String>,
    pub runtime_available: bool,
}

impl InstalledTool {
    pub(super) fn from_manifest(manifest: &ToolManifest, runtime_available: bool) -> Self {
        Self {
            package: manifest.package.name.clone(),
            requested: manifest.package.requested.clone(),
            version: manifest.package.resolved.clone(),
            node: manifest.runtime.resolved.clone(),
            installer: manifest.installer.version.clone(),
            executables: manifest
                .executables
                .iter()
                .map(|executable| executable.name.clone())
                .collect(),
            runtime_available,
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct NpmPackageManifest {
    pub(super) name: String,
    pub(super) version: String,
    #[serde(default)]
    pub(super) bin: Option<NpmBins>,
}

impl NpmPackageManifest {
    pub(super) fn read(package: &str, root: &Path) -> Fallible<Self> {
        let path = root.join("package.json");
        let file = File::open(&path).map_err(|_| ErrorKind::PackageManifestReadError {
            package: package.to_owned(),
        })?;
        serde_json::from_reader(file).map_err(|_| {
            ErrorKind::PackageManifestParseError {
                package: package.to_owned(),
            }
            .into()
        })
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(untagged)]
pub(super) enum NpmBins {
    Single(String),
    Multiple(BTreeMap<String, String>),
}

fn metadata_error(operation: &str, path: &Path) -> crate::error::VoltaError {
    ErrorKind::ToolMetadataError {
        operation: operation.to_owned(),
        path: path.to_owned(),
    }
    .into()
}
