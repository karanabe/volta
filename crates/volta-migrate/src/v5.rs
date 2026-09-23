use std::fs::File;
use std::path::PathBuf;

use super::empty::Empty;
use super::v4::V4;
use log::debug;
use volta_core::error::{Context, ErrorKind, Fallible, VoltaError};
use volta_core::fs::remove_file_if_exists;
use volta_layout::v5;

/// Represents the V5 layout introduced for Volta 3.0.0.
pub struct V5 {
    pub home: v5::VoltaHome,
}

impl V5 {
    pub fn new(home: PathBuf) -> Self {
        V5 {
            home: v5::VoltaHome::new(home),
        }
    }

    /// Mark the layout as current only after its directories are ready.
    fn complete_migration(home: v5::VoltaHome) -> Fallible<Self> {
        File::create(home.layout_file()).with_context(|| ErrorKind::CreateLayoutFileError {
            file: home.layout_file().to_owned(),
        })?;

        Ok(V5 { home })
    }
}

impl TryFrom<Empty> for V5 {
    type Error = VoltaError;

    fn try_from(old: Empty) -> Fallible<V5> {
        debug!("New Volta installation detected, creating fresh V5 layout");

        let home = v5::VoltaHome::new(old.home);
        home.create().with_context(|| ErrorKind::CreateDirError {
            dir: home.root().to_owned(),
        })?;

        V5::complete_migration(home)
    }
}

impl TryFrom<V4> for V5 {
    type Error = VoltaError;

    fn try_from(old: V4) -> Fallible<V5> {
        debug!("Migrating from V4 layout");

        let home = v5::VoltaHome::new(old.home.root().to_owned());
        // create_dir_all leaves existing 2.x and pre-release tool data intact.
        home.create().with_context(|| ErrorKind::CreateDirError {
            dir: home.root().to_owned(),
        })?;

        let layout = V5::complete_migration(home)?;
        remove_file_if_exists(old.home.layout_file())?;
        Ok(layout)
    }
}
