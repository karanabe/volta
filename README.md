# Volta

Volta manages Node.js, package managers, and JavaScript CLI tools, with automatic
per-project version switching.

[![Build status](https://github.com/karanabe/volta/actions/workflows/release.yml/badge.svg)](https://github.com/karanabe/volta/actions/workflows/release.yml)

> [!NOTE]
> This is a personal-use fork of [volta-cli/volta](https://github.com/volta-cli/volta).
> Support outside the maintainer's workflows is not guaranteed.
> See [Compatibility](COMPATIBILITY.md) for platform details.

## Quick start

### 1. Install Volta

**Linux / macOS:** Download and run this fork's installer:

```bash
curl --proto '=https' --tlsv1.2 -fsSLO \
  https://github.com/karanabe/volta/releases/latest/download/volta-install.sh &&
bash volta-install.sh
rm volta-install.sh
```

**Windows:** Download and run the
[x86-64 MSI](https://github.com/karanabe/volta/releases/latest/download/volta-windows-x86_64.msi)
or
[ARM64 MSI](https://github.com/karanabe/volta/releases/latest/download/volta-windows-arm64.msi).

To update Volta later, run the installer again.

### 2. Open a new terminal

The installer configures your shell. Close and reopen your terminal, then check:

```sh
volta --version
```

### 3. Install Node.js

```sh
volta install node@lts
node --version
npm --version
```

You're ready to use Node.js and its bundled npm. No separate Node installation
or manual version switching is needed.

## Everyday use

### Use pnpm (optional)

```sh
volta install pnpm@latest
pnpm --version
```

pnpm support is enabled by default in Volta 3; no feature flag is needed.

### Pin a project's Node version (optional)

Run this inside a project directory containing `package.json`:

```sh
volta pin node@lts
```

Volta saves the resolved version in `package.json` and uses it automatically
whenever you work in that project. Commit the file to share the version with
your team.

### Install a CLI tool (Volta 3)

Use `volta tool install` for CLI packages, instead of `volta install`:

```sh
volta tool install prettier
prettier --version
```

Tools get their own environments; Volta installs the pnpm backend automatically.
Run `volta tool --help` for upgrades, removal, and other tool commands.

## More information

- `volta --help` — available commands
- [Upstream guide](https://docs.volta.sh/guide/understanding) — core concepts
- [Release notes](RELEASES.md) — changes and migration notes for this fork
- [Contributing](CONTRIBUTING.md) and [Code of Conduct](CODE_OF_CONDUCT.md)

## Acknowledgements

Volta was originally developed by David Herman, Charles Pierce, and the
[upstream contributors](https://github.com/volta-cli/volta/graphs/contributors).
