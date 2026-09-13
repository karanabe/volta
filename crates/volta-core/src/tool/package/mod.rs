use std::fmt::{self, Display};
use std::fs::create_dir_all;
use std::path::Path;

use super::Tool;
use crate::error::{Context, ErrorKind, Fallible};
use crate::fs::{remove_dir_if_exists, rename, symlink_dir};
use crate::layout::volta_home;
use crate::platform::Image;
use crate::session::Session;
use crate::style::tool_version;
use crate::version::VersionSpec;
use fs_utils::ensure_containing_dir_exists;
use tempfile::{tempdir_in, TempDir};

mod configure;
mod install;
mod manager;
mod metadata;
mod uninstall;

pub use manager::PackageManager;
pub use metadata::{BinConfig, PackageConfig, PackageManifest};
pub use uninstall::uninstall;

/// A third-party package specification.
///
/// Volta no longer accepts third-party packages through `volta install`. This
/// type remains so old Volta-managed packages can be migrated and uninstalled.
pub struct Package {
    name: String,
    version: VersionSpec,
}

impl Package {
    pub fn new(name: String, version: VersionSpec) -> Self {
        Package { name, version }
    }

    /// Reinstall a package while migrating a legacy Volta layout.
    #[doc(hidden)]
    pub fn migrate_legacy_install(self, platform_image: &Image) -> Fallible<PackageManifest> {
        let manager = PackageManager::Npm;
        let staging = setup_staging_directory(manager)?;

        install::run_global_install(self.to_string(), staging.path().to_owned(), platform_image)?;
        let manifest = configure::parse_manifest(&self.name, staging.path().to_owned(), manager)?;

        persist_install(&self.name, &self.version, staging.path())?;
        link_package_to_shared_dir(&self.name, manager)?;
        configure::write_config_and_shims(&self.name, &manifest, platform_image, manager)?;

        Ok(manifest)
    }
}

impl Tool for Package {
    fn fetch(self: Box<Self>, _session: &mut Session) -> Fallible<()> {
        Err(ErrorKind::CannotFetchPackage {
            package: self.to_string(),
        }
        .into())
    }

    fn install(self: Box<Self>, _session: &mut Session) -> Fallible<()> {
        Err(ErrorKind::PackageInstallRemoved {
            package: self.to_string(),
        }
        .into())
    }

    fn pin(self: Box<Self>, _session: &mut Session) -> Fallible<()> {
        Err(ErrorKind::CannotPinPackage { package: self.name }.into())
    }
}

impl Display for Package {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.version {
            VersionSpec::None => f.write_str(&self.name),
            _ => f.write_str(&tool_version(&self.name, &self.version)),
        }
    }
}

/// Create the temporary staging directory we will use to install and ensure expected
/// subdirectories exist within it
fn setup_staging_directory(manager: PackageManager) -> Fallible<TempDir> {
    // Workaround to ensure relative symlinks continue to work.
    // The final installed location of packages is:
    //      $VOLTA_HOME/tools/image/packages/{name}/
    // To ensure that the temp directory has the same amount of nesting, we use:
    //      $VOLTA_HOME/tmp/image/packages/{tempdir}/
    // This way any relative symlinks will have the same amount of nesting and will remain valid
    // even when the directory is persisted.
    let mut staging_root = volta_home()?.tmp_dir().to_owned();
    staging_root.push("image");
    staging_root.push("packages");
    create_dir_all(&staging_root).with_context(|| ErrorKind::ContainingDirError {
        path: staging_root.clone(),
    })?;
    let staging = tempdir_in(&staging_root).with_context(|| ErrorKind::CreateTempDirError {
        in_dir: staging_root,
    })?;

    let source_dir = manager.source_dir(staging.path().to_owned());
    ensure_containing_dir_exists(&source_dir)
        .with_context(|| ErrorKind::ContainingDirError { path: source_dir })?;

    let binary_dir = manager.binary_dir(staging.path().to_owned());
    ensure_containing_dir_exists(&binary_dir)
        .with_context(|| ErrorKind::ContainingDirError { path: binary_dir })?;

    Ok(staging)
}

fn persist_install<V>(package_name: &str, package_version: V, staging_dir: &Path) -> Fallible<()>
where
    V: Display,
{
    let package_dir = volta_home()?.package_image_dir(package_name);

    remove_dir_if_exists(&package_dir)?;

    // Handle scoped packages (@vue/cli), which have an extra directory for the scope
    ensure_containing_dir_exists(&package_dir).with_context(|| ErrorKind::ContainingDirError {
        path: package_dir.to_owned(),
    })?;

    rename(staging_dir, &package_dir).with_context(|| ErrorKind::SetupToolImageError {
        tool: package_name.into(),
        version: package_version.to_string(),
        dir: package_dir,
    })?;

    Ok(())
}

fn link_package_to_shared_dir(package_name: &str, manager: PackageManager) -> Fallible<()> {
    let home = volta_home()?;
    let mut source = manager.source_dir(home.package_image_dir(package_name));
    source.push(package_name);

    let target = home.shared_lib_dir(package_name);

    remove_dir_if_exists(&target)?;

    // Handle scoped packages (@vue/cli), which have an extra directory for the scope
    ensure_containing_dir_exists(&target).with_context(|| ErrorKind::ContainingDirError {
        path: target.clone(),
    })?;

    symlink_dir(source, target).with_context(|| ErrorKind::CreateSharedLinkError {
        name: package_name.into(),
    })
}
