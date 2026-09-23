use std::ffi::OsString;

use super::executor::{Executor, ToolCommand, ToolKind};
use super::{debug_active_image, debug_no_platform};
use crate::error::{ErrorKind, Fallible};
use crate::platform::{Platform, Source, System};
use crate::session::{ActivityKind, Session};

/// Build an `Executor` for Yarn
pub(super) fn command(
    args: &[OsString],
    recursive: bool,
    session: &mut Session,
) -> Fallible<Executor> {
    session.add_event_start(ActivityKind::Yarn);
    // Don't re-evaluate the context if this is a recursive call
    let platform = if recursive {
        None
    } else {
        Platform::current(session)?
    };

    Ok(ToolCommand::new("yarn", args, platform, ToolKind::Yarn).into())
}

/// Determine the execution context (PATH and failure error message) for Yarn
pub(super) fn execution_context(
    platform: Option<Platform>,
    session: &mut Session,
) -> Fallible<(OsString, ErrorKind)> {
    match platform {
        Some(plat) => {
            validate_platform_yarn(&plat)?;

            let image = plat.checkout(session)?;
            let path = image.path()?;
            debug_active_image(&image);

            Ok((path, ErrorKind::BinaryExecError))
        }
        None => {
            let path = System::path()?;
            debug_no_platform();
            Ok((path, ErrorKind::NoPlatform))
        }
    }
}

fn validate_platform_yarn(platform: &Platform) -> Fallible<()> {
    match &platform.yarn {
        Some(_) => Ok(()),
        None => match platform.node.source {
            Source::Project => Err(ErrorKind::NoProjectYarn.into()),
            Source::Default | Source::Binary => Err(ErrorKind::NoDefaultYarn.into()),
            Source::CommandLine => Err(ErrorKind::NoCommandLineYarn.into()),
        },
    }
}
