//! Immutable, content-addressed storage for unpacked npm package contents.
//!
//! This module deliberately knows nothing about dependency graphs, `node_modules`
//! resolution, executable exposure, or tool-environment layouts. Those concerns
//! belong to [`super::environment`].

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use log::debug;
use sha2::{Digest, Sha256};
use tempfile::tempdir_in;

use crate::error::{ErrorKind, Fallible};

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub(crate) struct ContentHash(String);

impl ContentHash {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PackageArtifact {
    pub(crate) content_hash: ContentHash,
    pub(crate) path: PathBuf,
}

/// Volta-owned storage for immutable package contents.
///
/// Entries are addressed only by a digest of their filesystem content. Package
/// names, versions, dependency edges, and install locations are intentionally
/// absent from this abstraction.
pub(crate) struct PackageStore {
    packages: PathBuf,
    temporary: PathBuf,
}

impl PackageStore {
    pub(crate) fn new(packages: PathBuf, temporary: PathBuf) -> Self {
        Self {
            packages,
            temporary,
        }
    }

    /// Add one package's files to the store, or reuse an identical existing entry.
    ///
    /// Nested `node_modules` directories are excluded because they express the
    /// dependency topology of an environment, not the contents of this package.
    pub(crate) fn intern(&self, source: &Path) -> Fallible<PackageArtifact> {
        let started = Instant::now();
        fs::create_dir_all(&self.packages)
            .and_then(|_| fs::create_dir_all(&self.temporary))
            .map_err(|error| store_error(&self.packages, error))?;

        let staging =
            tempdir_in(&self.temporary).map_err(|error| store_error(&self.temporary, error))?;
        copy_package_contents(source, staging.path())?;
        // Hash the representation that will actually be stored. On Windows,
        // package-internal symlinks may have been converted to copies when the
        // OS does not permit creating symlinks.
        let content_hash = hash_package(staging.path())?;
        if let Err(error) = make_files_read_only(staging.path()) {
            let staging_path = staging.keep();
            let _ = remove_tree(&staging_path);
            return Err(error);
        }
        let destination = self.packages.join(content_hash.as_str());

        if destination.exists() {
            verify_entry(&destination, &content_hash)?;
            let staging_path = staging.keep();
            remove_tree(&staging_path).map_err(|error| store_error(&staging_path, error))?;
            debug!(
                "Package store hit {} ({:?})",
                content_hash.as_str(),
                started.elapsed()
            );
            return Ok(PackageArtifact {
                content_hash,
                path: destination,
            });
        }

        let staging_path = staging.keep();
        match fs::rename(&staging_path, &destination) {
            Ok(()) => {}
            Err(_error) if destination.exists() => {
                // Another process may have published the same digest. Its entry
                // must still be verified before it is trusted.
                let _ = remove_tree(&staging_path);
                verify_entry(&destination, &content_hash)?;
            }
            Err(error) => {
                let _ = remove_tree(&staging_path);
                return Err(store_error(&destination, error));
            }
        }

        debug!(
            "Published package store entry {} ({:?})",
            content_hash.as_str(),
            started.elapsed()
        );

        Ok(PackageArtifact {
            content_hash,
            path: destination,
        })
    }
}

fn verify_entry(path: &Path, expected: &ContentHash) -> Fallible<()> {
    let actual = hash_package(path)?;
    if &actual == expected {
        Ok(())
    } else {
        Err(ErrorKind::PackageStoreCorrupt {
            content_hash: expected.0.clone(),
        }
        .into())
    }
}

fn hash_package(root: &Path) -> Fallible<ContentHash> {
    let mut hasher = Sha256::new();
    // Domain-separate the digest and version its serialization so future
    // formats cannot accidentally be mistaken for entries produced here.
    hasher.update(b"volta-package-store-v1\0");
    hash_directory(root, root, &mut hasher)?;
    Ok(ContentHash(format!("{:x}", hasher.finalize())))
}

fn hash_directory(root: &Path, directory: &Path, hasher: &mut Sha256) -> Fallible<()> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| store_error(directory, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| store_error(directory, error))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        if entry.file_name() == OsStr::new("node_modules") {
            continue;
        }

        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("walked store path remains beneath its root");
        let relative = relative
            .to_str()
            .ok_or_else(|| store_message(&path, "package path is not valid UTF-8"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| store_error(&path, error))?;

        if file_type.is_dir() {
            hash_header(hasher, b'd', relative);
            hash_directory(root, &path, hasher)?;
        } else if file_type.is_file() {
            hash_header(hasher, b'f', relative);
            hash_file(&path, hasher)?;
            hash_executable_bit(&path, hasher)?;
        } else if file_type.is_symlink() {
            hash_header(hasher, b'l', relative);
            let target = validated_symlink_target(root, &path)?;
            hash_bytes(hasher, target.to_string_lossy().as_bytes());
        } else {
            return Err(store_message(&path, "unsupported filesystem entry type"));
        }
    }

    Ok(())
}

