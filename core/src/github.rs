use std::{
    collections::HashSet,
    future::{ready, Future},
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{redirect::Policy, Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};

use crate::{settings, Readme, RepoMetadata, Store, SyncLease, SyncPhase, SyncResult};

const API_URL: &str = "https://api.github.com/";
const PAGE_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(500), Duration::from_secs(2)];
pub(crate) const TOKEN_ENV: &str = "STARCHIVE_GITHUB_TOKEN";

#[derive(Debug)]
pub enum GitHubError {
    Configuration(&'static str),
    Transport(reqwest::Error),
    Unauthorized,
    Forbidden,
    RateLimited {
        retry_after: Option<u64>,
        reset_at: Option<u64>,
    },
    Http(StatusCode),
    InvalidResponse(reqwest::Error),
    InterruptedStarPage(usize),
    InvalidStarPage(usize),
    InvalidPagination,
    InvalidUrl(&'static str),
    InvalidReadme(&'static str),
}

impl std::fmt::Display for GitHubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(message) => f.write_str(message),
            Self::Transport(error) => write!(f, "GitHub request failed: {error}"),
            Self::Unauthorized => f.write_str("GitHub rejected authentication (HTTP 401). Check the credential source in Settings or update your GitHub CLI login or environment token."),
            Self::Forbidden => f.write_str("GitHub denied access (HTTP 403). Check the credential's read permissions and repository access."),
            Self::RateLimited { retry_after: Some(seconds), .. } => {
                write!(f, "GitHub rate limit reached. Retry after {seconds} seconds.")
            }
            Self::RateLimited { reset_at: Some(timestamp), .. } => {
                write!(f, "GitHub rate limit reached. Retry after Unix timestamp {timestamp}.")
            }
            Self::RateLimited { .. } => f.write_str("GitHub rate limit reached. Wait before retrying."),
            Self::Http(status) => write!(f, "GitHub returned HTTP {status}. Retry if the failure is temporary."),
            Self::InvalidResponse(error) => write!(f, "GitHub returned an unreadable response: {error}"),
            Self::InterruptedStarPage(page) => write!(f, "获取第 {page} 页星标时连接中断，自动重试两次后仍未成功。已保留原有资料，可稍后重试同步。"),
            Self::InvalidStarPage(page) => write!(f, "GitHub 返回的第 {page} 页星标数据格式异常。已保留原有资料。"),
            Self::InvalidPagination => f.write_str("GitHub returned an invalid pagination link. Retry the sync."),
            Self::InvalidUrl(resource) => write!(f, "GitHub returned an invalid {resource} URL"),
            Self::InvalidReadme(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for GitHubError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) | Self::InvalidResponse(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct Star {
    starred_at: String,
    repo: GitHubRepo,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubRepo {
    pub id: i64,
    pub full_name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    pub html_url: String,
    pub private: bool,
    pub default_branch: Option<String>,
    #[serde(default)]
    pub stargazers_count: i64,
    pub updated_at: Option<String>,
    pub starred_at: Option<String>,
}

impl From<GitHubRepo> for RepoMetadata {
    fn from(repo: GitHubRepo) -> Self {
        Self {
            id: repo.id,
            full_name: repo.full_name,
            description: repo.description,
            language: repo.language,
            html_url: repo.html_url,
            private: repo.private,
        }
    }
}

#[derive(Clone)]
pub struct GitHub {
    http: Client,
    base: Url,
    token: Option<reqwest::header::HeaderValue>,
}

impl GitHub {
    pub fn new(token: Option<&str>) -> Result<Self, GitHubError> {
        Self::with_base_url(API_URL, token)
    }

    pub(crate) async fn from_settings() -> anyhow::Result<Self> {
        tokio::task::spawn_blocking(|| {
            let token = settings::github_token()?;
            Ok(Self::new(token.as_deref())?)
        })
        .await?
    }

    fn with_base_url(base: &str, token: Option<&str>) -> Result<Self, GitHubError> {
        let base = Url::parse(base)
            .map_err(|_| GitHubError::Configuration("Invalid GitHub API base URL"))?;
        let http = Client::builder()
            .user_agent(concat!("Starfolio/", env!("CARGO_PKG_VERSION")))
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| GitHubError::Transport(error.without_url()))?;

        let token = token.map(str::trim).filter(|token| !token.is_empty());
        let token = token
            .map(|token| {
                let invalid_token =
                    GitHubError::Configuration("The GitHub credential contains invalid characters");
                if !token.bytes().all(|byte| byte.is_ascii_graphic()) {
                    return Err(invalid_token);
                }
                let mut header = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| invalid_token)?;
                header.set_sensitive(true);
                Ok(header)
            })
            .transpose()?;

        Ok(Self { http, base, token })
    }

    /// Record the attempt before discovering credentials so setup failures are visible on reopen.
    pub async fn sync_from_settings(store: &Store) -> anyhow::Result<SyncResult> {
        let lease = Self::sync_lease(store).await?;
        match Self::from_settings().await {
            Ok(client) => client.sync_with_lease(lease).await,
            Err(error) => Err(lease.fail(error).await),
        }
    }

    /// Update the local library only after every page of GitHub stars is available.
    pub async fn sync(&self, store: &Store) -> anyhow::Result<SyncResult> {
        self.sync_with_lease(Self::sync_lease(store).await?).await
    }

    async fn sync_lease(store: &Store) -> anyhow::Result<SyncLease> {
        store.try_sync_lease().await?.ok_or_else(|| {
            anyhow::anyhow!("GitHub sync is already in progress. Try again once it finishes.")
        })
    }

    async fn sync_with_lease(&self, lease: SyncLease) -> anyhow::Result<SyncResult> {
        let fetched = self
            .starred_with_progress(|phase, pages, repositories| {
                lease.set_progress(phase, pages, repositories)
            })
            .await;
        match fetched {
            Ok((stars, pages)) => {
                let repositories: Vec<_> = stars.into_iter().map(RepoMetadata::from).collect();
                lease.complete_fetched_snapshot(&repositories, pages).await
            }
            Err(error) => Err(lease.fail(error).await),
        }
    }

    /// Fetch the entire authenticated star list before returning a snapshot.
    pub async fn starred(&self) -> Result<Vec<GitHubRepo>, GitHubError> {
        let (repositories, _) = self.starred_with_progress(|_, _, _| ready(Ok(()))).await?;
        Ok(repositories)
    }

    async fn starred_with_progress<
        E: From<GitHubError>,
        F: Future<Output = Result<(), E>> + Send,
    >(
        &self,
        mut report: impl FnMut(SyncPhase, usize, usize) -> F,
    ) -> Result<(Vec<GitHubRepo>, usize), E> {
        report(SyncPhase::Fetching, 0, 0).await?;
        let token = self.token.as_ref().ok_or(GitHubError::Configuration(
            "Configure a GitHub credential with read access to your stars in Settings, with gh auth login, or with GH_TOKEN",
        ))?;
        let endpoint = self.base.join("user/starred").expect("valid API endpoint");
        let mut url = endpoint.clone();
        url.query_pairs_mut().append_pair("per_page", "100");

        let mut visited = HashSet::new();
        let mut repositories = Vec::new();
        let mut pages = 0;
        loop {
            if !visited.insert(url.clone()) {
                return Err(GitHubError::InvalidPagination.into());
            }
            let mut delays = PAGE_RETRY_DELAYS.into_iter();
            let (stars, next) = loop {
                match self.starred_page(&url, &endpoint, token, pages + 1).await {
                    Ok(page) => break page,
                    Err(error @ GitHubError::InterruptedStarPage(_)) => {
                        let Some(delay) = delays.next() else {
                            return Err(error.into());
                        };
                        report(SyncPhase::Retrying, pages, repositories.len()).await?;
                        tokio::time::sleep(delay).await;
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            for star in stars {
                Self::validate_repo_url(&star.repo)?;
                repositories.push(GitHubRepo {
                    starred_at: Some(star.starred_at),
                    ..star.repo
                });
            }
            pages += 1;
            report(SyncPhase::Fetching, pages, repositories.len()).await?;
            match next {
                Some(next) => url = next,
                None => return Ok((repositories, pages)),
            }
        }
    }

    async fn starred_page(
        &self,
        url: &Url,
        endpoint: &Url,
        token: &reqwest::header::HeaderValue,
        page: usize,
    ) -> Result<(Vec<Star>, Option<Url>), GitHubError> {
        let response = self
            .get(url.clone(), "application/vnd.github.star+json", Some(token))
            .await
            .map_err(|error| match error {
                GitHubError::Transport(ref cause) if cause.is_timeout() => {
                    GitHubError::InterruptedStarPage(page)
                }
                other => other,
            })?;
        let response = Self::check_status(response).await?;
        let next = Self::next_page(&response, url, endpoint)?;
        // Finish reading the HTTP body before decoding it or advancing the pagination cursor.
        let body = response
            .bytes()
            .await
            .map_err(|_| GitHubError::InterruptedStarPage(page))?;
        let stars = serde_json::from_slice(&body).map_err(|error| {
            if error.is_eof() {
                GitHubError::InterruptedStarPage(page)
            } else {
                GitHubError::InvalidStarPage(page)
            }
        })?;
        Ok((stars, next))
    }

    /// Call only for an explicit search expansion with public search terms.
    pub async fn search_public(&self, query: &str) -> Result<Vec<GitHubRepo>, GitHubError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(GitHubError::Configuration(
                "Enter public repository search terms",
            ));
        }
        #[derive(Deserialize)]
        struct SearchResults {
            items: Vec<GitHubRepo>,
        }

        let mut url = self
            .base
            .join("search/repositories")
            .expect("valid API endpoint");
        url.query_pairs_mut()
            .append_pair("q", &format!("{query} is:public"));
        let response = self.get(url, "application/vnd.github+json", None).await?;
        let response = Self::check_status(response).await?;
        let mut results = Self::json::<SearchResults>(response).await?.items;
        results.retain(|repo| !repo.private);
        for repo in &results {
            Self::validate_repo_url(repo)?;
        }
        Ok(results)
    }

    /// Fetch the default README for a selected public repository by its stable GitHub ID.
    pub async fn readme(&self, repository_id: i64) -> Result<Option<String>, GitHubError> {
        Ok(self
            .fetch_readme(repository_id, None)
            .await?
            .map(|readme| readme.content))
    }

    /// Fetch and cache the selected repository's README while its saved metadata still matches.
    pub async fn refresh_readme(
        &self,
        store: &Store,
        repo: &RepoMetadata,
    ) -> anyhow::Result<Option<Readme>> {
        let token = if repo.private {
            Some(self.token.as_ref().ok_or(GitHubError::Configuration(
                "Configure a GitHub credential with read access to this private repository's contents in Settings, with gh auth login, or with GH_TOKEN",
            ))?)
        } else {
            None
        };
        let Some(readme) = self.fetch_readme(repo.id, token).await? else {
            return Ok(None);
        };
        anyhow::ensure!(
            store.cache_readme(repo, &readme).await?,
            "The saved repository changed while loading its README. Reopen the repository and retry."
        );
        Ok(Some(readme))
    }

    async fn fetch_readme(
        &self,
        repository_id: i64,
        token: Option<&reqwest::header::HeaderValue>,
    ) -> Result<Option<Readme>, GitHubError> {
        #[derive(Deserialize)]
        struct EncodedReadme {
            encoding: String,
            content: String,
            html_url: String,
        }

        let url = self
            .base
            .join(&format!("repositories/{repository_id}/readme"))
            .expect("valid API endpoint");
        let response = self.get(url, "application/vnd.github+json", token).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let readme: EncodedReadme = Self::json(Self::check_status(response).await?).await?;
        Self::web_url(&readme.html_url, "README")?;
        if readme.encoding != "base64" {
            return Err(GitHubError::InvalidReadme(
                "GitHub returned an unsupported README encoding. Open the README on GitHub.",
            ));
        }
        let encoded: Vec<_> = readme
            .content
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        let content = STANDARD
            .decode(encoded)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or(GitHubError::InvalidReadme(
                "GitHub returned a README that could not be decoded as UTF-8 text.",
            ))?;
        Ok(Some(Readme {
            content,
            source_url: readme.html_url,
        }))
    }

    fn valid_name_part(part: &str) -> bool {
        !matches!(part, "" | "." | "..")
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    }

    /// Check that a URL identifies a repository on github.com before opening it.
    pub fn repository_url(value: &str) -> Result<Url, GitHubError> {
        let url = Self::web_url(value, "repository")?;
        let valid_path = url
            .path()
            .strip_prefix('/')
            .and_then(|path| path.split_once('/'))
            .is_some_and(|(owner, repo)| {
                Self::valid_name_part(owner) && Self::valid_name_part(repo)
            });
        if !valid_path {
            return Err(GitHubError::InvalidUrl("repository"));
        }
        Ok(url)
    }

    fn validate_repo_url(repo: &GitHubRepo) -> Result<(), GitHubError> {
        let url = Self::repository_url(&repo.html_url)?;
        if !url
            .path()
            .trim_start_matches('/')
            .eq_ignore_ascii_case(&repo.full_name)
        {
            return Err(GitHubError::InvalidUrl("repository"));
        }
        Ok(())
    }

    fn web_url(value: &str, resource: &'static str) -> Result<Url, GitHubError> {
        let url = Url::parse(value).map_err(|_| GitHubError::InvalidUrl(resource))?;
        if url.scheme() != "https"
            || url.host_str() != Some("github.com")
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(GitHubError::InvalidUrl(resource));
        }
        Ok(url)
    }

    async fn get(
        &self,
        url: Url,
        accept: &str,
        token: Option<&reqwest::header::HeaderValue>,
    ) -> Result<Response, GitHubError> {
        let mut request = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, accept)
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = token {
            request = request.header(reqwest::header::AUTHORIZATION, token);
        }
        request
            .send()
            .await
            .map_err(|error| GitHubError::Transport(error.without_url()))
    }

    async fn check_status(response: Response) -> Result<Response, GitHubError> {
        let status = response.status();
        match status {
            StatusCode::OK => Ok(response),
            StatusCode::UNAUTHORIZED => Err(GitHubError::Unauthorized),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::FORBIDDEN => {
                let header = |name| {
                    response
                        .headers()
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                };
                let retry_after_header = header("retry-after");
                let remaining = header("x-ratelimit-remaining");
                let retry_after = retry_after_header.and_then(|value| value.parse().ok());
                let reset_at = (remaining == Some("0"))
                    .then(|| header("x-ratelimit-reset")?.parse().ok())
                    .flatten();

                if status == StatusCode::TOO_MANY_REQUESTS
                    || retry_after_header.is_some()
                    || remaining == Some("0")
                {
                    return Err(GitHubError::RateLimited {
                        retry_after,
                        reset_at,
                    });
                }

                #[derive(Deserialize)]
                struct ErrorResponse {
                    message: String,
                }
                if response
                    .json::<ErrorResponse>()
                    .await
                    .is_ok_and(|error| error.message.to_ascii_lowercase().contains("rate limit"))
                {
                    Err(GitHubError::RateLimited {
                        retry_after,
                        reset_at,
                    })
                } else {
                    Err(GitHubError::Forbidden)
                }
            }
            _ => Err(GitHubError::Http(status)),
        }
    }

    fn next_page(
        response: &Response,
        current: &Url,
        endpoint: &Url,
    ) -> Result<Option<Url>, GitHubError> {
        let mut next = None;
        for header in response.headers().get_all(reqwest::header::LINK) {
            let header = header
                .to_str()
                .map_err(|_| GitHubError::InvalidPagination)?;
            for link in header.split(',') {
                let (target, attributes) = link
                    .trim()
                    .strip_prefix('<')
                    .and_then(|link| link.split_once('>'))
                    .ok_or(GitHubError::InvalidPagination)?;
                let is_next = attributes
                    .split(';')
                    .filter_map(|attribute| attribute.split_once('='))
                    .any(|(name, value)| {
                        name.trim().eq_ignore_ascii_case("rel")
                            && value
                                .trim()
                                .trim_matches('"')
                                .split_whitespace()
                                .any(|relation| relation.eq_ignore_ascii_case("next"))
                    });
                if !is_next {
                    continue;
                }
                let url = current
                    .join(target)
                    .map_err(|_| GitHubError::InvalidPagination)?;
                if url.origin() != endpoint.origin()
                    || url.path() != endpoint.path()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                    || next.replace(url).is_some()
                {
                    return Err(GitHubError::InvalidPagination);
                }
            }
        }
        Ok(next)
    }

    async fn json<T: serde::de::DeserializeOwned>(response: Response) -> Result<T, GitHubError> {
        response
            .json()
            .await
            .map_err(|error| GitHubError::InvalidResponse(error.without_url()))
    }
}

#[cfg(test)]
#[path = "github/page_retry_tests.rs"]
mod page_retry_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyncProgress;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};

