mod file;
pub mod schema;

use crate::domain::{User, UserKey};
use anyhow::{Context, Result};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::RwLock;
use tracing::{error, info};

/// Thread-safe in-memory user store backed by per-user JSON files.
pub struct UserStore {
    users: RwLock<HashMap<UserKey, Arc<RwLock<User>>>>,
    data_dir: PathBuf,
}

impl UserStore {
    /// Load all existing user files from `data_dir` into memory.
    pub async fn load(data_dir: &Path) -> Result<Self> {
        tokio::fs::create_dir_all(data_dir)
            .await
            .with_context(|| format!("Cannot create user data dir: {}", data_dir.display()))?;

        let mut users = HashMap::new();
        let mut read_dir = tokio::fs::read_dir(data_dir)
            .await
            .with_context(|| format!("Cannot read user data dir: {}", data_dir.display()))?;

        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match file::read_user(&path).await {
                Ok(user) => {
                    let key = user.key.clone();
                    users.insert(key, Arc::new(RwLock::new(user)));
                }
                Err(e) => {
                    error!(path = %path.display(), err = %e, "Skipping unreadable user file");
                }
            }
        }

        info!(count = users.len(), "Loaded users from disk");
        Ok(Self {
            users: RwLock::new(users),
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// Get a user by key, or create a new one if they don't exist yet.
    pub async fn get_or_create(&self, key: UserKey) -> Arc<RwLock<User>> {
        self.get_or_create_new(key).await.0
    }

    /// Like [`get_or_create`] but also returns `true` when the user was newly created.
    pub async fn get_or_create_new(&self, key: UserKey) -> (Arc<RwLock<User>>, bool) {
        {
            let map = self.users.read().await;
            if let Some(arc) = map.get(&key) {
                return (Arc::clone(arc), false);
            }
        }
        let mut map = self.users.write().await;
        if let Some(arc) = map.get(&key) {
            return (Arc::clone(arc), false);
        }
        let user = User::new(key.clone());
        let arc = Arc::new(RwLock::new(user));
        map.insert(key, Arc::clone(&arc));
        (arc, true)
    }

    /// Persist a user's current state to disk atomically.
    pub async fn persist(&self, user: &User) -> Result<()> {
        file::write_user(&self.data_dir, user).await
    }

    /// Return a snapshot of all currently loaded users (for the polling loop).
    pub async fn all_users(&self) -> Vec<Arc<RwLock<User>>> {
        self.users.read().await.values().map(Arc::clone).collect()
    }
}
