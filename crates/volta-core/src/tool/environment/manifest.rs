use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use node_semver::Version;

use crate::error::{ErrorKind, Fallible};
use crate::version::version_serde;

pub(super) const MANIFEST_FILE: &str = "volta-tool.json";

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct ToolManifest {
    pub(super) schema_version: u32,
    pub(super) installation_id: String,
    pub(super) requested: String,
    pub(super) package: String,
    #[serde(with = "version_serde")]
    pub(super) resolved_version: Version,
    pub(super) runtime: RuntimeSelection,
    pub(super) executables: Vec<Executable>,
    pub(super) packages: Vec<PackageNode>,
}

impl ToolManifest {
    pub(super) fn read(path: &Path) -> Fallible<Self> {
        let file = File::open(path).map_err(|_| metadata_error("read", path))?;
        let manifest: Self =
            serde_json::from_reader(file).map_err(|_| metadata_error("parse", path))?;
        if manifest.schema_version != 1 {
            return Err(ErrorKind::ToolEnvironmentCorrupt {
                package: manifest.package,
                reason: format!(
                    "unsupported tool manifest schema {}",
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
pub(super) struct RuntimeSelection {
    #[serde(with = "version_serde")]
    pub(super) node: Version,
    pub(super) source: RuntimeSource,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum RuntimeSource {
    Project,
    Default,
    CommandLine,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct Executable {
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) integrity: String,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct PackageNode {
    /// Package path relative to the tool-environment root. This path is the
    /// node's identity in the resolved Node module topology.
    pub(super) path: String,
    pub(super) name: String,
    pub(super) version: String,
    pub(super) content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) registry_integrity: Option<String>,
    pub(super) dependencies: Vec<DependencyEdge>,
    pub(super) materialization: MaterializationSummary,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct DependencyEdge {
    pub(super) name: String,
    pub(super) kind: DependencyKind,
    /// Resolved target path relative to the environment, or `None` for an
    /// optional/peer dependency that npm did not materialize.
    pub(super) target: Option<String>,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum DependencyKind {
    Production,
    Optional,
    Peer,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
pub(super) struct MaterializationSummary {
    pub(super) hard_links: u64,
    pub(super) copies: u64,
    pub(super) symlinks: u64,
}

/// Stable, presentation-oriented information returned by `volta tool list`.
#[derive(Clone, Debug)]
pub struct InstalledTool {
    pub package: String,
    pub requested: String,
    pub version: Version,
    pub node: Version,
    pub executables: Vec<String>,
    pub package_count: usize,
}

impl From<&ToolManifest> for InstalledTool {
    fn from(manifest: &ToolManifest) -> Self {
        Self {
            package: manifest.package.clone(),
            requested: manifest.requested.clone(),
            version: manifest.resolved_version.clone(),
            node: manifest.runtime.node.clone(),
            executables: manifest
                .executables
                .iter()
                .map(|executable| executable.name.clone())
                .collect(),
            package_count: manifest.packages.len(),
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct NpmPackageManifest {
    pub(super) name: String,
    pub(super) version: String,
    #[serde(default)]
    pub(super) bin: Option<NpmBins>,
    #[serde(default)]
    pub(super) dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "optionalDependencies")]
    pub(super) optional_dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "peerDependencies")]
    pub(super) peer_dependencies: BTreeMap<String, String>,
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
