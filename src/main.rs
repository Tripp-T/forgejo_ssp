use ::{
    anyhow::Context,
    axum::{
        Router,
        body::Body,
        debug_handler,
        extract::{FromRequestParts, Request, State},
        http::{HeaderName, Response},
        response::IntoResponse,
    },
    clap::Parser,
    reqwest::StatusCode,
    serde::{Deserialize, Serialize},
    std::{fmt::Display, ops::Deref, sync::Arc},
    thiserror::Error as ThisError,
    tokio::signal,
    tower_http::services::{ServeDir, fs::ServeFileSystemResponseBody},
    tracing::{debug, error, info, warn},
    tracing_subscriber::EnvFilter,
};

mod data;

#[derive(Parser, Debug)]
pub struct Opts {
    #[clap(short, long, env = "HTTP_PORT")]
    /// Port to listen on
    pub port: u16,
    #[clap(short, long, env = "HTTP_ADDR")]
    /// Address to listen on
    pub addr: String,
    #[clap(long, env = "HTTP_HOST_SUFFIX")]
    /// Suffix of the HTTP host
    /// Ex. "ssp.mydomain.tld"
    /// Requests will be parsed down to:
    /// {repo}.{user}.{host_suffix}
    /// or
    /// {repo}.{host_suffix}
    pub host_suffix: String,

    #[clap(short, long, env = "DATA")]
    /// Path to the data directory
    pub data: std::path::PathBuf,
    #[clap(long, env = "GIT_HTTPS_BASE_URL")]
    /// Base URL of the git server
    /// Ex. "https://forgejo.mydomain.tld/"
    pub git_https_base_url: String,
    #[clap(long, env = "GIT_PAGES_BRANCH", default_value = "pages")]
    /// Branch to pull the static files from
    pub git_pages_branch: String,
    #[clap(long, env = "GIT_DEFAULT_REPO_USER")]
    /// Default user to use when looking for a repo, if no user was provided in the request
    pub git_default_repo_user: String,
    #[clap(env = "GIT_PASSWORD")]
    pub git_password: String,
}

#[derive(Clone)]
struct AppState(Arc<AppStateInner>);
struct AppStateInner {
    data: data::DataManager,
    opts: Opts,
}
impl AppState {
    async fn new(opts: Opts) -> anyhow::Result<Self> {
        let data = data::DataManager::new(opts.data.clone()).await?;
        Ok(Self(Arc::new(AppStateInner { data, opts })))
    }
}
impl Deref for AppState {
    type Target = AppStateInner;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let opts = Opts::parse();
    let state = AppState::new(opts).await?;

    axum::serve(
        tokio::net::TcpListener::bind(format!("{}:{}", &state.opts.addr, &state.opts.port))
            .await
            .inspect(|_| {
                info!(
                    "Listening for HTTP connections on: http://{}:{}",
                    &state.opts.addr, &state.opts.port
                )
            })?,
        Router::new()
            .fallback(handle_request)
            .with_state(state)
            .into_make_service(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("Failed to run server")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    // Listen for SIGTERM (This is what `docker stop` sends)
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    // If we are not on Unix, just create a future that never resolves
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("Received Ctrl+C"),
        _ = terminate => info!("Received SIGTERM"),
    };
    info!("Shutting down...");
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, ThisError)]
enum AppError {
    #[error("Not Found")]
    NotFound,
    #[error("Invalid Request")]
    InvalidRequest,
    #[error("Internal Error")]
    InternalError,
}
impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        match self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found").into_response(),
            AppError::InvalidRequest => {
                (StatusCode::BAD_REQUEST, "Invalid request").into_response()
            }
            AppError::InternalError => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response()
            }
        }
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
struct RequestedRepo {
    user: String,
    repo: String,
}
impl FromRequestParts<AppState> for RequestedRepo {
    type Rejection = AppError;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let host = parts
            .headers
            .get(HeaderName::from_static("host"))
            .ok_or(AppError::NotFound)?
            .to_str()
            .map_err(|_| AppError::NotFound)?;
        let host_suffix = &state.opts.host_suffix;
        if !host.ends_with(host_suffix) {
            warn!("Invalid host: {}", host);
            return Err(AppError::NotFound);
        }
        let host = &host[..host.len() - host_suffix.len()];
        let mut parts = host.split('.');
        let repo = parts.next().ok_or(AppError::InvalidRequest)?;
        let user = parts
            .next()
            .and_then(|u| if u.is_empty() { None } else { Some(u) })
            .or_else(|| Some(state.opts.git_default_repo_user.as_str()))
            .expect("There should always be a user");
        Ok(Self {
            user: user.to_string(),
            repo: repo.to_string(),
        })
    }
}
impl Display for RequestedRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{user}/{repo}", user = self.user, repo = self.repo)
    }
}

#[debug_handler]
async fn handle_request(
    state: State<AppState>,
    req_repo: RequestedRepo,
    req: Request<Body>,
) -> Result<Response<ServeFileSystemResponseBody>, AppError> {
    let repo_path = state.data.get_repo(req_repo, &state.opts).await?;
    if req
        .uri()
        .path()
        .split('/')
        .any(|segment| segment.eq(".git"))
    {
        warn!("disallowing access to .git directory");
        return Err(AppError::NotFound);
    }
    debug!("Serving static repo: {:?}", repo_path);
    ServeDir::new(repo_path).try_call(req).await.map_err(|e| {
        error!("Failed to serve static repo: {e}");
        AppError::NotFound
    })
}
