use std::{io::{self, Cursor}, time::UNIX_EPOCH};

use clap::error;
use git2::{Cred, RemoteCallbacks, Repository, build::{CheckoutBuilder, RepoBuilder}};
use tokio::fs;

use crate::*;

pub struct DataManager {
    data_dir: std::path::PathBuf,
}

impl DataManager {
    pub async fn new(data_dir: std::path::PathBuf) -> anyhow::Result<Self> {
        if !data_dir.is_dir() {
            fs::create_dir_all(&data_dir).await.context("Failed to create data directory")?
        }
        Ok(Self { data_dir })
    }

    /// Returns the path to the local repository for the given user and repo, refreshing it if necessary
    pub async fn get_repo(&self, user: &str, repo: &str, opts: &Opts) -> Result<std::path::PathBuf, AppError> {
        let assumed_path = self.data_dir.join(user).join(repo);
        if assumed_path.exists()
            && let Some(meta) = self.get_meta(user, repo).await?
                && meta.seconds_since_updated() < 60 * 60 {
                    // Repo is present, and has been updated in the last hour
                    return Ok(assumed_path);
        }
        // Repo is not present, or has been updated more than an hour ago
        self.refresh_repo(user, repo, opts).await?;
        Ok(assumed_path)
    }

    /// Refreshes local repository with remote
    pub async fn refresh_repo(&self, user: &str, repo: &str, opts: &Opts) -> Result<(), AppError> {
        let local_path = self.data_dir.join(user).join(repo);
        let repo_url = format!("{base_url}/{user}/{repo}.git", base_url = opts.git_https_base_url);

        let repository = if local_path.exists() {
            // Open existing repository
            let repository = Repository::open(&local_path).map_err(|e| {
                error!("Failed to open local repository: {e}");
                AppError::InternalError
            })?;
            
            // git pull
            repository.find_remote("origin").and_then(|mut remote| {
                let mut callbacks = RemoteCallbacks::new();
                callbacks.credentials(|_url, _, _| {
                    Cred::userpass_plaintext("git", &opts.git_password)
                });
                let mut fo = git2::FetchOptions::new();
                fo.remote_callbacks(callbacks);
                remote.fetch(&[&opts.git_pages_branch], Some(&mut fo), None)
            }).map_err(|e| {
                error!("Failed to fetch remote repository: {e}");
                AppError::InternalError
            })?;

            repository
        } else {
            // Clone remote repository
            RepoBuilder::new()
                .fetch_options({
                    let mut callbacks = RemoteCallbacks::new();
                    callbacks.credentials(|_url, _, _| {
                        Cred::userpass_plaintext("git", &opts.git_password)
                    });
                    let mut fo = git2::FetchOptions::new();
                    fo.remote_callbacks(callbacks);
                    fo
                })
                .clone(&repo_url, &local_path)
                .map_err(|e| {
                    error!("Failed to clone remote repository: {e}");
                    AppError::InternalError
                })?
        };
        // Checkout desired branch
        {
            let desired_branch = format!("origin/{}", opts.git_pages_branch);
            let obj = repository.revparse_single(&desired_branch)
                .map_err(|e| {
                    error!("Failed to find desired branch for remote repository: {e}");
                    std::fs::remove_dir_all(&local_path).context("Failed to remove local repository").unwrap();
                    AppError::NotFound
                })?;
            
            repository.checkout_tree(&obj, Some(CheckoutBuilder::new().force())).map_err(|e| {
                error!("Failed to checkout desired branch for remote repository: {e}");
                std::fs::remove_dir_all(&local_path).context("Failed to remove local repository").unwrap();
                AppError::InternalError
            })?;
    
            repository.set_head(&format!("refs/heads/{}", &opts.git_pages_branch)).map_err(|e| {
                error!("Failed to set head for remote repository: {e}");
                std::fs::remove_dir_all(&local_path).context("Failed to remove local repository").unwrap();
                AppError::InternalError
            })?;
        }

        let mut meta = self.get_meta(user, repo).await?.unwrap_or_default();
        meta.update();
        self.set_meta(user, repo, meta).await?;
        Ok(())
    }
    
    /// Get [RepoMeta] for a given user and repo, if it exists
    pub async fn get_meta(&self, user: &str, repo: &str) -> Result<Option<RepoMeta>, AppError> {
        let path = self.data_dir.join(user).join(format!("{repo}.meta.json"));
        if !path.exists() {
            return Ok(None)
        }
        let meta = tokio::fs::read_to_string(&path).await.map_err(|e| {
            error!("Failed to read meta file: {e}");
            AppError::InternalError
        })?;
        let meta: RepoMeta = serde_json::from_str(&meta).map_err(|e| {
            error!("Failed to parse meta file: {e}");
            AppError::InternalError
        })?;
        Ok(Some(meta))
    }

    /// Set [RepoMeta] for a given user and repo
    pub async fn set_meta(&self, user: &str, repo: &str, meta: RepoMeta) -> Result<(), AppError> {
        let path = self.data_dir.join(user).join(format!("{repo}.meta.json"));
        let meta = serde_json::to_string(&meta).map_err(|e| {
            error!("Failed to serialize meta: {e}");
            AppError::InternalError
        })?;
        tokio::fs::write(&path, meta).await.map_err(|e| {
            error!("Failed to write meta file: {e}");
            AppError::InternalError
        })?;
        Ok(())
    }
}


#[derive(Debug, Serialize, Deserialize)]
pub struct RepoMeta {
    created_at: u64,
    updated_at: u64,
}
impl Default for RepoMeta {
    fn default() -> Self {
        let current_time = current_time_seconds();
        Self {
            created_at: current_time,
            updated_at: current_time,
        }
    }

}
impl RepoMeta {
    pub fn update(&mut self) {
        self.updated_at = current_time_seconds();
    }
    pub fn seconds_since_updated(&self) -> u64 {
        current_time_seconds() - self.updated_at
    }
}

fn current_time_seconds() -> u64 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).expect("SystemTime should be after Unix Epoch!").as_secs()
}