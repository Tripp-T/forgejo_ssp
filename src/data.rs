use {
    crate::*,
    git2::{
        Cred, RemoteCallbacks, Repository,
        build::{CheckoutBuilder, RepoBuilder},
    },
    std::{collections::HashMap, path::PathBuf, time::UNIX_EPOCH},
    tokio::{
        fs,
        sync::{Mutex, RwLock, oneshot},
    },
};

pub struct DataManager {
    data_dir: std::path::PathBuf,
    fetcher: FetchingManager,
}

impl DataManager {
    pub async fn new(data_dir: std::path::PathBuf) -> anyhow::Result<Self> {
        if !data_dir.is_dir() {
            fs::create_dir_all(&data_dir)
                .await
                .context("Failed to create data directory")?
        }
        Ok(Self {
            data_dir,
            fetcher: FetchingManager::new(),
        })
    }

    /// Returns the path to the local repository for the given user and repo, refreshing it if necessary
    pub async fn get_repo(
        &self,
        req_repo: RequestedRepo,
        opts: &Opts,
    ) -> Result<std::path::PathBuf, AppError> {
        let destination = self
            .data_dir
            .join(req_repo.user.clone())
            .join(req_repo.repo.clone());
        if destination.exists()
            && let Some(meta) = self.get_meta(&req_repo).await?
            && meta.seconds_since_updated() < 60 * 60
        {
            // Repo is present, and has been updated in the last hour
            return Ok(destination);
        }
        // Repo is not present, or has been updated more than an hour ago
        self.refresh_repo(req_repo, opts).await?;
        Ok(destination)
    }

    /// Refreshes local repository with remote
    pub async fn refresh_repo(&self, req_repo: RequestedRepo, opts: &Opts) -> Result<(), AppError> {
        let destination = self
            .data_dir
            .join(req_repo.user.clone())
            .join(req_repo.repo.clone());
        let repo_url = format!(
            "{base_url}/{req_repo}.git",
            base_url = opts.git_https_base_url
        );

        self.fetcher
            .fetch(req_repo.clone(), repo_url, &destination, opts)
            .await?;

        let mut meta = self.get_meta(&req_repo).await?.unwrap_or_default();
        meta.update();
        self.set_meta(&req_repo, meta).await?;
        Ok(())
    }

