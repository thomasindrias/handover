use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::model::ContentRef;
use crate::store::atomic::{create_private, read_private};

const INLINE_LIMIT_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(session_dir: &Path) -> Self {
        Self {
            root: session_dir.join("blobs/sha256"),
        }
    }

    pub fn put(&self, bytes: &[u8]) -> Result<ContentRef> {
        if bytes.len() <= INLINE_LIMIT_BYTES {
            if let Ok(text) = std::str::from_utf8(bytes) {
                return Ok(ContentRef::Inline {
                    text: text.to_owned(),
                });
            }
        }

        let sha256 = hex::encode(Sha256::digest(bytes));
        let path = self.path_for_hash(&sha256)?;
        match create_private(&path, bytes) {
            Ok(()) => {}
            Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                self.verify_blob(&path, &sha256, bytes.len())?;
            }
            Err(error) => return Err(error),
        }

        Ok(ContentRef::Blob {
            sha256,
            bytes: bytes.len(),
        })
    }

    pub fn resolve(&self, content: &ContentRef) -> Result<Vec<u8>> {
        match content {
            ContentRef::Inline { text } => Ok(text.as_bytes().to_vec()),
            ContentRef::Blob { sha256, bytes } => {
                let path = self.path_for_hash(sha256)?;
                self.verify_blob(&path, sha256, *bytes)
            }
        }
    }

    fn verify_blob(&self, path: &Path, sha256: &str, expected_len: usize) -> Result<Vec<u8>> {
        let bytes = read_private(path)?;
        let actual = hex::encode(Sha256::digest(&bytes));
        if bytes.len() != expected_len || actual != sha256 {
            return Err(Error::InvalidState(format!(
                "content blob {} is corrupt",
                path.display()
            )));
        }
        Ok(bytes)
    }

    fn path_for_hash(&self, sha256: &str) -> Result<PathBuf> {
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(Error::InvalidState(
                "content blob hash must be 64 lowercase hexadecimal characters".into(),
            ));
        }
        Ok(self.root.join(&sha256[..2]).join(&sha256[2..]))
    }

    #[cfg(test)]
    fn stored_path(&self, content: &ContentRef) -> Option<PathBuf> {
        match content {
            ContentRef::Inline { .. } => None,
            ContentRef::Blob { sha256, .. } => self.path_for_hash(sha256).ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::{Arc, Barrier};

    use tempfile::TempDir;

    use super::BlobStore;
    use crate::model::ContentRef;

    #[test]
    fn small_utf8_content_stays_inline() {
        let temp = TempDir::new().unwrap();
        let store = BlobStore::new(temp.path());

        let content = store.put(b"small output").unwrap();

        assert_eq!(
            content,
            ContentRef::Inline {
                text: "small output".into()
            }
        );
        assert_eq!(store.resolve(&content).unwrap(), b"small output");
        assert!(!temp.path().join("blobs").exists());
    }

    #[test]
    fn large_content_is_private_and_deduplicated() {
        let temp = TempDir::new().unwrap();
        let store = BlobStore::new(temp.path());
        let bytes = vec![b'x'; 8 * 1024 + 1];

        let first = store.put(&bytes).unwrap();
        let second = store.put(&bytes).unwrap();

        assert_eq!(first, second);
        let path = store.stored_path(&first).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn concurrent_identical_writes_verify_and_reuse_the_winner() {
        let temp = TempDir::new().unwrap();
        let store = Arc::new(BlobStore::new(temp.path()));
        let barrier = Arc::new(Barrier::new(8));
        let bytes = Arc::new(vec![b'r'; 32 * 1024]);
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                let bytes = Arc::clone(&bytes);
                std::thread::spawn(move || {
                    barrier.wait();
                    store.put(&bytes)
                })
            })
            .collect();

        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap().unwrap())
            .collect();
        assert!(results.iter().all(|result| result == &results[0]));
        assert_eq!(store.resolve(&results[0]).unwrap(), *bytes);
    }

    #[test]
    fn existing_corrupt_or_redirected_blobs_are_rejected() {
        let temp = TempDir::new().unwrap();
        let store = BlobStore::new(temp.path());
        let bytes = vec![b'x'; 8 * 1024 + 1];
        let content = store.put(&bytes).unwrap();
        let path = store.stored_path(&content).unwrap();

        std::fs::write(&path, b"corrupt").unwrap();
        assert!(store.put(&bytes).is_err());

        std::fs::remove_file(&path).unwrap();
        let target = temp.path().join("target");
        std::fs::write(&target, &bytes).unwrap();
        symlink(&target, &path).unwrap();
        assert!(store.put(&bytes).is_err());
    }

    #[test]
    fn small_non_utf8_content_uses_a_blob_and_hashes_are_not_paths() {
        let temp = TempDir::new().unwrap();
        let store = BlobStore::new(temp.path());
        let content = store.put(&[0xff, 0xfe]).unwrap();
        assert!(matches!(content, ContentRef::Blob { bytes: 2, .. }));
        assert_eq!(store.resolve(&content).unwrap(), [0xff, 0xfe]);

        let forged = ContentRef::Blob {
            sha256: "../../escape".into(),
            bytes: 0,
        };
        assert!(store.resolve(&forged).is_err());
    }
}
