# Compatibility

This fork is maintained for personal use, primarily on WSL2/Linux x86-64.
Compatibility outside the maintainer's workflows is not guaranteed. The
upstream project's historical OS minimums and SemVer support promises are not
independently verified by this fork.

The current [test workflow](.github/workflows/test.yml) runs workspace and
acceptance tests on `ubuntu-latest`, `macos-latest`, and `windows-latest`.
Real-network smoke tests run on Ubuntu and cover Current/LTS Node, npm/npx,
pnpm, Yarn, and pnpm-backed isolated CLI environments. The isolated-environment
acceptance fixtures currently run on Unix only.

The [release workflow](.github/workflows/release.yml) builds:

- Linux x86-64 and ARM64 archives using cross-rs.
- macOS universal archives for x86-64 and Apple Silicon.
- Windows x86-64 and ARM64 installers and archives.

Release builds depend on the test workflow. Producing an artifact does not
prove compatibility with every older OS version or with every Node/package
manager version. The exact Rust toolchain is recorded in
[rust-toolchain.toml](rust-toolchain.toml).

All workspace packages use Rust Edition 2024. Build and test with the pinned
toolchain; this fork does not declare or test a separate minimum supported Rust
version. [rustfmt.toml](rustfmt.toml) retains the existing formatting style
independently of the language edition.
