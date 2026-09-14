//! Provides fetcher for npm distributions

use std::fs::write;
use std::path::Path;

use super::super::registry::public_registry_package;
use crate::error::{Context, ErrorKind, Fallible};
use crate::fs::{create_staging_dir, rename, set_executable};
use crate::hook::ToolHooks;
use crate::layout::volta_home;
use crate::style::{progress_bar, tool_version};
use crate::tool::distribution::{npm_integrity, VerifiedDownload};
use crate::tool::{self, Npm};
use crate::version::VersionSpec;
use archive::{Archive, Tarball};
use fs_utils::ensure_containing_dir_exists;
use log::debug;
use node_semver::Version;

pub fn fetch(version: &Version, hooks: Option<&ToolHooks<Npm>>) -> Fallible<()> {
    let npm_dir = volta_home()?.npm_inventory_dir();
    let cache_file = npm_dir.join(Npm::archive_filename(&version.to_string()));

    let download = VerifiedDownload::fetch(
        tool::Spec::Npm(VersionSpec::Exact(version.clone())),
        cache_file,
        || determine_remote_url(version, hooks),
        || npm_integrity("npm", version),
    )?;
    let archive =
        Tarball::load(download.open()?).with_context(|| ErrorKind::UnpackArchiveError {
            tool: "npm".into(),
            version: version.to_string(),
        })?;
    unpack_archive(archive, version)?;
    download.persist()?;

    Ok(())
}

/// Unpack the npm archive into the image directory so that it is ready for use
fn unpack_archive(archive: Box<dyn Archive>, version: &Version) -> Fallible<()> {
    let temp = create_staging_dir()?;
    debug!("Unpacking npm into '{}'", temp.path().display());

    let progress = progress_bar(
        archive.origin(),
        &tool_version("npm", version),
        archive.compressed_size(),
    );
    let version_string = version.to_string();

    archive
        .unpack(temp.path(), &mut |_, read| {
            progress.inc(read as u64);
        })
        .with_context(|| ErrorKind::UnpackArchiveError {
            tool: "npm".into(),
            version: version_string.clone(),
        })?;

    let bin_path = temp.path().join("package").join("bin");
    overwrite_launcher(&bin_path, "npm")?;
    overwrite_launcher(&bin_path, "npx")?;

    #[cfg(windows)]
    {
        overwrite_cmd_launcher(&bin_path, "npm")?;
        overwrite_cmd_launcher(&bin_path, "npx")?;
    }

    let dest = volta_home()?.npm_image_dir(&version_string);
    ensure_containing_dir_exists(&dest)
        .with_context(|| ErrorKind::ContainingDirError { path: dest.clone() })?;

    rename(temp.path().join("package"), &dest).with_context(|| ErrorKind::SetupToolImageError {
        tool: "npm".into(),
        version: version_string.clone(),
        dir: dest.clone(),
    })?;

    progress.finish_and_clear();

    // Note: We write this after the progress bar is finished to avoid display bugs with re-renders of the progress
    debug!("Installing npm in '{}'", dest.display());

    Ok(())
}

/// Determine the remote URL to download from, using the hooks if avaialble
fn determine_remote_url(version: &Version, hooks: Option<&ToolHooks<Npm>>) -> Fallible<String> {
    let version_str = version.to_string();
    match hooks {
        Some(&ToolHooks {
            distro: Some(ref hook),
            ..
        }) => {
            debug!("Using npm.distro hook to determine download URL");
            let distro_file_name = Npm::archive_filename(&version_str);
            hook.resolve(version, &distro_file_name)
        }
        _ => Ok(public_registry_package("npm", &version_str)),
    }
}

/// Overwrite the launcher script
fn overwrite_launcher(base_path: &Path, tool: &str) -> Fallible<()> {
    let path = base_path.join(tool);
    write(
        &path,
        // Note: Adapted from the existing npm/npx launcher, without unnecessary detection of Node location
        format!(
            r#"#!/bin/sh
(set -o igncr) 2>/dev/null && set -o igncr; # cygwin encoding fix

basedir=`dirname "$0"`

case `uname` in
    *CYGWIN*) basedir=`cygpath -w "$basedir"`;;
esac

node "$basedir/{}-cli.js" "$@"
"#,
            tool
        ),
    )
    .and_then(|_| set_executable(&path))
    .with_context(|| ErrorKind::WriteLauncherError { tool: tool.into() })
}

/// Overwrite the CMD launcher
#[cfg(windows)]
fn overwrite_cmd_launcher(base_path: &Path, tool: &str) -> Fallible<()> {
    write(
        base_path.join(format!("{}.cmd", tool)),
        // Note: Adapted from the existing npm/npx cmd launcher, without unnecessary detection of Node location
        format!(
            r#"@ECHO OFF

node "%~dp0\{}-cli.js" %*
"#,
            tool
        ),
    )
    .with_context(|| ErrorKind::WriteLauncherError { tool: tool.into() })
}
