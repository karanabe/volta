use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use tempfile::NamedTempFile;

use crate::error::{ErrorKind, Fallible};

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub(super) struct ToolRegistry {
    #[serde(default = "schema_version")]
    pub(super) schema_version: u32,
    #[serde(default)]
    pub(super) tools: BTreeMap<String, RegisteredTool>,
    #[serde(default)]
    pub(super) commands: BTreeMap<String, RegisteredCommand>,
}

impl ToolRegistry {
    pub(super) fn read(path: &Path) -> Fallible<Self> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    schema_version: schema_version(),
                    ..Self::default()
                });
            }
            Err(_) => return Err(metadata_error("read", path)),
        };
        let registry: Self =
            serde_json::from_reader(file).map_err(|_| metadata_error("parse", path))?;
        if registry.schema_version != schema_version() {
            return Err(metadata_error("parse", path));
        }
        Ok(registry)
    }

    pub(super) fn write(&self, path: &Path) -> Fallible<()> {
        let parent = path.parent().expect("tool registry path has a parent");
        fs::create_dir_all(parent).map_err(|_| metadata_error("create", parent))?;
        let mut temporary =
            NamedTempFile::new_in(parent).map_err(|_| metadata_error("write", path))?;
        serde_json::to_writer_pretty(temporary.as_file_mut(), self)
            .map_err(|_| metadata_error("serialize", path))?;
        temporary
            .as_file_mut()
            .write_all(b"\n")
            .map_err(|_| metadata_error("write", path))?;
        temporary
            .as_file_mut()
            .sync_all()
            .map_err(|_| metadata_error("write", path))?;
        temporary
            .persist(path)
            .map_err(|_| metadata_error("publish", path))?;
        Ok(())
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct RegisteredTool {
    pub(super) installation: String,
    pub(super) executables: Vec<String>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct RegisteredCommand {
    pub(super) package: String,
    pub(super) installation: String,
}

const fn schema_version() -> u32 {
    1
}

fn metadata_error(operation: &str, path: &Path) -> crate::error::VoltaError {
    ErrorKind::ToolMetadataError {
        operation: operation.to_owned(),
        path: path.to_owned(),
    }
    .into()
}
