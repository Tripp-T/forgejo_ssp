use std::io::{self, Cursor};

use clap::error;
use tokio::fs;
use zip::ZipArchive;

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
    pub async fn get_repo(&self, user: &str, repo: &str, opts: &Opts) -> Result<std::path::PathBuf, AppError> {
        let assumed_path = self.data_dir.join(user).join(repo);
        if assumed_path.is_dir() {
            return Ok(assumed_path)
        }
        let remote_url = format!("{base_url}/{user}/{repo}/archive/{branch}.zip", base_url = opts.git_https_base_url, branch = opts.git_pages_branch);
        let response = reqwest::get(remote_url).await.context("failed to get make git https request").map_err(|e| {
            error!("{e}");
            AppError::InternalError
        })?;
        if !response.status().is_success() {
            error!("Response status is not success: {response:?}");
            return Err(AppError::InternalError)
        }
        let bytes = response.bytes().await.context("Failed to parse bytes").map_err(|e| {
            error!("{e}");
            AppError::InternalError
        })?;

        let reader = Cursor::new(bytes);
        let mut archive = ZipArchive::new(reader).context("Failed to open zip archive").map_err(|e| {
            error!("Failed to open archive: {e}");
            AppError::InternalError
        })?;

        for i in 0..archive.len() {
            let assumed_path = self.data_dir.join(user); // All files in the archive are under a parent folder named after the repo, so we re-initialize the assumed path
            let mut file = archive.by_index(i).map_err(|e| {
                error!("Failed to index zip archive: {e}");
                AppError::InternalError
            })?;
            
            // Use enclosed_name() to prevent "Zip Slip" directory traversal attacks
            let outpath = match file.enclosed_name() {
                Some(path) => assumed_path.join(path),
                None => continue, 
            };

            // If the item is a directory, create it
            if file.is_dir() {
                fs::create_dir_all(&outpath).await.map_err(|e| {
                    error!("Failed to create directory: {e}");
                    AppError::InternalError
                })?;
            } else {
                // If the item is a file, ensure its parent directory exists
                if let Some(p) = outpath.parent() {
                    if !p.exists() {
                        fs::create_dir_all(p).await.map_err(|e| {
                            error!("Failed to create directory: {e}");
                            AppError::InternalError
                        })?;
                    }
                }
                // Create the file and copy the uncompressed contents into it
                let mut outfile = std::fs::File::create(&outpath).map_err(|e| {
                    error!("Failed to create file: {e}");
                    AppError::InternalError
                })?;
                io::copy(&mut file, &mut outfile).map_err(|e| {
                    error!("Failed to copy file: {e}");
                    AppError::InternalError
                })?;
            }
        }
        Ok(assumed_path)
    }
}