    struct Server(TcpListener);

    struct PendingRequest {
        stream: BufReader<TcpStream>,
        request: String,
    }

    impl Server {
        async fn start() -> Self {
            Self(TcpListener::bind("127.0.0.1:0").await.unwrap())
        }

        fn url(&self) -> String {
            format!("http://{}/", self.0.local_addr().unwrap())
        }

        async fn accept(&self) -> PendingRequest {
            let (stream, _) = tokio::time::timeout(Duration::from_secs(5), self.0.accept())
                .await
                .expect("expected a GitHub API request")
                .unwrap();
            let mut stream = BufReader::new(stream);
            let mut request = String::new();
            loop {
                let mut line = String::new();
                assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                request.push_str(&line);
            }
            PendingRequest { stream, request }
        }

        async fn reply(&self, status: u16, headers: &str, body: &str) -> String {
            self.accept().await.respond(status, headers, body).await
        }
    }

    impl PendingRequest {
        async fn respond(mut self, status: u16, headers: &str, body: &str) -> String {
            let response = format!(
                "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                body.len()
            );
            self.stream.write_all(response.as_bytes()).await.unwrap();
            self.request
        }
    }

    fn repo(id: i64, full_name: &str, private: bool) -> serde_json::Value {
        json!({
            "id": id,
            "full_name": full_name,
            "html_url": format!("https://github.com/{full_name}"),
            "private": private,
        })
    }