fn hash_header(hasher: &mut Sha256, kind: u8, relative: &str) {
    hasher.update([kind]);
    hash_bytes(hasher, relative.as_bytes());
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hash_file(path: &Path, hasher: &mut Sha256) -> Fallible<()> {
    let mut file = File::open(path).map_err(|error| store_error(path, error))?;
    let length = file
        .metadata()
        .map_err(|error| store_error(path, error))?
        .len();
    hasher.update(length.to_le_bytes());
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| store_error(path, error))?;
        if read == 0 {
            return Ok(());
        }
        hasher.update(&buffer[..read]);
    }
}

#[cfg(unix)]
fn hash_executable_bit(path: &Path, hasher: &mut Sha256) -> Fallible<()> {
    let mode = fs::metadata(path)
        .map_err(|error| store_error(path, error))?
        .permissions()
        .mode();
    hasher.update([u8::from(mode & 0o111 != 0)]);
    Ok(())
}

#[cfg(windows)]
fn hash_executable_bit(_path: &Path, hasher: &mut Sha256) -> Fallible<()> {
    hasher.update([0]);
    Ok(())
}

fn copy_package_contents(source: &Path, destination: &Path) -> Fallible<()> {
    copy_package_directory(source, source, destination)
}

fn copy_package_directory(package_root: &Path, source: &Path, destination: &Path) -> Fallible<()> {
    let mut entries = fs::read_dir(source)
        .map_err(|error| store_error(source, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| store_error(source, error))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        if entry.file_name() == OsStr::new("node_modules") {
            continue;
        }

        let from = entry.path();
        let to = destination.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| store_error(&from, error))?;
        if file_type.is_dir() {
            fs::create_dir(&to).map_err(|error| store_error(&to, error))?;
            copy_package_directory(package_root, &from, &to)?;
        } else if file_type.is_file() {
            fs::copy(&from, &to).map_err(|error| store_error(&to, error))?;
        } else if file_type.is_symlink() {
            let target = validated_symlink_target(package_root, &from)?;
            create_symlink(package_root, &target, &to, &from)?;
        } else {
            return Err(store_message(&from, "unsupported filesystem entry type"));
        }
    }

    Ok(())
}

fn validated_symlink_target(package_root: &Path, link: &Path) -> Fallible<PathBuf> {
    let target = fs::read_link(link).map_err(|error| store_error(link, error))?;
    if target.to_str().is_none() {
        return Err(store_message(
            link,
            "package symlink target is not valid UTF-8",
        ));
    }
    if target.is_absolute() {
        return Err(store_message(
            link,
            "absolute package symlink is not allowed",
        ));
    }

    let parent = link
        .parent()
        .expect("a package entry always has a parent directory");
    let resolved = parent.join(&target);
    let mut depth = 0_usize;
    for component in resolved
        .strip_prefix(package_root)
        .unwrap_or(&resolved)
        .components()
    {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(store_message(link, "package symlink escapes its package"));
            }
        }
    }
    Ok(target)
}

#[cfg(unix)]
fn create_symlink(
    _package_root: &Path,
    target: &Path,
    destination: &Path,
    source: &Path,
) -> Fallible<()> {
    std::os::unix::fs::symlink(target, destination).map_err(|error| store_error(source, error))
}

#[cfg(windows)]
fn create_symlink(
    package_root: &Path,
    target: &Path,
    destination: &Path,
    source: &Path,
) -> Fallible<()> {
    let resolved = source
        .parent()
        .expect("a package symlink always has a parent")
        .join(target);
    if resolved.is_dir() {
        match std::os::windows::fs::symlink_dir(target, destination) {
            Ok(()) => Ok(()),
            Err(_) => {
                fs::create_dir(destination).map_err(|error| store_error(destination, error))?;
                copy_package_directory(package_root, &resolved, destination)
            }
        }
    } else {
        match std::os::windows::fs::symlink_file(target, destination) {
            Ok(()) => Ok(()),
            Err(_) => fs::copy(&resolved, destination)
                .map(|_| ())
                .map_err(|error| store_error(source, error)),
        }
    }
}

fn make_files_read_only(root: &Path) -> Fallible<()> {
    for entry in fs::read_dir(root)
        .map_err(|error| store_error(root, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| store_error(root, error))?
    {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| store_error(&path, error))?;
        if file_type.is_dir() {
            make_files_read_only(&path)?;
        } else if file_type.is_file() {
            let mut permissions = fs::metadata(&path)
                .map_err(|error| store_error(&path, error))?
                .permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&path, permissions).map_err(|error| store_error(&path, error))?;
        }
    }
    Ok(())
}

fn remove_tree(path: &Path) -> io::Result<()> {
    if path.exists() {
        #[cfg(windows)]
        make_tree_writable(path)?;
        fs::remove_dir_all(path)
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn make_tree_writable(path: &Path) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path)?;
        if metadata.is_dir() {
            make_tree_writable(&entry_path)?;
        } else if metadata.is_file() {
            let mut permissions = metadata.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(entry_path, permissions)?;
        }
    }
    Ok(())
}

fn store_error(path: &Path, error: io::Error) -> crate::error::VoltaError {
    store_message(path, &error.to_string())
}

