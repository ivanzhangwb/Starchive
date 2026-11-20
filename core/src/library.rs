use std::{
    collections::HashSet,
    fs::{File, OpenOptions, TryLockError},
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    types::Json,
    FromRow, QueryBuilder, Sqlite, SqlitePool,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, FromRow)]
pub struct RepoMetadata {
    pub id: i64,
    pub full_name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub html_url: String,
    pub private: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, FromRow)]
pub struct Repository {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub metadata: RepoMetadata,
    pub starred: bool,
    pub note: String,
    #[sqlx(json)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct FieldEdit<T> {
    pub expected: T,
    pub value: T,
}

pub enum PatchResult {
    Saved(Repository),
    Conflict(Repository),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoQuery {
    pub query: Option<String>,
    pub language: Option<String>,
    pub tag: Option<String>,
    pub starred: Option<bool>,
    pub public_only: bool,
    /// Skip personal notes when matching the query.
    pub exclude_notes: bool,
}

pub struct RepositoryPage {
    pub repositories: Vec<Repository>,
    pub total: i64,
    pub languages: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, FromRow)]
pub struct Readme {
    pub content: String,
    pub source_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncResult {
    pub starred: u64,
    pub newly_starred: u64,
    pub unstarred: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPhase {
    Preparing,
    Fetching,
    Retrying,
    Saving,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncProgress {
    pub phase: SyncPhase,
    pub pages: usize,
    pub repositories: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStatus {
    pub in_progress: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<SyncProgress>,
    pub last_attempt_at: Option<String>,
    pub last_successful_at: Option<String>,
    pub last_result: Option<SyncResult>,
    pub last_error: Option<String>,
}

#[derive(FromRow)]
struct SyncState {
    running: bool,
    progress: Option<Json<SyncProgress>>,
    last_attempt_at: Option<String>,
    last_successful_at: Option<String>,
    last_result: Option<Json<SyncResult>>,
    last_error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Store {
    pool: SqlitePool,
    path: PathBuf,
    sync_lock_path: PathBuf,
}

#[derive(Debug)]
#[must_use = "hold the lease until pagination and snapshot recording finish"]
pub struct SyncLease {
    pool: SqlitePool,
    progress_pool: SqlitePool,
    _lock: FileLock,
}

#[derive(Debug)]
pub(crate) struct FileLock(File);

impl Drop for FileLock {
    fn drop(&mut self) {
        // A concurrent subprocess spawn may temporarily retain the open file.
        let _ = self.0.unlock();
    }
}

impl Store {
    pub async fn open(path: &Path) -> Result<Self> {
        let path = path.to_owned();
        let (path, _init_lock) = tokio::task::spawn_blocking(move || -> Result<_> {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Create data directory {}", parent.display()))?;
            }
            open_local_file(&path).with_context(|| format!("Open data file {}", path.display()))?;
            let path = path
                .canonicalize()
                .with_context(|| format!("Resolve data file {}", path.display()))?;
            // SQLite's SQLx migrator leaves locking to callers.
            let init_path = lock_path(&path, "-starchive-init.lock");
            let lock = open_local_file(&init_path)
                .with_context(|| format!("Open migration lock {}", init_path.display()))?;
            lock.lock()
                .with_context(|| format!("Acquire migration lock {}", init_path.display()))?;
            Ok((path, FileLock(lock)))
        })
        .await??;

        let options = SqliteConnectOptions::new()
            .filename(&path)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .with_context(|| format!("Connect to SQLite database {}", path.display()))?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("Migrate the Starfolio database")?;

        Ok(Self {
            pool,
            sync_lock_path: lock_path(&path, "-starchive-sync.lock"),
            path,
        })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub(crate) async fn lock_conversation(&self, session_id: i64) -> Result<FileLock> {
        let path = lock_path(&self.path, &format!("-starchive-session-{session_id}.lock"));
        try_lock(&path)
            .await?
            .context("The conversation is busy. Wait for the current reply or change, then retry.")
    }

    pub async fn get_repository(&self, id: i64) -> Result<Option<Repository>> {
        Ok(sqlx::query_as("SELECT * FROM repositories WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn get_repository_by_name(&self, full_name: &str) -> Result<Option<Repository>> {
        Ok(sqlx::query_as(
            "SELECT * FROM repositories WHERE full_name = ? ORDER BY starred DESC, id DESC LIMIT 1",
        )
        .bind(full_name.trim())
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn list_repositories(&self, query: &RepoQuery) -> Result<Vec<Repository>> {
        Ok(repository_query("SELECT r.*", query)
            .push(" ORDER BY r.full_name, r.id")
            .build_query_as()
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn repositories_by_id(&self, ids: &[i64]) -> Result<Vec<Repository>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_as(
            "SELECT r.* FROM repositories r JOIN json_each(?) AS ids ON r.id = ids.value",
        )
        .bind(Json(ids))
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn page_repositories(
        &self,
        query: &RepoQuery,
        offset: usize,
        limit: usize,
    ) -> Result<RepositoryPage> {
        let mut snapshot = self.pool.begin().await?;
        let total = repository_query("SELECT count(*)", query)
            .build_query_scalar()
            .fetch_one(&mut *snapshot)
            .await?;
        let repositories = repository_query("SELECT r.*", query)
            .push(" ORDER BY r.full_name, r.id LIMIT ")
            .push_bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .push(" OFFSET ")
            .push_bind(i64::try_from(offset).unwrap_or(i64::MAX))
            .build_query_as()
            .fetch_all(&mut *snapshot)
            .await?;
        let languages = sqlx::query_scalar(
            "SELECT DISTINCT language FROM repositories
             WHERE (? IS NULL OR starred = ?) AND (NOT ? OR NOT private) AND language IS NOT NULL
             ORDER BY language",
        )
        .bind(query.starred)
        .bind(query.starred)
        .bind(query.public_only)
        .fetch_all(&mut *snapshot)
        .await?;
        let tags = sqlx::query_scalar(
            "SELECT DISTINCT tag.value FROM repositories, json_each(repositories.tags) AS tag
             WHERE (? IS NULL OR starred = ?) AND (NOT ? OR NOT private) ORDER BY tag.value",
        )
        .bind(query.starred)
        .bind(query.starred)
        .bind(query.public_only)
        .fetch_all(&mut *snapshot)
        .await?;
        snapshot.commit().await?;
        Ok(RepositoryPage {
            repositories,
            total,
            languages,
            tags,
        })
    }

    pub async fn edit_repository(
        &self,
        id: i64,
        note: Option<&str>,
        tags: Option<&[String]>,
    ) -> Result<Repository> {
        self.update_repository(id, note, tags, None, None)
            .await?
            .with_context(|| format!("Repository {id} is missing from the local library"))
    }

    pub async fn patch_repository(
        &self,
        id: i64,
        note: Option<&FieldEdit<String>>,
        tags: Option<&FieldEdit<Vec<String>>>,
    ) -> Result<PatchResult> {
        if let Some(repo) = self
            .update_repository(
                id,
                note.map(|edit| edit.value.as_str()),
                tags.map(|edit| edit.value.as_slice()),
                note.map(|edit| edit.expected.as_str()),
                tags.map(|edit| edit.expected.as_slice()),
            )
            .await?
        {
            return Ok(PatchResult::Saved(repo));
        }
        let current = self
            .get_repository(id)
            .await?
            .with_context(|| format!("Repository {id} is missing from the local library"))?;
        Ok(PatchResult::Conflict(current))
    }

    async fn update_repository(
        &self,
        id: i64,
        note: Option<&str>,
        tags: Option<&[String]>,
        expected_note: Option<&str>,
        expected_tags: Option<&[String]>,
    ) -> Result<Option<Repository>> {
        let tags = tags.map(|tags| {
            let mut seen = HashSet::new();
            tags.iter()
                .map(|tag| tag.trim())
                .filter(|tag| !tag.is_empty() && seen.insert(tag.to_ascii_lowercase()))
                .collect::<Vec<_>>()
        });
        Ok(sqlx::query_as(
            "UPDATE repositories
             SET note = COALESCE(?1, note), tags = COALESCE(?2, tags)
             WHERE id = ?3 AND (?4 IS NULL OR note = ?4)
               AND (?5 IS NULL OR json(tags) = json(?5))
             RETURNING *",
        )
        .bind(note)
        .bind(tags.map(Json))
        .bind(id)
        .bind(expected_note)
        .bind(expected_tags.map(Json))
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn get_readme(&self, id: i64) -> Result<Option<Readme>> {
        Ok(sqlx::query_as(
            "SELECT content, source_url FROM repository_readmes WHERE repository_id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn get_readme_for(
        &self,
        id: i64,
        full_name: &str,
        private: bool,
    ) -> Result<Option<Readme>> {
        self.readme_for(id, full_name, private, false).await
    }

    /// Legacy cache entries remain available offline while model input requires verified identity.
    pub(crate) async fn verified_readme_for(
        &self,
        id: i64,
        full_name: &str,
        private: bool,
    ) -> Result<Option<Readme>> {
        self.readme_for(id, full_name, private, true).await
    }

    async fn readme_for(
        &self,
        id: i64,
        full_name: &str,
        private: bool,
        verified_only: bool,
    ) -> Result<Option<Readme>> {
        Ok(sqlx::query_as(
            "SELECT readme.content, readme.source_url FROM repository_readmes readme
             JOIN repositories repo ON repo.id = readme.repository_id
             WHERE repo.id = ? AND repo.full_name = ? AND repo.private = ?
               AND (NOT ? OR readme.identity_verified)",
        )
        .bind(id)
        .bind(full_name)
        .bind(private)
        .bind(verified_only)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn cache_readme(&self, repo: &RepoMetadata, readme: &Readme) -> Result<bool> {
        let result = sqlx::query(
            "INSERT INTO repository_readmes (repository_id, content, source_url, identity_verified)
             SELECT id, ?, ?, TRUE FROM repositories WHERE id = ? AND full_name = ? AND private = ?
             ON CONFLICT(repository_id) DO UPDATE SET
                 content = excluded.content, source_url = excluded.source_url, identity_verified = TRUE",
        )
        .bind(&readme.content)
        .bind(&readme.source_url)
        .bind(repo.id)
        .bind(&repo.full_name)
        .bind(repo.private)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() != 0)
    }

    pub async fn try_sync_lease(&self) -> Result<Option<SyncLease>> {
        let Some(lock) = try_lock(&self.sync_lock_path).await? else {
            return Ok(None);
        };
        sqlx::query(
            "UPDATE library_sync_status SET running = TRUE, progress = ?,
                 last_attempt_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), last_error = NULL
             WHERE id = 1",
        )
        .bind(Json(SyncProgress {
            phase: SyncPhase::Preparing,
            pages: 0,
            repositories: 0,
        }))
        .execute(&self.pool)
        .await
        .context("Record the start of GitHub sync")?;

        Ok(Some(SyncLease {
            pool: self.pool.clone(),
            // A progress notification may skip a page when another process is editing.
            // The final snapshot still uses the regular connection and its write timeout.
            progress_pool: SqlitePoolOptions::new()
                .max_connections(1)
                .connect_lazy_with(
                    (*self.pool.connect_options())
                        .clone()
                        .busy_timeout(Duration::ZERO),
                ),
            _lock: lock,
        }))
    }

    pub async fn sync_status(&self) -> Result<SyncStatus> {
        let mut state = self.sync_state().await?;
        let mut in_progress = false;
        if state.running {
            in_progress = match try_lock(&self.sync_lock_path).await? {
                None => true,
                Some(_lock) => {
                    // Sync may have finished between the first read and the lock check.
                    state = self.sync_state().await?;
                    false
                }
            };
        }
        let last_error = if state.running && !in_progress {
            Some("The previous GitHub sync was interrupted. Start sync again to retry.".to_owned())
        } else {
            state.last_error
        };
        Ok(SyncStatus {
            in_progress,
            progress: state
                .progress
                .filter(|_| in_progress)
                .map(|progress| progress.0),
            last_attempt_at: state.last_attempt_at,
            last_successful_at: state.last_successful_at,
            last_result: state.last_result.map(|result| result.0),
            last_error,
        })
    }

    async fn sync_state(&self) -> Result<SyncState> {
        sqlx::query_as("SELECT * FROM library_sync_status WHERE id = 1")
            .fetch_one(&self.pool)
            .await
            .context("Read GitHub sync status")
    }
}

impl SyncLease {
    pub(crate) async fn set_progress(
        &self,
        phase: SyncPhase,
        pages: usize,
        repositories: usize,
    ) -> Result<()> {
        let result = sqlx::query("UPDATE library_sync_status SET progress = ? WHERE id = 1")
            .bind(Json(SyncProgress {
                phase,
                pages,
                repositories,
            }))
            .execute(&self.progress_pool)
            .await;
        if let Err(sqlx::Error::Database(error)) = &result {
            // SQLite's extended result codes keep the primary code in the low byte.
            let code = error.code().and_then(|value| value.parse::<i32>().ok());
            if matches!(code.map(|value| value & 0xff), Some(5 | 6)) {
                return Ok(());
            }
        }
        result.context("Record GitHub sync progress")?;
        Ok(())
    }

    pub async fn complete_snapshot(self, repositories: &[RepoMetadata]) -> Result<SyncResult> {
        self.complete_fetched_snapshot(repositories, 0).await
    }

    pub(crate) async fn complete_fetched_snapshot(
        self,
        repositories: &[RepoMetadata],
        pages: usize,
    ) -> Result<SyncResult> {
        match self.apply_snapshot(repositories, pages).await {
            Ok(result) => Ok(result),
            Err(error) => Err(self.fail(error).await),
        }
    }

    pub(crate) async fn fail(self, error: anyhow::Error) -> anyhow::Error {
        match self.record_failure(&format!("{error:#}")).await {
            Ok(()) => error,
            Err(status_error) => error.context(format!(
                "Could not record the sync failure: {status_error:#}"
            )),
        }
    }

    pub async fn record_failure(self, error: &str) -> Result<()> {
        sqlx::query(
            "UPDATE library_sync_status
             SET running = FALSE, progress = NULL, last_error = ? WHERE id = 1",
        )
        .bind(error)
        .execute(&self.pool)
        .await
        .context("Record GitHub sync failure")?;
        Ok(())
    }

    async fn apply_snapshot(
        &self,
        repositories: &[RepoMetadata],
        pages: usize,
    ) -> Result<SyncResult> {
        self.set_progress(SyncPhase::Saving, pages, repositories.len())
            .await?;
        let ids: HashSet<_> = repositories.iter().map(|repo| repo.id).collect();
        ensure!(
            ids.len() == repositories.len(),
            "GitHub snapshot contains duplicate repository IDs"
        );

        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let retained: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM repositories
             WHERE starred AND id IN (SELECT value FROM json_each(?))",
        )
        .bind(Json(&ids))
        .fetch_one(&mut *transaction)
        .await?;
        let retired = sqlx::query(
            "UPDATE repositories SET starred = FALSE
             WHERE starred AND id NOT IN (SELECT value FROM json_each(?))",
        )
        .bind(Json(&ids))
        .execute(&mut *transaction)
        .await?;

        // Six binds per row, below even SQLite's legacy 999-variable limit.
        for batch in repositories.chunks(150) {
            let mut upsert = QueryBuilder::<Sqlite>::new(
                "INSERT INTO repositories (id, full_name, description, language, html_url, private, starred) ",
            );
            upsert.push_values(batch, |mut row, repo| {
                row.push_bind(repo.id)
                    .push_bind(&repo.full_name)
                    .push_bind(&repo.description)
                    .push_bind(&repo.language)
                    .push_bind(&repo.html_url)
                    .push_bind(repo.private)
                    .push("TRUE");
            });
            upsert.push(
                " ON CONFLICT(id) DO UPDATE SET
                     full_name = excluded.full_name, description = excluded.description,
                     language = excluded.language, html_url = excluded.html_url,
                     private = excluded.private, starred = TRUE",
            );
            upsert
                .build()
                .execute(&mut *transaction)
                .await
                .with_context(|| {
                    format!(
                        "Save GitHub repository batch starting at ID {}",
                        batch[0].id
                    )
                })?;
        }

        let result = SyncResult {
            starred: repositories.len() as u64,
            newly_starred: repositories.len() as u64 - retained as u64,
            unstarred: retired.rows_affected(),
        };
        sqlx::query(
            "UPDATE library_sync_status SET running = FALSE, progress = NULL,
                 last_successful_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                 last_result = ?, last_error = NULL
             WHERE id = 1",
        )
        .bind(Json(&result))
        .execute(&mut *transaction)
        .await?;
        transaction
            .commit()
            .await
            .context("Commit the GitHub snapshot")?;
        Ok(result)
    }
}

fn repository_query<'a>(select: &str, filters: &'a RepoQuery) -> QueryBuilder<'a, Sqlite> {
    let mut query = QueryBuilder::new(select);
    query.push(" FROM repositories r WHERE TRUE");
    if let Some(term) = filters
        .query
        .as_deref()
        .map(str::trim)
        .filter(|term| !term.is_empty())
    {
        query
            .push(" AND (instr(lower(r.full_name), lower(")
            .push_bind(term)
            .push(")) > 0 OR instr(lower(r.description), lower(")
            .push_bind(term)
            .push(")) > 0");
        if !filters.exclude_notes {
            query
                .push(" OR instr(lower(r.note), lower(")
                .push_bind(term)
                .push(")) > 0");
        }
        query.push(")");
    }
    if let Some(language) = &filters.language {
        query
            .push(" AND r.language = ")
            .push_bind(language.trim())
            .push(" COLLATE NOCASE");
    }
    if let Some(tag) = &filters.tag {
        query
            .push(" AND EXISTS (SELECT 1 FROM json_each(r.tags) WHERE value = ")
            .push_bind(tag.trim())
            .push(" COLLATE NOCASE)");
    }
    if let Some(starred) = filters.starred {
        query.push(" AND r.starred = ").push_bind(starred);
    }
    if filters.public_only {
        query.push(" AND NOT r.private");
    }
    query
}

fn lock_path(database: &Path, suffix: &str) -> PathBuf {
    let mut path = database.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

fn open_local_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

async fn try_lock(path: &Path) -> Result<Option<FileLock>> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let lock =
            open_local_file(&path).with_context(|| format!("Open lock {}", path.display()))?;
        match lock.try_lock() {
            Ok(()) => Ok(Some(FileLock(lock))),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("Acquire lock {}", path.display()))
            }
        }
    })
    .await?
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[tokio::test]
    async fn an_active_writer_does_not_stall_or_abort_page_progress() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let store = Store::open(&folder.path().join("library.sqlite")).await?;
        let lease = store.try_sync_lease().await?.unwrap();
        lease.set_progress(SyncPhase::Fetching, 0, 0).await?;
        let writer = store.pool.begin_with("BEGIN IMMEDIATE").await?;
        tokio::time::timeout(
            Duration::from_secs(1),
            lease.set_progress(SyncPhase::Fetching, 1, 100),
        )
        .await??;
        writer.rollback().await?;
        lease.set_progress(SyncPhase::Fetching, 2, 200).await?;
        assert_eq!(
            store.sync_status().await?.progress.unwrap().repositories,
            200
        );
        lease.complete_snapshot(&[]).await?;
        assert!(!store.sync_status().await?.in_progress);
        Ok(())
    }
}
