//! Verify runtime archives before extraction and retain authenticated checksums
//! with the inventory so a previously verified download can be reused offline.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD, Engine};
use fs_utils::ensure_containing_dir_exists;
use node_semver::Version;
use sha2::{Digest, Sha256, Sha384, Sha512};
use tempfile::NamedTempFile;

use super::{download_tool_error, registry::public_registry_index, Spec};
use crate::error::{Context, ErrorKind, Fallible};
use crate::fs::{create_staging_file, remove_file_if_exists};
use crate::style::progress_spinner;

/// Checksums are compared using the strongest supported algorithm in an SRI
/// value. A valid weak digest must never override a failing stronger digest.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Algorithm {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Algorithm {
    fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha384 => "sha384",
            Self::Sha512 => "sha512",
        }
    }

    fn length(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }
}

pub(super) struct Integrity {
    algorithm: Algorithm,
    digests: Vec<Vec<u8>>,
}

impl Integrity {
    fn parse(value: &str) -> Option<Self> {
        let mut selected: Option<Self> = None;
        for token in value.split_whitespace() {
            let (algorithm, digest) = token.split_once('-')?;
            let algorithm = match algorithm {
                "sha1" => Algorithm::Sha1,
                "sha256" => Algorithm::Sha256,
                "sha384" => Algorithm::Sha384,
                "sha512" => Algorithm::Sha512,
                _ => continue,
            };
            let digest = digest.split('?').next()?;
            let digest = STANDARD.decode(digest).ok()?;
            if digest.len() != algorithm.length() {
                return None;
            }
            match &mut selected {
                Some(current) if current.algorithm > algorithm => {}
                Some(current) if current.algorithm == algorithm => current.digests.push(digest),
                _ => {
                    selected = Some(Self {
                        algorithm,
                        digests: vec![digest],
                    });
                }
            }
        }
        selected
    }

    fn from_hex(algorithm: Algorithm, value: &str) -> Option<Self> {
        if value.len() != algorithm.length() * 2 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let digest = (0..value.len())
            .step_by(2)
            .map(|offset| u8::from_str_radix(&value[offset..offset + 2], 16).ok())
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            algorithm,
            digests: vec![digest],
        })
    }

    fn serialize(&self) -> String {
        self.digests
            .iter()
            .map(|digest| format!("{}-{}", self.algorithm.name(), STANDARD.encode(digest)))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn matches(&self, path: &Path) -> io::Result<bool> {
        fn digest<D: Digest + Default>(path: &Path) -> io::Result<Vec<u8>> {
            let mut file = File::open(path)?;
            let mut hasher = D::default();
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    return Ok(hasher.finalize().to_vec());
                }
                hasher.update(&buffer[..read]);
            }
        }
        let actual = match self.algorithm {
            Algorithm::Sha1 => digest::<sha1::Sha1>(path)?,
            Algorithm::Sha256 => digest::<Sha256>(path)?,
            Algorithm::Sha384 => digest::<Sha384>(path)?,
            Algorithm::Sha512 => digest::<Sha512>(path)?,
        };
        Ok(self.digests.contains(&actual))
    }
}

pub(super) fn node_integrity(url: &str, filename: &str) -> Fallible<Integrity> {
    let text = attohttpc::get(url)
        .send()
        .and_then(attohttpc::Response::error_for_status)
        .and_then(attohttpc::Response::text)
        .with_context(super::registry_fetch_error("Node checksums", url))?;
    parse_node_integrity(&text, filename).ok_or_else(|| integrity_error(filename))
}

fn parse_node_integrity(text: &str, filename: &str) -> Option<Integrity> {
    let mut matches = text.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        let digest = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        (name == filename && fields.next().is_none()).then_some(digest)
    });
    let digest = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Integrity::from_hex(Algorithm::Sha256, digest)
}

pub(super) fn npm_integrity(package: &str, version: &Version) -> Fallible<Integrity> {
    // Runtime mirrors must distribute the official package bytes. Keep the
    // checksum authority independent of the configurable archive location.
    let url = format!("{}/{}", public_registry_index(package), version);
    #[derive(serde::Deserialize)]
    struct Metadata {
        name: String,
        version: String,
        dist: Distribution,
    }
    #[derive(serde::Deserialize)]
    struct Distribution {
        integrity: Option<String>,
        shasum: Option<String>,
    }
    let metadata: Metadata = attohttpc::get(&url)
        .send()
        .and_then(attohttpc::Response::error_for_status)
        .and_then(attohttpc::Response::json)
        .with_context(super::registry_fetch_error(package, &url))?;
    let label = format!("{package}@{version}");
    if metadata.name != package || metadata.version != version.to_string() {
        return Err(integrity_error(&label));
    }
    // SHA-1 is only a compatibility fallback for old releases lacking SRI.
    // Malformed or unsupported SRI must fail, even when shasum is valid.
    match metadata.dist.integrity {
        Some(value) => Integrity::parse(&value),
        None => metadata
            .dist
            .shasum
            .and_then(|value| Integrity::from_hex(Algorithm::Sha1, &value)),
    }
    .ok_or_else(|| integrity_error(&label))
}

/// A verified archive that is committed to inventory only after successful
/// extraction. The caller holds the Volta mutation lock throughout its lifetime.
pub(super) struct VerifiedDownload {
    staging: Option<NamedTempFile>,
    cache: PathBuf,
    integrity: Integrity,
    tool: String,
}

