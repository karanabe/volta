use volta_core::error::{ErrorKind, ExitCode, Fallible};
use volta_core::session::{ActivityKind, Session};
use volta_core::tool;
use volta_core::version::VersionSpec;

use crate::command::Command;

#[derive(clap::Args)]
pub(crate) struct Uninstall {
    /// The tool to uninstall, such as `node@22.20.0`, `pnpm`, or `typescript`
    tool: String,

    /// Remove Node even when isolated tools refer to it
    #[arg(long)]
    force: bool,
}

impl Command for Uninstall {
    fn run(self, session: &mut Session) -> Fallible<ExitCode> {
        session.add_event_start(ActivityKind::Uninstall);

        let tool = tool::Spec::try_from_str(&self.tool)?;

        // Package removal names the installed package identity; inventory
        // removal for runtimes and package managers accepts exact versions.
        if let tool::Spec::Package(_name, version) = &tool {
            let VersionSpec::None = version else {
                return Err(ErrorKind::Unimplemented {
                    feature: "uninstalling specific versions of tools".into(),
                }
                .into());
            };
        }

        tool.uninstall(self.force, session)?;

        session.add_event_end(ActivityKind::Uninstall, ExitCode::Success);
        Ok(ExitCode::Success)
    }
}