    fn star(id: i64, full_name: &str) -> serde_json::Value {
        json!({"starred_at": "2025-05-09T10:11:12Z", "repo": repo(id, full_name, false)})
    }

    #[tokio::test]
    async fn explicit_search_sends_one_anonymous_public_query_and_returns_github_links() {
        let server = Server::start().await;
        let client = GitHub::with_base_url(&server.url(), Some("example-token")).unwrap();
        let body =
            json!({"items": [repo(1, "acme/search.rs", false), repo(2, "acme/hidden", true)]})
                .to_string();
        let headers = format!(
            "Link: <{}search/repositories?page=2>; rel=\"next\"\r\n",
            server.url()
        );
        let (result, request) = tokio::join!(
            client.search_public("中文 search & Rust is:private"),
            server.reply(200, &headers, &body)
        );

        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        let mut request_line = request.lines().next().unwrap().split_whitespace();
        assert_eq!(request_line.next(), Some("GET"));
        let url = Url::parse(&server.url())
            .unwrap()
            .join(request_line.next().unwrap())
            .unwrap();
        assert_eq!(url.path(), "/search/repositories");
        assert_eq!(
            url.query_pairs().find(|(key, _)| key == "q").unwrap().1,
            "中文 search & Rust is:private is:public"
        );
        let candidates = result.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].html_url, "https://github.com/acme/search.rs");
    }

    #[tokio::test]
    async fn sync_client_reads_all_starred_pages_with_the_configured_credential() {
        let server = Server::start().await;
        let client = GitHub::with_base_url(&server.url(), Some("example-token")).unwrap();
        let first = json!([{
            "starred_at": "2025-05-09T10:11:12Z",
            "repo": repo(10, "acme/first", true),
        }]);
        let next = json!([{
            "starred_at": "2025-05-03T10:11:12Z",
            "repo": repo(20, "acme/second", false),
        }]);
        let (result, ()) = tokio::join!(client.starred(), async {
            let link = format!(
                "Link: <{}user/starred?page=8>; rel=\"last\", <{}user/starred?page=2&per_page=100>; rel=\"next\"\r\n",
                server.url(), server.url(),
            );
            let first = server.reply(200, &link, &first.to_string()).await;
            let next = server.reply(200, "", &next.to_string()).await;
            assert_eq!(
                first.lines().next(),
                Some("GET /user/starred?per_page=100 HTTP/1.1")
            );
            assert_eq!(
                next.lines().next(),
                Some("GET /user/starred?page=2&per_page=100 HTTP/1.1")
            );
            for request in [first, next] {
                assert!(request.contains("authorization: Bearer example-token"));
                assert!(request.contains("accept: application/vnd.github.star+json"));
            }
        });
        let repos = result.unwrap();
        assert_eq!(
            repos.iter().map(|repo| repo.id).collect::<Vec<_>>(),
            [10, 20]
        );
        assert!(repos[0].private);
        assert_eq!(repos[1].starred_at.as_deref(), Some("2025-05-03T10:11:12Z"));
    }

    #[tokio::test]
    async fn remote_errors_distinguish_bad_credentials_permissions_and_rate_limits() {
        let server = Server::start().await;
        let client = GitHub::with_base_url(&server.url(), Some("example-token")).unwrap();
        for (status, headers, body, message) in [
            (401, "", "{}", "Check the credential source in Settings"),
            (403, "", "{}", "Check the credential's read permissions"),
            (403, "Retry-After: 60\r\n", "{}", "Retry after 60 seconds"),
            (
                403,
                "",
                r#"{"message":"You have exceeded a secondary rate limit."}"#,
                "Wait before retrying",
            ),
            (429, "", "{}", "Wait before retrying"),
        ] {
            let (result, _) = tokio::join!(client.starred(), server.reply(status, headers, body));
            let error = result.unwrap_err().to_string();
            assert!(error.contains(message), "unexpected error: {error}");
            assert!(!error.contains("example-token"));
        }
    }

    #[tokio::test]
    async fn reads_selected_readme_as_text_and_handles_absence() {
        let server = Server::start().await;
        let client = GitHub::with_base_url(&server.url(), Some("example-token")).unwrap();
        let readme = json!({
            "encoding": "base64",
            "content": "IyDkuK3mlocK\n",
            "html_url": "https://github.com/acme/search.rs/blob/main/docs/README.md",
        })
        .to_string();
        let (result, request) = tokio::join!(client.readme(1), server.reply(200, "", &readme),);
        assert_eq!(
            request.lines().next(),
            Some("GET /repositories/1/readme HTTP/1.1")
        );
        assert!(!request.contains("authorization:"));
        assert_eq!(result.unwrap().as_deref(), Some("# 中文\n"));

        let (result, _) = tokio::join!(client.readme(2), server.reply(404, "", "{}"),);
        assert_eq!(result.unwrap(), None);
    }

    #[tokio::test]
    async fn rejects_repository_links_outside_github() {
        let server = Server::start().await;
        let client = GitHub::with_base_url(&server.url(), None).unwrap();
        let mut invalid = repo(1, "acme/lookup", false);
        invalid["html_url"] = json!("https://github.com.example.org/acme/lookup");
        let body = json!({"items": [invalid]}).to_string();
        let (result, _) =
            tokio::join!(client.search_public("lookup"), server.reply(200, "", &body));
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid repository URL"));
    }

    #[tokio::test]
    async fn slow_pagination_reports_progress_preserves_the_library_and_retries_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("library.sqlite");
        let store = Store::open(&path).await.unwrap();
        let observer = Store::open(&path).await.unwrap();
        let server = Server::start().await;
        let client = GitHub::with_base_url(&server.url(), Some("example-token")).unwrap();
        let original = json!([star(1, "acme/old-name"), star(2, "acme/retired")]).to_string();
        let (result, _) = tokio::join!(client.sync(&store), server.reply(200, "", &original));
        assert_eq!(result.unwrap().starred, 2);
        let tags = [String::from("搜索")];
        store
            .edit_repository(1, Some("Works offline"), Some(&tags))
            .await
            .unwrap();
        let previous = observer
            .list_repositories(&Default::default())
            .await
            .unwrap();
        let last_successful = observer.sync_status().await.unwrap().last_successful_at;
        let rename = json!([star(1, "acme/new-name")]).to_string();
        let more = "Link: <?page=2>; rel=\"next\"\r\n";
        let fetching = |pages, repositories| {
            Some(SyncProgress {
                phase: SyncPhase::Fetching,
                pages,
                repositories,
            })
        };

        let (result, ()) = tokio::join!(client.sync(&store), async {
            let first = server.accept().await;
            assert_eq!(
                observer.sync_status().await.unwrap().progress,
                fetching(0, 0)
            );
            first.respond(200, more, &rename).await;

            let second = server.accept().await;
            let status = observer.sync_status().await.unwrap();
            assert!(status.in_progress);
            assert_eq!(status.progress, fetching(1, 1));
            assert_eq!(status.last_successful_at, last_successful);
            assert_eq!(
                observer
                    .list_repositories(&Default::default())
                    .await
                    .unwrap(),
                previous
            );
            second.respond(502, "", "{}").await;
        });
        assert!(result.unwrap_err().to_string().contains("502"));
        let status = observer.sync_status().await.unwrap();
        assert!(!status.in_progress);
        assert_eq!(status.progress, None);
        assert_eq!(status.last_successful_at, last_successful);
        assert_eq!(status.last_result.unwrap().starred, 2);
        assert!(status.last_error.unwrap().contains("502"));
        assert_eq!(
            observer
                .list_repositories(&Default::default())
                .await
                .unwrap(),
            previous
        );

        sqlx::query(
            "CREATE TRIGGER verify_saving BEFORE INSERT ON repositories
             BEGIN
                 SELECT RAISE(ABORT, 'missing saving progress') WHERE NOT EXISTS (
                     SELECT 1 FROM library_sync_status WHERE running
                       AND json_extract(progress, '$.phase') = 'saving'
                       AND json_extract(progress, '$.pages') = 3
                       AND json_extract(progress, '$.repositories') = 2
                 );
             END",
        )
        .execute(store.pool())
        .await
        .unwrap();
        let (result, ()) = tokio::join!(client.sync(&store), async {
            let first = server.accept().await;
            let status = observer.sync_status().await.unwrap();
            assert_eq!(status.progress, fetching(0, 0));
            assert!(status.last_error.is_none());
            first.respond(200, more, &rename).await;
            server
                .reply(200, "Link: <?page=3>; rel=\"next\"\r\n", "[]")
                .await;

            let third = server.accept().await;
            assert_eq!(
                observer.sync_status().await.unwrap().progress,
                fetching(2, 1)
            );
            assert_eq!(
                observer
                    .list_repositories(&Default::default())
                    .await
                    .unwrap(),
                previous
            );
            third
                .respond(200, "", &json!([star(3, "acme/new-star")]).to_string())
                .await;
        });
        let summary = result.unwrap();
        assert_eq!(
            (summary.starred, summary.newly_starred, summary.unstarred),
            (2, 1, 1)
        );
        let status = observer.sync_status().await.unwrap();
        assert!(!status.in_progress);
        assert_eq!(status.progress, None);
        assert!(status.last_error.is_none());
        assert_eq!(status.last_result, Some(summary));
        let renamed = observer.get_repository(1).await.unwrap().unwrap();
        assert_eq!(renamed.metadata.full_name, "acme/new-name");
        assert_eq!(renamed.note, "Works offline");
        assert_eq!(renamed.tags, tags);
        assert!(!store.get_repository(2).await.unwrap().unwrap().starred);
        assert!(store.get_repository(3).await.unwrap().unwrap().starred);
    }

    #[tokio::test]
    async fn caches_the_selected_repositorys_readme_and_keeps_it_after_a_failed_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("library.sqlite"))
            .await
            .unwrap();
        let server = Server::start().await;
        let client = GitHub::with_base_url(&server.url(), Some("example-token")).unwrap();
        let stars = json!([star(1, "acme/selected")]).to_string();
        let (result, _) = tokio::join!(client.sync(&store), server.reply(200, "", &stars));
        result.unwrap();
        let metadata = store.get_repository(1).await.unwrap().unwrap().metadata;

        let source = "https://github.com/acme/selected/blob/trunk/README.md";
        let body = json!({"encoding": "base64", "content": "IyBMb29rdXAK", "html_url": source})
            .to_string();
        let (result, request) = tokio::join!(
            client.refresh_readme(&store, &metadata),
            server.reply(200, "", &body)
        );
        assert!(!request.contains("authorization:"));
        let readme = result.unwrap().unwrap();
        assert_eq!(readme.source_url, source);
        assert_eq!(readme.content, "# Lookup\n");

        let (result, _) = tokio::join!(
            client.refresh_readme(&store, &metadata),
            server.reply(403, "", "{}")
        );
        assert!(result.is_err());
        assert_eq!(store.get_readme(1).await.unwrap(), Some(readme));
    }

    #[tokio::test]
    async fn explicitly_selected_private_repository_uses_authenticated_readme_access() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("library.sqlite"))
            .await
            .unwrap();
        let server = Server::start().await;
        let client = GitHub::with_base_url(&server.url(), Some("example-token")).unwrap();
        let stars = json!([{
            "starred_at": "2025-05-09T10:11:12Z",
            "repo": repo(1, "acme/selected", true),
        }])
        .to_string();
        let (result, _) = tokio::join!(client.sync(&store), server.reply(200, "", &stars));
        result.unwrap();
        let metadata = store.get_repository(1).await.unwrap().unwrap().metadata;
        let body = json!({
            "encoding": "base64",
            "content": "IyBMb29rdXAK",
            "html_url": "https://github.com/new-owner/renamed/blob/trunk/README.md",
        })
        .to_string();
        let (result, request) = tokio::join!(
            client.refresh_readme(&store, &metadata),
            server.reply(200, "", &body),
        );
        assert_eq!(
            request.lines().next(),
            Some("GET /repositories/1/readme HTTP/1.1")
        );
        assert!(request.contains("authorization: Bearer example-token"));
        assert_eq!(result.unwrap().unwrap().content, "# Lookup\n");
    }

    #[tokio::test]
    async fn competing_sync_is_rejected_while_the_first_sync_waits_for_a_page() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("library.sqlite");
        let store = Store::open(&path).await.unwrap();
        let other_entry_point = Store::open(&path).await.unwrap();
        let server = Server::start().await;
        let client = GitHub::with_base_url(&server.url(), Some("example-token")).unwrap();
        let (result, ()) = tokio::join!(client.sync(&store), async {
            server
                .reply(200, "Link: <?page=2>; rel=\"next\"\r\n", "[]")
                .await;
            assert!(other_entry_point.sync_status().await.unwrap().in_progress);
            let duplicate =
                tokio::time::timeout(Duration::from_secs(2), client.sync(&other_entry_point))
                    .await
                    .unwrap()
                    .unwrap_err();
            assert!(duplicate.to_string().contains("already in progress"));
            assert!(other_entry_point
                .list_repositories(&Default::default())
                .await
                .unwrap()
                .is_empty());
            let final_page = json!([star(1, "acme/only")]).to_string();
            let request = server.reply(200, "", &final_page).await;
            assert_eq!(
                request.lines().next(),
                Some("GET /user/starred?page=2 HTTP/1.1")
            );
        });
        assert_eq!(result.unwrap().starred, 1);
        assert!(!other_entry_point.sync_status().await.unwrap().in_progress);
        assert_eq!(
            other_entry_point
                .list_repositories(&Default::default())
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn authenticated_pagination_does_not_follow_cross_origin_links() {
        let server = Server::start().await;
        let other_server = Server::start().await;
        let client = GitHub::with_base_url(&server.url(), Some("example-token")).unwrap();
        let link = format!(
            "Link: <{}user/starred?page=2>; rel=\"next\"\r\n",
            other_server.url()
        );
        let (result, _) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(client.starred(), server.reply(200, &link, "[]"))
        })
        .await
        .unwrap();
        assert!(matches!(result, Err(GitHubError::InvalidPagination)));
    }
}