impl VerifiedDownload {
    pub(super) fn fetch(
        tool: Spec,
        cache: PathBuf,
        remote_url: impl FnOnce() -> Fallible<String>,
        expected: impl Fn() -> Fallible<Integrity>,
    ) -> Fallible<Self> {
        let label = tool.to_string();
        let cached_integrity = fs::read_to_string(checksum_path(&cache))
            .ok()
            .and_then(|value| Integrity::parse(value.trim()));
        if cache.is_file() {
            let integrity = match cached_integrity {
                Some(integrity) => integrity,
                None => expected()?,
            };
            if integrity.matches(&cache).unwrap_or(false) {
                return Ok(Self {
                    staging: None,
                    cache,
                    integrity,
                    tool: label,
                });
            }
        }

        let url = remote_url()?;
        log::debug!("Downloading {} from {}", label, url);
        let mut staging = create_staging_file()?;
        let spinner = progress_spinner(format!("Downloading {label}"));
        attohttpc::get(&url)
            .send()
            .and_then(attohttpc::Response::error_for_status)
            .and_then(|response| response.write_to(staging.as_file_mut()))
            .with_context(download_tool_error(tool, &url))?;
        spinner.finish_and_clear();
        let integrity = expected()?;
        if !integrity
            .matches(staging.path())
            .with_context(|| ErrorKind::ArchiveIntegrityError {
                tool: label.clone(),
            })?
        {
            return Err(integrity_error(&label));
        }
        Ok(Self {
            staging: Some(staging),
            cache,
            integrity,
            tool: label,
        })
    }

    pub(super) fn open(&self) -> Fallible<File> {
        let path = self
            .staging
            .as_ref()
            .map_or(self.cache.as_path(), |staging| staging.path());
        File::open(path).with_context(|| ErrorKind::ArchiveIntegrityError {
            tool: self.tool.clone(),
        })
    }

    pub(super) fn persist(self) -> Fallible<()> {
        let context = || ErrorKind::PersistInventoryError {
            tool: self.tool.clone(),
        };
        ensure_containing_dir_exists(&self.cache).with_context(context)?;
        if let Some(staging) = self.staging {
            staging.persist(&self.cache).with_context(context)?;
        }
        let mut checksum =
            NamedTempFile::new_in(self.cache.parent().expect("archive has a parent"))
                .with_context(context)?;
        checksum
            .write_all(self.integrity.serialize().as_bytes())
            .with_context(context)?;
        checksum
            .persist(checksum_path(&self.cache))
            .with_context(context)?;
        Ok(())
    }
}

fn checksum_path(archive: &Path) -> PathBuf {
    let mut name = archive.as_os_str().to_owned();
    name.push(".integrity");
    PathBuf::from(name)
}

pub(super) fn remove_cached_archive(archive: &Path) -> Fallible<()> {
    remove_file_if_exists(archive)?;
    remove_file_if_exists(checksum_path(archive))
}

fn integrity_error(tool: &str) -> crate::error::VoltaError {
    ErrorKind::ArchiveIntegrityError {
        tool: tool.to_owned(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn node_checksums_require_an_exact_unambiguous_filename() {
        let text = format!("{ABC_SHA256}  node.tar.gz.extra\n{ABC_SHA256} *node.tar.gz\n");
        assert!(parse_node_integrity(&text, "node.tar.gz").is_some());
        assert!(parse_node_integrity(&text, "node.zip").is_none());
        assert!(parse_node_integrity(&(text.clone() + &text), "node.tar.gz").is_none());
        assert!(parse_node_integrity("invalid  node.tar.gz", "node.tar.gz").is_none());
    }

    #[test]
    fn verifies_complete_files_and_rejects_changed_bytes() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"abc").unwrap();
        let integrity = Integrity::from_hex(Algorithm::Sha256, ABC_SHA256).unwrap();
        assert!(integrity.matches(file.path()).unwrap());
        file.write_all(b"trailing bytes").unwrap();
        assert!(!integrity.matches(file.path()).unwrap());
    }

    #[test]
    fn sri_uses_the_strongest_algorithm_and_accepts_any_of_its_digests() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"abc").unwrap();
        let weak = format!("sha256-{}", STANDARD.encode(Sha256::digest(b"abc")));
        let wrong = format!("sha512-{}", STANDARD.encode(Sha512::digest(b"wrong")));
        let valid = format!("sha512-{}", STANDARD.encode(Sha512::digest(b"abc")));
        assert!(!Integrity::parse(&format!("{weak} {wrong}"))
            .unwrap()
            .matches(file.path())
            .unwrap());
        let integrity = Integrity::parse(&format!("{wrong} {weak} {valid}")).unwrap();
        assert!(integrity.matches(file.path()).unwrap());
        assert!(Integrity::parse(&integrity.serialize())
            .unwrap()
            .matches(file.path())
            .unwrap());
    }

    #[test]
    fn rejects_invalid_and_unsupported_integrity_values() {
        for value in ["", "sha512-bad", "sha512-%%%", "sha999-YWJj", "sha256-"] {
            assert!(Integrity::parse(value).is_none(), "{value}");
        }
        assert!(Integrity::from_hex(Algorithm::Sha256, &"é".repeat(32)).is_none());
    }

    #[test]
    fn supports_legacy_sha1_checksums() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"abc").unwrap();
        let integrity =
            Integrity::from_hex(Algorithm::Sha1, "a9993e364706816aba3e25717850c26c9cd0d89d")
                .unwrap();
        assert!(integrity.matches(file.path()).unwrap());
        assert!(Integrity::parse(&integrity.serialize())
            .unwrap()
            .matches(file.path())
            .unwrap());
    }
}
