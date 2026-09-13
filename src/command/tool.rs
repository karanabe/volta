use std::ffi::OsString;

use volta_core::error::{report_error, ExitCode, Fallible};
use volta_core::run::execute_tool_environment;
use volta_core::session::{ActivityKind, Session};
use volta_core::tool::environment::{self, InstallOptions, ToolPackageSpec};

use crate::command::Command;
use crate::common::{Error, IntoResult};

#[derive(clap::Args)]
pub(crate) struct Tool {
    #[command(subcommand)]
    command: ToolCommand,
}

#[derive(clap::Subcommand)]
enum ToolCommand {
    /// Installs a JavaScript CLI package in its own environment
    Install(Install),

    /// Uninstalls an isolated JavaScript CLI package
    Uninstall(Uninstall),

    /// Upgrades an isolated JavaScript CLI package from its recorded request
    Upgrade(Upgrade),

    /// Lists installed isolated JavaScript CLI packages
    List(List),

    /// Prints the executable path for an installed tool command
    Which(Which),

    /// Runs an installed package or command in its isolated environment
    Run(Run),
}

impl Command for Tool {
    fn run(self, session: &mut Session) -> Fallible<ExitCode> {
        match self.command {
            ToolCommand::Install(command) => command.run(session),
            ToolCommand::Uninstall(command) => command.run(session),
            ToolCommand::Upgrade(command) => command.run(session),
            ToolCommand::List(command) => command.run(session),
            ToolCommand::Which(command) => command.run(session),
            ToolCommand::Run(command) => command.run(session),
        }
    }
}

#[derive(clap::Args)]
struct Install {
    /// npm package specification, such as `eslint`, `eslint@9`, or `@scope/package@1.2.3`
    #[arg(value_name = "package-spec")]
    package: String,

    /// Node version request for this tool (defaults to the global Node version)
    #[arg(long, value_name = "version")]
    node: Option<String>,

    /// Allow a dependency to run its install/build scripts (repeatable)
    #[arg(long = "allow-build", value_name = "package")]
    allow_builds: Vec<String>,
}

impl Command for Install {
    fn run(self, session: &mut Session) -> Fallible<ExitCode> {
        session.add_event_start(ActivityKind::Install);
        let spec = ToolPackageSpec::parse(self.package)?;
        let node = self.node.map(|version| version.parse()).transpose()?;
        environment::install(
            spec,
            InstallOptions {
                node,
                allow_builds: self.allow_builds,
            },
            session,
        )?;
        session.add_event_end(ActivityKind::Install, ExitCode::Success);
        Ok(ExitCode::Success)
    }
}

#[derive(clap::Args)]
struct Upgrade {
    /// Installed package identity
    #[arg(
        value_name = "package",
        required_unless_present = "all",
        conflicts_with = "all"
    )]
    package: Option<String>,

    /// Upgrade every installed isolated tool
    #[arg(long)]
    all: bool,

    /// Change the Node version request while upgrading
    #[arg(long, value_name = "version")]
    node: Option<String>,
}

impl Command for Upgrade {
    fn run(self, session: &mut Session) -> Fallible<ExitCode> {
        session.add_event_start(ActivityKind::Upgrade);
        let packages = match self.package {
            Some(package) => vec![package],
            None => environment::installed_package_names()?,
        };
        for package in packages {
            let node = self
                .node
                .as_ref()
                .map(|version| version.parse())
                .transpose()?;
            environment::upgrade(&package, node, session)?;
        }
        session.add_event_end(ActivityKind::Upgrade, ExitCode::Success);
        Ok(ExitCode::Success)
    }
}

#[derive(clap::Args)]
struct Uninstall {
    /// Installed npm package name
    #[arg(value_name = "package")]
    package: String,
}

impl Command for Uninstall {
    fn run(self, session: &mut Session) -> Fallible<ExitCode> {
        session.add_event_start(ActivityKind::Uninstall);
        environment::uninstall(&self.package)?;
        session.add_event_end(ActivityKind::Uninstall, ExitCode::Success);
        Ok(ExitCode::Success)
    }
}

#[derive(clap::Args)]
struct List {}

impl Command for List {
    fn run(self, session: &mut Session) -> Fallible<ExitCode> {
        session.add_event_start(ActivityKind::List);
        for tool in environment::list()? {
            let status = if tool.runtime_available {
                String::new()
            } else {
                " BROKEN: missing Node runtime".to_owned()
            };
            println!(
                "{} (node@{}, installed by pnpm@{}) [{}]{}",
                volta_core::style::tool_version(&tool.package, &tool.version),
                tool.node,
                tool.installer,
                tool.executables.join(", "),
                status,
            );
        }
        session.add_event_end(ActivityKind::List, ExitCode::Success);
        Ok(ExitCode::Success)
    }
}

#[derive(clap::Args)]
struct Which {
    /// Executable name exposed by an installed tool
    #[arg(value_name = "command")]
    command: String,
}

impl Command for Which {
    fn run(self, session: &mut Session) -> Fallible<ExitCode> {
        session.add_event_start(ActivityKind::Which);
        println!("{}", environment::which(&self.command)?.display());
        session.add_event_end(ActivityKind::Which, ExitCode::Success);
        Ok(ExitCode::Success)
    }
}

#[derive(clap::Args)]
struct Run {
    /// Installed package identity or executable name
    #[arg(value_name = "package-or-command")]
    selector: String,

    /// Arguments passed to the tool
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    args: Vec<OsString>,
}

impl Command for Run {
    fn run(self, session: &mut Session) -> Fallible<ExitCode> {
        session.add_event_start(ActivityKind::Run);
        match execute_tool_environment(&self.selector, &self.args, session).into_result() {
            Ok(()) => {
                session.add_event_end(ActivityKind::Run, ExitCode::Success);
                Ok(ExitCode::Success)
            }
            Err(Error::Tool(code)) => {
                session.add_event_tool_end(ActivityKind::Run, code);
                Ok(ExitCode::ExecutionFailure)
            }
            Err(Error::Volta(error)) => {
                report_error(env!("CARGO_PKG_VERSION"), &error);
                session.add_event_error(ActivityKind::Run, &error);
                session.add_event_end(ActivityKind::Run, error.exit_code());
                Ok(error.exit_code())
            }
        }
    }
}
