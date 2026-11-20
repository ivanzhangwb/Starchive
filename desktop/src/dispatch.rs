use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use starchive_core::{
    agent::{ContextSelection, Conversations, PiAgent, RepositoryDetails, Session},
    github::GitHub,
    library::{FieldEdit, PatchResult},
    runtime::{self, AgentLibrary},
    RepoQuery, Store,
};

mod view;

#[derive(Clone, Copy)]
struct Id(i64);

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0)
            .map(Self)
            .ok_or_else(|| serde::de::Error::custom("Expected a positive decimal ID string"))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum StarStatus {
    Starred,
    Unstarred,
    All,
}

#[derive(Deserialize)]
#[serde(tag = "action", content = "payload", rename_all = "snake_case")]
enum Action {
    RepoList {
        query: String,
        language: Option<String>,
        tag: Option<String>,
        status: StarStatus,
        offset: usize,
        limit: usize,
    },
    RepoGet {
        repo_id: Id,
        session_id: Option<Id>,
    },
    RepoUpdate {
        repo_id: Id,
        note: Option<FieldEdit<String>>,
        tags: Option<FieldEdit<Vec<String>>>,
    },
    RepoReadme {
        repo_id: Id,
        session_id: Option<Id>,
    },
    SyncStatus {},
    SyncStart {},
    AgentList {},
    SessionList {},
    SessionCreate {
        agent_id: Option<String>,
    },
    SessionGet {
        session_id: Id,
    },
    SessionSetAgent {
        session_id: Id,
        agent_id: String,
    },
    SessionSetContext {
        session_id: Id,
        repo_id: Id,
        include_private: bool,
        include_notes: bool,
    },
    SessionRemoveContext {
        session_id: Id,
        repo_id: Id,
    },
    AgentSend {
        session_id: Id,
        text: String,
    },
    AgentExpand {
        session_id: Id,
        public_query: String,
    },
    OpenGithub {
        url: String,
    },
}

const PI_ID: &str = "pi";

pub struct Desktop {
    store: Store,
    conversations: Conversations,
    pi_path: PathBuf,
}

impl Desktop {
    pub async fn open(path: &Path, pi_path: PathBuf) -> Result<Self> {
        let store = Store::open(path).await?;
        let conversations = Conversations::new(&store);
        Ok(Self {
            store,
            conversations,
            pi_path,
        })
    }

