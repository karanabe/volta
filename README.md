# Volta

<p align="center">
  The Hassle-Free JavaScript Tool Manager
</p>

<p align="center">
  <a href="https://github.com/karanabe/volta/actions/workflows/release.yml">
    <img alt="Production Build Status" src="https://github.com/karanabe/volta/actions/workflows/release.yml/badge.svg" />
  </a>
</p>

---

> [!IMPORTANT]
> **This fork continues development for personal use.** The upstream
> [`volta-cli/volta`](https://github.com/volta-cli/volta) project is no longer
> maintained. This fork is kept current for the maintainer's personal workflows;
> compatibility and support for other environments are not guaranteed.

---


**Fast:** Install and run any JS tool quickly and seamlessly! Volta is built in Rust and ships as a snappy native binary.

**Reliable:** Ensure everyone in your project has the same tools—without interfering with their workflow.

**Focused:** Volta manages runtimes, package managers, and isolated environments for JavaScript CLI tools.

## Features

- Speed 🚀
- Seamless, per-project version switching
- Cross-platform support, including Windows and all Unix shells
- Support for multiple package managers
- Stable tool installation—no reinstalling on every Node upgrade!
- Extensibility hooks for site-specific customization

## Installing and updating this fork

On Linux or macOS, download and run the installer published with the latest
[GitHub Release](https://github.com/karanabe/volta/releases/latest):

```bash
curl --proto '=https' --tlsv1.2 -fsSLO \
  https://github.com/karanabe/volta/releases/latest/download/volta-install.sh
bash volta-install.sh
rm volta-install.sh
```

Run the same commands again to update an existing installation. To install a
specific release, pass its version without the leading `v` when running the
downloaded script, for example `bash volta-install.sh --version 2.0.3`.

On Windows, download the latest
[x86-64 MSI](https://github.com/karanabe/volta/releases/latest/download/volta-windows-x86_64.msi)
or
[ARM64 MSI](https://github.com/karanabe/volta/releases/latest/download/volta-windows-arm64.msi).

The Unix installer verifies the selected archive against the `SHA256SUMS` file
from the same release before extracting it. See the upstream
[Getting Started Guide](https://docs.volta.sh/guide/getting-started) for shell
setup and platform details that still apply to this fork.

## Using Volta

Read the upstream [Understanding Volta Guide](https://docs.volta.sh/guide/understanding)
for the core concepts and project pinning workflow. pnpm support is enabled by
default in this fork; the former `VOLTA_FEATURE_PNPM` environment variable is
no longer needed.

Use Volta to install and pin runtimes and package managers:

```bash
volta install node@lts pnpm@latest
volta pin node@lts pnpm@latest
```

Install JavaScript command-line tools with `volta tool install`. Each tool gets
its own dependency environment while identical package contents are reused from
Volta's shared content-addressed store:

```bash
volta tool install eslint
volta tool install prettier@3
volta tool install @openai/codex
```

Manage and run those tools with:

```bash
volta tool list
volta tool which eslint
volta tool run eslint -- --fix .
volta tool uninstall eslint
```

The Node runtime selected when a tool is installed is persisted with its
environment. A current project's pinned runtime is selected first; otherwise
Volta uses the default runtime. Tool execution never falls back to an arbitrary
`node` from `PATH`, and a project-local executable continues to take precedence
over an installed global tool.

The deprecated `volta install <package>` workflow has been replaced by
`volta tool install <package>`. Existing packages from the legacy Volta layout
are not modified automatically: they can still be executed and removed with
`volta uninstall` while they are migrated explicitly.

## Contributing to Volta

Issues and pull requests are welcome, but this fork's priorities follow the
maintainer's personal workflows. Before contributing, please read the
[code of conduct](CODE_OF_CONDUCT.md). The upstream
[Contributing Guide](https://docs.volta.sh/contributing/) remains useful for
development setup and repository conventions.

## Acknowledgements

Volta was originally developed by David Herman and Charles Pierce. The original
project and its contributors developed and maintained Volta through November
2025. Since September 2026, this fork has been independently continued and
maintained by an individual for personal use.
