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

**Focused:** Volta selects Node and package-manager versions while each package manager owns its global packages.

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

Global package commands are passed through to the selected package manager.
Install command-line tools with that package manager, for example:

```bash
pnpm add --global @openai/codex
```

The deprecated `volta install <package>` workflow has been removed. Existing
Volta-managed packages can still be executed and removed with `volta uninstall`
while they are migrated manually to the preferred package manager.

## Contributing to Volta

Issues and pull requests are welcome, but this fork's priorities follow the
maintainer's personal workflows. Before contributing, please read the
[code of conduct](CODE_OF_CONDUCT.md). The upstream
[Contributing Guide](https://docs.volta.sh/contributing/) remains useful for
development setup and repository conventions.
