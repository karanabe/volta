//! Removal of runtime and package-manager inventory entries.

use node_semver::Version;

use super::distribution::remove_cached_archive;
use super::{environment, Node, Npm, Pnpm, Yarn};
use crate::error::{ErrorKind, Fallible};
use crate::fs::{remove_dir_if_exists, remove_file_if_exists};
use crate::layout::volta_home;
use crate::session::Session;
use crate::style::{success_prefix, tool_version};
use crate::sync::VoltaLock;
use crate::version::VersionSpec;
use log::info;

pub(super) fn node(requested: VersionSpec, force: bool, session: &mut Session) -> Fallible<()> {
    let _lock = VoltaLock::acquire()?;
    let default = session
        .default_platform()?
        .map(|platform| platform.node.clone());
    let version = exact_or_default("node", requested, default)?;
    let tools = environment::tools_using_node(&version)?;
    if !force && !tools.is_empty() {
        return Err(ErrorKind::ToolRuntimeInUse {
            version: version.to_string(),
            tools,
        }
        .into());
    }

    let home = volta_home()?;
    let version_string = version.to_string();
    let image = home.node_image_dir(&version_string);
    let archive = home
        .node_inventory_dir()
        .join(Node::archive_filename(&version));
    let npm_metadata = home.node_npm_version_file(&version_string);
    ensure_installed("node", &version, &[&image, &archive, &npm_metadata])?;

    session.toolchain_mut()?.clear_active_node(&version)?;
    remove_dir_if_exists(image)?;
    remove_cached_archive(&archive)?;
    remove_file_if_exists(npm_metadata)?;
    info!(
        "{} {} uninstalled",
        success_prefix(),
        tool_version("node", version)
    );
    Ok(())
}

pub(super) fn npm(requested: VersionSpec, session: &mut Session) -> Fallible<()> {
    let _lock = VoltaLock::acquire()?;
    let platform = session.default_platform()?;
    let has_platform = platform.is_some();
    let active = platform.and_then(|platform| platform.npm.clone());
    if matches!(requested, VersionSpec::None) {
        if !has_platform {
            return Err(ErrorKind::NoPlatform.into());
        }
        if active.is_none() {
            return Err(ErrorKind::BundledNpmUninstall.into());
        }
    }
    let version = exact_or_default("npm", requested, active)?;
    let home = volta_home()?;
    let version_string = version.to_string();
    let image = home.npm_image_dir(&version_string);
    let archive = home
        .npm_inventory_dir()
        .join(Npm::archive_filename(&version_string));
    ensure_installed("npm", &version, &[&image, &archive])?;
    if session
        .default_platform()?
        .and_then(|platform| platform.npm.as_ref())
        == Some(&version)
    {
        session.toolchain_mut()?.set_active_npm(None)?;
    }
    remove_dir_if_exists(image)?;
    remove_cached_archive(&archive)?;
    info!(
        "{} {} uninstalled",
        success_prefix(),
        tool_version("npm", version)
    );
    Ok(())
}

pub(super) fn pnpm(requested: VersionSpec, session: &mut Session) -> Fallible<()> {
    let _lock = VoltaLock::acquire()?;
    let active = session
        .default_platform()?
        .and_then(|platform| platform.pnpm.clone());
    let version = exact_or_default("pnpm", requested, active)?;
    let home = volta_home()?;
    let version_string = version.to_string();
    let image = home.pnpm_image_dir(&version_string);
    let archive = home
        .pnpm_inventory_dir()
        .join(Pnpm::archive_filename(&version_string));
    ensure_installed("pnpm", &version, &[&image, &archive])?;
    if session
        .default_platform()?
        .and_then(|platform| platform.pnpm.as_ref())
        == Some(&version)
    {
        session.toolchain_mut()?.set_active_pnpm(None)?;
    }
    remove_dir_if_exists(image)?;
    remove_cached_archive(&archive)?;
    info!(
        "{} {} uninstalled",
        success_prefix(),
        tool_version("pnpm", version)
    );
    Ok(())
}

pub(super) fn yarn(requested: VersionSpec, session: &mut Session) -> Fallible<()> {
    let _lock = VoltaLock::acquire()?;
    let active = session
        .default_platform()?
        .and_then(|platform| platform.yarn.clone());
    let version = exact_or_default("yarn", requested, active)?;
    let home = volta_home()?;
    let version_string = version.to_string();
    let image = home.yarn_image_dir(&version_string);
    let archive = home
        .yarn_inventory_dir()
        .join(Yarn::archive_filename(&version_string));
    ensure_installed("yarn", &version, &[&image, &archive])?;
    if session
        .default_platform()?
        .and_then(|platform| platform.yarn.as_ref())
        == Some(&version)
    {
        session.toolchain_mut()?.set_active_yarn(None)?;
    }
    remove_dir_if_exists(image)?;
    remove_cached_archive(&archive)?;
    info!(
        "{} {} uninstalled",
        success_prefix(),
        tool_version("yarn", version)
    );
    Ok(())
}

fn exact_or_default(
    tool: &str,
    requested: VersionSpec,
    default: Option<Version>,
) -> Fallible<Version> {
    match requested {
        VersionSpec::Exact(version) => Ok(version),
        VersionSpec::None => default.ok_or_else(|| match tool {
            "node" => ErrorKind::NoPlatform.into(),
            "pnpm" => ErrorKind::NoDefaultPnpm.into(),
            "yarn" => ErrorKind::NoDefaultYarn.into(),
            _ => ErrorKind::InventoryToolNotInstalled {
                tool: tool.to_owned(),
                version: "<active>".to_owned(),
            }
            .into(),
        }),
        range_or_tag => Err(ErrorKind::UninstallVersionRequired {
            tool: tool.to_owned(),
            requested: range_or_tag.to_string(),
        }
        .into()),
    }
}

fn ensure_installed(tool: &str, version: &Version, paths: &[&std::path::Path]) -> Fallible<()> {
    if paths.iter().any(|path| path.exists()) {
        Ok(())
    } else {
        Err(ErrorKind::InventoryToolNotInstalled {
            tool: tool.to_owned(),
            version: version.to_string(),
        }
        .into())
    }
}