    pub async fn dispatch(&self, action: &str, payload: Value) -> Result<Value> {
        let action = serde_json::from_value(json!({"action": action, "payload": payload}))
            .context("Invalid desktop request")?;
        let store = &self.store;
        let sessions = &self.conversations;
        match action {
            Action::RepoList {
                query,
                language,
                tag,
                status,
                offset,
                limit,
            } => {
                let starred = match status {
                    StarStatus::Starred => Some(true),
                    StarStatus::Unstarred => Some(false),
                    StarStatus::All => None,
                };
                let page = store
                    .page_repositories(
                        &RepoQuery {
                            query: Some(query),
                            language,
                            tag,
                            starred,
                            ..RepoQuery::default()
                        },
                        offset,
                        limit,
                    )
                    .await?;
                let repos: Vec<_> = page.repositories.iter().map(view::library_repo).collect();
                Ok(
                    json!({"repos": repos, "total": page.total, "languages": page.languages, "tags": page.tags}),
                )
            }
            Action::RepoGet {
                repo_id: Id(id),
                session_id,
            } => Ok(
                match sessions
                    .repository_details(id, session_id.map(|id| id.0))
                    .await?
                {
                    Some(RepositoryDetails::Library { repo, readme }) => {
                        json!({"repo": view::library_repo(&repo), "readme": readme, "editable": true})
                    }
                    Some(RepositoryDetails::Candidate { repo, readme }) => {
                        json!({"repo": view::candidate_repo(&repo, None), "readme": readme, "editable": false})
                    }
                    None => Value::Null,
                },
            ),
            Action::RepoUpdate {
                repo_id: Id(id),
                note,
                tags,
            } => {
                let result = store
                    .patch_repository(id, note.as_ref(), tags.as_ref())
                    .await?;
                let (status, repo) = match result {
                    PatchResult::Saved(repo) => ("saved", repo),
                    PatchResult::Conflict(repo) => ("conflict", repo),
                };
                Ok(json!({"status": status, "repo": view::library_repo(&repo)}))
            }
            Action::RepoReadme {
                repo_id: Id(id),
                session_id,
            } => {
                let readme = self.fetch_readme(id, session_id).await?;
                Ok(json!({"readme": readme}))
            }
            Action::SyncStatus {} => Ok(view::sync_status(store.sync_status().await?)),
            Action::SyncStart {} => {
                GitHub::sync_from_env(store).await?;
                Ok(view::sync_status(store.sync_status().await?))
            }
            Action::AgentList {} => Ok(json!([{
                "id": PI_ID, "name": "pi", "available": runtime::find_executable(&self.pi_path).is_some(),
            }])),
            Action::SessionList {} => Ok(json!(sessions
                .list()
                .await?
                .iter()
                .map(view::session_summary)
                .collect::<Vec<_>>())),
            Action::SessionCreate { agent_id } => {
                if let Some(id) = &agent_id {
                    check_agent(id)?;
                }
                self.session_view(
                    sessions
                        .create_with_agent("新建会话", agent_id.as_deref())
                        .await?,
                )
                .await
            }
            Action::SessionGet { session_id: Id(id) } => {
                self.session_view(sessions.get(id).await?).await
            }
            Action::SessionSetAgent {
                session_id: Id(id),
                agent_id,
            } => {
                check_agent(&agent_id)?;
                self.session_view(sessions.set_agent(id, &agent_id).await?)
                    .await
            }
            Action::SessionSetContext {
                session_id: Id(session_id),
                repo_id: Id(repo_id),
                include_private,
                include_notes,
            } => {
                let local = store.get_repository(repo_id).await?;
                if local.is_none() {
                    ensure!(sessions.public_candidate(session_id, repo_id).await?.is_some(),
                        "Repository is not in the local library or this conversation's public search results");
                }
                let selection = ContextSelection {
                    repo_id,
                    include_private: include_private
                        && local.as_ref().is_some_and(|repo| repo.metadata.private),
                    include_notes: include_notes && local.is_some(),
                };
                self.session_view(sessions.set_context(session_id, selection).await?)
                    .await
            }
            Action::SessionRemoveContext {
                session_id: Id(session_id),
                repo_id: Id(repo_id),
            } => {
                self.session_view(sessions.remove_context(session_id, repo_id).await?)
                    .await
            }
            Action::AgentSend {
                session_id: Id(id),
                text,
            } => {
                let session = sessions.get(id).await?;
                check_agent(
                    session
                        .agent_id
                        .as_deref()
                        .context("Choose a local agent first")?,
                )?;
                let executable =
                    runtime::find_executable(&self.pi_path).unwrap_or_else(|| self.pi_path.clone());
                let library = AgentLibrary::load(store).await?;
                self.session_view(
                    sessions
                        .ask(id, &text, &library, &PiAgent::new(executable))
                        .await?,
                )
                .await
            }
            Action::AgentExpand {
                session_id: Id(id),
                public_query,
            } => {
                self.session_view(
                    sessions
                        .expand(id, &public_query, &GitHub::new(None)?)
                        .await?,
                )
                .await
            }
            Action::OpenGithub { url } => {
                let url = GitHub::repository_url(&url)?;
                open::that_detached(url.as_str())
                    .context("Could not open GitHub in your browser")?;
                Ok(Value::Null)
            }
        }
    }

    async fn session_view(&self, session: Session) -> Result<Value> {
        view::session(&self.store, session).await
    }

    async fn fetch_readme(&self, repo_id: i64, session_id: Option<Id>) -> Result<Option<String>> {
        if let Some(repo) = self.store.get_repository(repo_id).await? {
            return Ok(runtime::refresh_readme(&self.store, &repo.metadata)
                .await?
                .map(|readme| readme.content));
        }
        let Id(session_id) =
            session_id.context("Select a saved repository or a public search candidate")?;
        ensure!(
            self.conversations
                .public_candidate(session_id, repo_id)
                .await?
                .is_some(),
            "Repository is not in the local library or this conversation's public search results"
        );
        let Some(readme) = GitHub::new(None)?.readme(repo_id).await? else {
            return self
                .conversations
                .candidate_readme(session_id, repo_id)
                .await;
        };
        self.conversations
            .cache_candidate_readme(session_id, repo_id, &readme)
            .await?;
        Ok(Some(readme))
    }
}

fn check_agent(agent_id: &str) -> Result<()> {
    ensure!(agent_id == PI_ID, "Choose a supported local agent (pi)");
    Ok(())
}

#[cfg(test)]
mod tests;
