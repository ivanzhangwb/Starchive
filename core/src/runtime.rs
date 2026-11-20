use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use directories::BaseDirs;

use crate::{agent, github::GitHub, Readme, RepoMetadata, RepoQuery, Store};

/// Both app entry points use this path unless the caller selects a database explicitly.
pub fn database_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("STARCHIVE_DB_PATH").filter(|path| !path.is_empty()) {
        return Ok(path.into());
    }
    let dirs = BaseDirs::new().context("Set STARCHIVE_DB_PATH to a writable SQLite file")?;
    Ok(dirs
        .data_local_dir()
        .join("Starchive")
        .join("library.sqlite3"))
}

pub async fn refresh_readme(store: &Store, repo: &RepoMetadata) -> Result<Option<Readme>> {
    let github = if repo.private {
        GitHub::from_env()?
    } else {
        GitHub::new(None)?
    };
    github.refresh_readme(store, repo).await
}

pub fn pi_executable() -> PathBuf {
    env::var_os("STARCHIVE_PI_PATH")
        .filter(|path| !path.is_empty())
        .map_or_else(|| PathBuf::from("pi"), PathBuf::from)
}

pub fn find_executable(path: &Path) -> Option<PathBuf> {
    fn executable(path: &Path) -> bool {
        let Ok(metadata) = path.metadata() else {
            return false;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        metadata.is_file()
    }

    fn resolve(path: PathBuf) -> Option<PathBuf> {
        if executable(&path) {
            return Some(path);
        }
        #[cfg(windows)]
        if path.extension().is_none() {
            return ["exe", "cmd", "bat", "com"]
                .into_iter()
                .map(|ext| path.with_extension(ext))
                .find(|candidate| executable(candidate));
        }
        None
    }

    if path.is_absolute()
        || path
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
    {
        return resolve(path.to_owned());
    }
    env::split_paths(&env::var_os("PATH")?).find_map(|dir| {
        // A relative PATH entry may produce a bare filename, which would trigger a second PATH lookup.
        resolve(std::path::absolute(dir.join(path)).ok()?)
    })
}

pub struct AgentLibrary<'a> {
    store: &'a Store,
    repositories: BTreeMap<i64, agent::Repository>,
}

impl<'a> AgentLibrary<'a> {
    pub async fn load(store: &'a Store) -> Result<Self> {
        let repositories = store
            .list_repositories(&RepoQuery::default())
            .await?
            .into_iter()
            .map(|repo| {
                let meta = repo.metadata;
                (
                    meta.id,
                    agent::Repository {
                        id: meta.id,
                        full_name: meta.full_name,
                        description: meta.description,
                        language: meta.language,
                        topics: Vec::new(),
                        html_url: meta.html_url,
                        private: meta.private,
                        starred: repo.starred,
                        note: (!repo.note.is_empty()).then_some(repo.note),
                    },
                )
            })
            .collect();
        Ok(Self {
            store,
            repositories,
        })
    }
}

impl agent::Library for AgentLibrary<'_> {
    async fn repositories(&self) -> Result<Vec<agent::Repository>> {
        Ok(self.repositories.values().cloned().collect())
    }

    async fn cached_readme(&self, repo_id: i64) -> Result<Option<String>> {
        let Some(repo) = self.repositories.get(&repo_id) else {
            return Ok(None);
        };
        Ok(self
            .store
            .get_readme_for(repo.id, &repo.full_name, repo.private)
            .await?
            .map(|readme| readme.content))
    }
}

impl agent::PublicSearch for GitHub {
    async fn search_public(&self, query: &str) -> Result<Vec<agent::Repository>> {
        Ok(GitHub::search_public(self, query)
            .await?
            .into_iter()
            .map(|repo| agent::Repository {
                id: repo.id,
                full_name: repo.full_name,
                description: repo.description,
                language: repo.language,
                topics: repo.topics,
                html_url: repo.html_url,
                private: repo.private,
                starred: false,
                note: None,
            })
            .collect())
    }
}