fn store_message(path: &Path, message: &str) -> crate::error::VoltaError {
    ErrorKind::PackageStoreError {
        path: path.to_owned(),
        message: message.to_owned(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Arc, Barrier};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn identical_contents_reuse_one_entry() {
        let temp = tempdir().expect("temp directory");
        let source_a = temp.path().join("a");
        let source_b = temp.path().join("b");
        fs::create_dir_all(&source_a).expect("create package a");
        fs::create_dir_all(&source_b).expect("create package b");
        fs::write(source_a.join("index.js"), "same").expect("write package a");
        fs::write(source_b.join("index.js"), "same").expect("write package b");

        let store = PackageStore::new(
            temp.path().join("store/packages"),
            temp.path().join("store/tmp"),
        );
        let a = store.intern(&source_a).expect("intern package a");
        let b = store.intern(&source_b).expect("intern package b");

        assert_eq!(a.content_hash, b.content_hash);
        assert_eq!(a.path, b.path);
    }

    #[test]
    fn dependency_topology_is_not_part_of_stored_content() {
        let temp = tempdir().expect("temp directory");
        let source_a = temp.path().join("a");
        let source_b = temp.path().join("b");
        for source in [&source_a, &source_b] {
            fs::create_dir_all(source.join("node_modules/dependency")).expect("create dependency");
            fs::write(source.join("index.js"), "same").expect("write package");
        }
        fs::write(
            source_a.join("node_modules/dependency/index.js"),
            "version one",
        )
        .expect("write dependency one");
        fs::write(
            source_b.join("node_modules/dependency/index.js"),
            "version two",
        )
        .expect("write dependency two");

        let store = PackageStore::new(
            temp.path().join("store/packages"),
            temp.path().join("store/tmp"),
        );
        let a = store.intern(&source_a).expect("intern package a");
        let b = store.intern(&source_b).expect("intern package b");

        assert_eq!(a.content_hash, b.content_hash);
        assert!(!a.path.join("node_modules").exists());
    }

    #[test]
    fn file_boundaries_are_part_of_content_identity() {
        let temp = tempdir().expect("temp directory");
        let source_a = temp.path().join("a");
        let source_b = temp.path().join("b");
        fs::create_dir_all(&source_a).expect("create package a");
        fs::create_dir_all(&source_b).expect("create package b");

        // Without a file-length prefix, these serialize to the same hash
        // stream: the bytes in a/a can impersonate a second file header.
        let mut boundary_bytes = vec![0_u8, b'f'];
        boundary_bytes.extend_from_slice(&1_u64.to_le_bytes());
        boundary_bytes.push(b'b');
        fs::write(source_a.join("a"), boundary_bytes).expect("write package a");
        fs::write(source_b.join("a"), []).expect("write first package b file");
        fs::write(source_b.join("b"), []).expect("write second package b file");

        let store = PackageStore::new(
            temp.path().join("store/packages"),
            temp.path().join("store/tmp"),
        );
        let a = store.intern(&source_a).expect("intern package a");
        let b = store.intern(&source_b).expect("intern package b");

        assert_ne!(a.content_hash, b.content_hash);
    }

    #[test]
    fn corrupted_existing_entry_is_rejected() {
        let temp = tempdir().expect("temp directory");
        let source = temp.path().join("source");
        fs::create_dir_all(&source).expect("create package");
        fs::write(source.join("index.js"), "original").expect("write package");

        let store = PackageStore::new(
            temp.path().join("store/packages"),
            temp.path().join("store/tmp"),
        );
        let artifact = store.intern(&source).expect("intern package");
        let path = artifact.path.join("index.js");
        let mut permissions = fs::metadata(&path).expect("metadata").permissions();
        #[cfg(unix)]
        permissions.set_mode(permissions.mode() | 0o200);
        #[cfg(windows)]
        {
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
        }
        fs::set_permissions(&path, permissions).expect("make writable");
        fs::write(path, "corrupt").expect("corrupt package");

        let error = store.intern(&source).expect_err("corruption must fail");
        assert!(matches!(
            error.kind(),
            ErrorKind::PackageStoreCorrupt { .. }
        ));
    }

    #[test]
    fn concurrent_population_publishes_one_verified_entry() {
        let temp = tempdir().expect("temp directory");
        let source = temp.path().join("source");
        fs::create_dir_all(&source).expect("create package");
        fs::write(source.join("index.js"), "shared content").expect("write package");

        let store = Arc::new(PackageStore::new(
            temp.path().join("store/packages"),
            temp.path().join("store/tmp"),
        ));
        let source = Arc::new(source);
        let barrier = Arc::new(Barrier::new(4));
        let threads = (0..4)
            .map(|_| {
                let store = Arc::clone(&store);
                let source = Arc::clone(&source);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store.intern(&source).expect("concurrent intern").path
                })
            })
            .collect::<Vec<_>>();
        let paths = threads
            .into_iter()
            .map(|thread| thread.join().expect("intern thread"))
            .collect::<Vec<_>>();

        assert!(paths.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(
            fs::read_dir(temp.path().join("store/packages"))
                .expect("store packages")
                .count(),
            1
        );
    }
}
