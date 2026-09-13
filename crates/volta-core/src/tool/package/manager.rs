use std::path::PathBuf;

/// The package manager used to install a given package
#[derive(
    Copy, Clone, serde::Serialize, serde::Deserialize, PartialOrd, Ord, PartialEq, Eq, Debug,
)]
pub enum PackageManager {
    Npm,
    Pnpm,
    Yarn,
}

impl PackageManager {
    /// Given the `package_root`, returns the directory where the source is stored for this
    /// package manager. This will include the top-level `node_modules`, where appropriate.
    pub fn source_dir(self, package_root: PathBuf) -> PathBuf {
        let mut path = self.source_root(package_root);
        path.push("node_modules");

        path
    }

    /// Given the `package_root`, returns the root of the source directory. This directory will
    /// contain the top-level `node-modules`
    #[cfg(unix)]
    pub fn source_root(self, package_root: PathBuf) -> PathBuf {
        let mut path = package_root;
        match self {
            // On Unix, the source is always within a `lib` subdirectory, with both npm and Yarn
            PackageManager::Npm | PackageManager::Yarn => path.push("lib"),
            // pnpm puts the source node_modules directory in the global-dir
            // plus a versioned subdirectory.
            // FIXME: Here the subdirectory is hard-coded, I don't know if it's
            // possible to retrieve it from pnpm dynamically.
            PackageManager::Pnpm => path.push("5"),
        }

        path
    }

    /// Given the `package_root`, returns the root of the source directory. This directory will
    /// contain the top-level `node-modules`
    #[cfg(windows)]
    pub fn source_root(self, package_root: PathBuf) -> PathBuf {
        match self {
            // On Windows, npm puts the source node_modules directory in the root of the `prefix`
            PackageManager::Npm => package_root,
            // On Windows, we still tell yarn to use the `lib` subdirectory
            PackageManager::Yarn => {
                let mut path = package_root;
                path.push("lib");
                path
            }
            // pnpm puts the source node_modules directory in the global-dir
            // plus a versioned subdirectory.
            // FIXME: Here the subdirectory is hard-coded, I don't know if it's
            // possible to retrieve it from pnpm dynamically.
            PackageManager::Pnpm => {
                let mut path = package_root;
                path.push("5");
                path
            }
        }
    }

    /// Given the `package_root`, returns the directory where binaries are stored for this package
    /// manager.
    #[cfg(unix)]
    pub fn binary_dir(self, package_root: PathBuf) -> PathBuf {
        // On Unix, the binaries are always within a `bin` subdirectory for both npm and Yarn
        let mut path = package_root;
        path.push("bin");

        path
    }

    /// Given the `package_root`, returns the directory where binaries are stored for this package
    /// manager.
    #[cfg(windows)]
    pub fn binary_dir(self, package_root: PathBuf) -> PathBuf {
        match self {
            // On Windows, npm leaves the binaries at the root of the `prefix` directory
            PackageManager::Npm => package_root,
            // On Windows, Yarn still includes the `bin` subdirectory. pnpm by
            // default generates binaries into the `PNPM_HOME` path
            PackageManager::Yarn | PackageManager::Pnpm => {
                let mut path = package_root;
                path.push("bin");
                path
            }
        }
    }
}
