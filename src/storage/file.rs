use crate::domain::User;
use crate::storage::schema;
use anyhow::{Context, Result};
use std::path::Path;
use tokio::io::AsyncWriteExt;
use tracing::debug;

/// Read and deserialise a user from a JSON file, running any needed migrations.
pub async fn read_user(path: &Path) -> Result<User> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("Cannot read {}", path.display()))?;

    let user = schema::deserialise_and_migrate(&bytes)
        .with_context(|| format!("Cannot parse {}", path.display()))?;

    debug!(path = %path.display(), user_id = %user.key.channel_user_id, "Loaded user");
    Ok(user)
}

/// Atomically write a user's state to `<data_dir>/<stem>.json` using the
/// write-to-tmp → fsync → rename pattern.
pub async fn write_user(data_dir: &Path, user: &User) -> Result<()> {
    let json = serde_json::to_vec_pretty(user).context("Serialise user")?;

    let stem = user.key.file_stem();
    let final_path = data_dir.join(format!("{}.json", stem));

    // Create a named temp file in the same directory so rename is atomic.
    let tmp_path = data_dir.join(format!("{}.json.tmp.{}", stem, ulid::Ulid::new()));

    {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .await
            .with_context(|| format!("Cannot create tmp file {}", tmp_path.display()))?;

        file.write_all(&json)
            .await
            .with_context(|| format!("Cannot write tmp file {}", tmp_path.display()))?;

        file.flush().await.context("Flush tmp file")?;
        file.sync_all().await.context("fsync tmp file")?;
    }

    tokio::fs::rename(&tmp_path, &final_path)
        .await
        .with_context(|| {
            format!(
                "Cannot rename {} → {}",
                tmp_path.display(),
                final_path.display()
            )
        })?;

    debug!(path = %final_path.display(), "Persisted user");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{User, UserKey};

    #[tokio::test]
    async fn roundtrip_write_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = UserKey::telegram(42);
        let user = User::new(key.clone());

        write_user(dir.path(), &user).await.expect("write");

        let path = dir.path().join("telegram-42.json");
        assert!(path.exists(), "json file should exist after write");

        let loaded = read_user(&path).await.expect("read");
        assert_eq!(loaded.key, key);
        assert!(!loaded.notifications_paused);
    }

    #[tokio::test]
    async fn tmp_file_cleaned_up_on_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = User::new(UserKey::telegram(99));

        write_user(dir.path(), &user).await.expect("write");

        let tmp_files: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();

        assert!(
            tmp_files.is_empty(),
            "tmp files should be gone after rename"
        );
    }
}