    /// Get [RepoMeta] for a given user and repo, if it exists
    pub async fn get_meta(&self, req_repo: &RequestedRepo) -> Result<Option<RepoMeta>, AppError> {
        let path = self
            .data_dir
            .join(req_repo.user.clone())
            .join(format!("{}.meta.json", req_repo.repo));
        if !path.exists() {
            return Ok(None);
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
    pub async fn set_meta(&self, req_repo: &RequestedRepo, meta: RepoMeta) -> Result<(), AppError> {
        let path = self
            .data_dir
            .join(req_repo.user.clone())
            .join(format!("{}.meta.json", req_repo.repo));
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

#[derive(Default, PartialEq, Eq, PartialOrd, Ord, Clone)]
enum FetchingStatus {
    #[default]
    Idle,
    Fetching,
    Failed(AppError),
}

struct FetchingManager {
    status: RwLock<HashMap<RequestedRepo, FetchingStatus>>,
    callbacks: Mutex<HashMap<RequestedRepo, Vec<oneshot::Sender<()>>>>,
}
impl FetchingManager {
    pub fn new() -> Self {
        Self {
            status: RwLock::new(HashMap::new()),
            callbacks: Mutex::new(HashMap::new()),
        }
    }
    async fn add_callback(&self, repo: RequestedRepo) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel::<()>();
        self.callbacks
            .lock()
            .await
            .entry(repo)
            .or_default()
            .push(tx);
        rx
    }
    async fn get_callbacks(&self, repo: RequestedRepo) -> Vec<oneshot::Sender<()>> {
        self.callbacks
            .lock()
            .await
            .remove_entry(&repo)
            .map(|e| e.1)
            .unwrap_or_default()
    }
    async fn is_fetching(&self, repo: &RequestedRepo) -> bool {
        self.status
            .read()
            .await
            .get(repo)
            .cloned()
            .unwrap_or_default()
            == FetchingStatus::Fetching
    }
    async fn is_failed(&self, repo: &RequestedRepo) -> Option<AppError> {
        if let FetchingStatus::Failed(e) = self
            .status
            .read()
            .await
            .get(repo)
            .cloned()
            .unwrap_or_default()
        {
            Some(e)
        } else {
            None
        }
    }
    async fn set_status(&self, repo: RequestedRepo, status: FetchingStatus) {
        self.status.write().await.insert(repo, status);
    }
    pub async fn fetch(
        &self,
        repo: RequestedRepo,
        url: String,
        destination: &PathBuf,
        opts: &Opts,
    ) -> Result<(), AppError> {
        if self.is_fetching(&repo).await {
            // Creates a callback and awaits its call
            if let Err(e) = self.add_callback(repo.clone()).await.await {
                error!("failed to add callback: {e}");
                return Err(AppError::InternalError);
            };
            if let Some(e) = self.is_failed(&repo).await {
                return Err(e);
            } else {
                return Ok(());
            }
        }
        self.set_status(repo.clone(), FetchingStatus::Fetching)
            .await;
        let new_status = match self._fetch(url, destination, opts).await {
            Ok(_) => FetchingStatus::Idle,
            Err(e) => FetchingStatus::Failed(e),
        };
        self.set_status(repo.clone(), new_status.clone()).await;
        for cb in self.get_callbacks(repo).await {
            if cb.send(()).is_err() {
                error!("failed to send callback signal")
            };
        }
        if let FetchingStatus::Failed(e) = new_status {
            Err(e)
        } else {
            Ok(())
        }
    }
    /// private method to perform the fetch, should only ever be called by the public fetch method
    async fn _fetch(
        &self,
        url: String,
        destination: &PathBuf,
        opts: &Opts,
    ) -> Result<(), AppError> {
        let repository = if destination.exists() {
            // local repo exists already, so try to git pull it
            let repository = Repository::open(destination).map_err(|e| {
                error!("Failed to open local repository: {e}");
                AppError::InternalError
            })?;
            repository
                .find_remote("origin")
                .and_then(|mut remote| {
                    let mut callbacks = RemoteCallbacks::new();
                    callbacks.credentials(|_url, _, _| {
                        Cred::userpass_plaintext("git", &opts.git_password)
                    });
                    let mut fo = git2::FetchOptions::new();
                    fo.remote_callbacks(callbacks);
                    remote.fetch(&[&opts.git_pages_branch], Some(&mut fo), None)
                })
                .map_err(|e| {
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
                .clone(&url, destination)
                .map_err(|e| {
                    error!("Failed to clone remote repository: {e}");
                    AppError::InternalError
                })?
        };

        // Checkout configured branch
        let desired_branch = format!("origin/{}", opts.git_pages_branch);
        let obj = repository.revparse_single(&desired_branch).map_err(|e| {
            error!("Failed to find desired branch for remote repository: {e}");
            std::fs::remove_dir_all(destination)
                .map_err(|e| {
                    error!("Failed to remove local repository: {e}");
                    AppError::InternalError
                })
                .err()
                .unwrap_or(AppError::NotFound)
        })?;

        repository
            .checkout_tree(&obj, Some(CheckoutBuilder::new().force()))
            .map_err(|e| {
                error!("Failed to checkout desired branch for remote repository: {e}");
                std::fs::remove_dir_all(destination)
                    .context("Failed to remove local repository")
                    .map_err(|e| {
                        error!("Failed to remove local repository: {e}");
                        AppError::InternalError
                    })
                    .err()
                    .unwrap_or(AppError::InternalError)
            })?;

        repository
            .set_head(&format!("refs/heads/{}", &opts.git_pages_branch))
            .map_err(|e| {
                error!("Failed to set head for remote repository: {e}");
                std::fs::remove_dir_all(destination)
                    .context("Failed to remove local repository")
                    .map_err(|e| {
                        error!("Failed to remove local repository: {e}");
                        AppError::InternalError
                    })
                    .err()
                    .unwrap_or(AppError::InternalError)
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
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("SystemTime should be after Unix Epoch!")
        .as_secs()
}
