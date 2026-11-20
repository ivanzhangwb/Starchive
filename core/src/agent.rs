use std::collections::{HashMap, HashSet};
use std::future::Future;

use crate::Store;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

mod pi;
mod request;
pub use pi::PiAgent;
use request::{AgentHistory, InputScope};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: i64,
    pub title: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: i64,
    pub title: String,
    pub updated_at: String,
    pub agent_id: Option<String>,
    pub context: Vec<ContextSelection>,
    pub turns: Vec<Turn>,
    pub external_candidates: Vec<Repository>,
    pub expansions: Vec<Expansion>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSelection {
    pub repo_id: i64,
    pub include_private: bool,
    pub include_notes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub sequence: usize,
    pub message: String,
    pub response: AgentResponse,
    pub candidates: Vec<Repository>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input_scope: Option<InputScope>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expansion {
    pub sequence: usize,
    pub public_query: String,
    pub candidates: Vec<Repository>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    pub id: i64,
    pub full_name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub topics: Vec<String>,
    pub html_url: String,
    pub private: bool,
    pub starred: bool,
    pub note: Option<String>,
}

pub enum RepositoryDetails {
    Library {
        repo: crate::Repository,
        readme: Option<String>,
    },
    Candidate {
        repo: Repository,
        readme: Option<String>,
    },
}

pub trait Library: Sync {
    /// Local metadata, including retained unstarred repositories. No network access.
    fn repositories(&self) -> impl Future<Output = Result<Vec<Repository>>> + Send;
    /// Read from the local cache only.
    fn cached_readme(&self, repo_id: i64) -> impl Future<Output = Result<Option<String>>> + Send;
}

pub trait Agent: Sync {
    fn respond(&self, request: &AgentRequest)
        -> impl Future<Output = Result<AgentResponse>> + Send;
}

pub trait PublicSearch: Sync {
    /// Execute one public repository search with the user-approved terms.
    fn search_public(&self, query: &str) -> impl Future<Output = Result<Vec<Repository>>> + Send;
}

#[derive(Clone, Debug, Serialize)]
pub struct AgentRequest {
    pub message: String,
    history: Vec<AgentHistory>,
    pub selected_repositories: Vec<i64>,
    pub repositories: Vec<AgentRepository>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AgentRepository {
    pub id: i64,
    pub full_name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub topics: Vec<String>,
    pub starred: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResponse {
    pub text: String,
    pub recommendations: Vec<Recommendation>,
    pub gaps: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recommendation {
    pub repo_id: i64,
    pub fit: String,
    pub evidence: Vec<Evidence>,
    pub gaps: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub source: EvidenceSource,
    pub excerpt: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    RepositoryName,
    Description,
    Language,
    Topics,
    Readme,
    Note,
}

impl AgentRepository {
    fn contains(&self, evidence: &Evidence) -> bool {
        let excerpt = evidence.excerpt.as_str();
        if excerpt.trim().is_empty() {
            return false;
        }
        let source = match evidence.source {
            EvidenceSource::RepositoryName => Some(self.full_name.as_str()),
            EvidenceSource::Description => self.description.as_deref(),
            EvidenceSource::Language => self.language.as_deref(),
            EvidenceSource::Topics => {
                return self.topics.iter().any(|topic| topic.contains(excerpt))
            }
            EvidenceSource::Readme => self.readme.as_deref(),
            EvidenceSource::Note => self.note.as_deref(),
        };
        source.is_some_and(|text| text.contains(excerpt))
    }
}

#[derive(Default, Deserialize, Serialize)]
struct SessionData {
    agent_id: Option<String>,
    context: Vec<ContextSelection>,
    turns: Vec<Turn>,
    #[serde(default)]
    expansions: Vec<Expansion>,
}

struct SavedSession {
    id: i64,
    title: String,
    updated_at: String,
    revision: i64,
    data: SessionData,
}

impl SavedSession {
    fn into_session(self) -> Session {
        self.data.into_session(self.id, self.title, self.updated_at)
    }
}

impl SessionData {
    fn into_session(self, id: i64, title: String, updated_at: String) -> Session {
        let external_candidates = self.external_candidates().cloned().collect();
        Session {
            id,
            title,
            updated_at,
            agent_id: self.agent_id,
            context: self.context,
            turns: self.turns,
            external_candidates,
            expansions: self.expansions,
        }
    }

    fn next_sequence(&self) -> usize {
        self.turns.len() + self.expansions.len() + 1
    }

    fn external_candidates(&self) -> impl Iterator<Item = &Repository> {
        let mut seen = HashSet::new();
        self.expansions
            .iter()
            .rev()
            .flat_map(|expansion| &expansion.candidates)
            .filter(move |repo| seen.insert(repo.id))
    }
}

#[derive(Clone)]
pub struct Conversations {
    store: Store,
}

impl Conversations {
    pub fn new(store: &Store) -> Self {
        Self {
            store: store.clone(),
        }
    }

    pub async fn create(&self, title: &str) -> Result<Session> {
        self.create_with_agent(title, None).await
    }

    pub async fn create_with_agent(&self, title: &str, agent_id: Option<&str>) -> Result<Session> {
        let title = title.trim();
        anyhow::ensure!(!title.is_empty(), "Enter a title for the conversation");
        let data = SessionData {
            agent_id: agent_id.map(str::to_owned),
            ..SessionData::default()
        };
        let (id, updated_at) = sqlx::query_as::<_, (i64, String)>(
            "INSERT INTO agent_sessions (title, data) VALUES (?, ?) RETURNING id, updated_at",
        )
        .bind(title)
        .bind(serde_json::to_string(&data)?)
        .fetch_one(self.store.pool())
        .await
        .context("Could not create the local conversation")?;
        Ok(data.into_session(id, title.to_owned(), updated_at))
    }

    pub async fn list(&self) -> Result<Vec<SessionSummary>> {
        let rows = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT id, title, updated_at FROM agent_sessions ORDER BY updated_at DESC, id DESC",
        )
        .fetch_all(self.store.pool())
        .await
        .context("Could not list local conversations")?;
        Ok(rows
            .into_iter()
            .map(|(id, title, updated_at)| SessionSummary {
                id,
                title,
                updated_at,
            })
            .collect())
    }

    pub async fn get(&self, session_id: i64) -> Result<Session> {
        Ok(self.load(session_id).await?.into_session())
    }

    pub async fn set_agent(&self, session_id: i64, agent_id: &str) -> Result<Session> {
        ensure!(
            !agent_id.trim().is_empty(),
            "Choose an agent for the conversation"
        );
        self.update(session_id, |data| data.agent_id = Some(agent_id.to_owned()))
            .await
    }

    pub async fn set_context(
        &self,
        session_id: i64,
        selection: ContextSelection,
    ) -> Result<Session> {
        ensure!(selection.repo_id > 0, "Choose a valid repository");
        self.update(session_id, |data| {
            if let Some(existing) = data
                .context
                .iter_mut()
                .find(|item| item.repo_id == selection.repo_id)
            {
                *existing = selection.clone();
            } else {
                data.context.push(selection.clone());
            }
        })
        .await
    }

    pub async fn remove_context(&self, session_id: i64, repo_id: i64) -> Result<Session> {
        self.update(session_id, |data| {
            data.context
                .retain(|selection| selection.repo_id != repo_id)
        })
        .await
    }

    pub async fn repository_details(
        &self,
        repo_id: i64,
        session_id: Option<i64>,
    ) -> Result<Option<RepositoryDetails>> {
        if let Some(repo) = self.store.get_repository(repo_id).await? {
            let meta = &repo.metadata;
            let readme = self
                .store
                .get_readme_for(repo_id, &meta.full_name, meta.private)
                .await?
                .map(|readme| readme.content);
            return Ok(Some(RepositoryDetails::Library { repo, readme }));
        }
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        let Some(repo) = self.public_candidate(session_id, repo_id).await? else {
            return Ok(None);
        };
        let readme = self.candidate_readme(session_id, repo_id).await?;
        Ok(Some(RepositoryDetails::Candidate { repo, readme }))
    }

    pub async fn public_candidate(
        &self,
        session_id: i64,
        repo_id: i64,
    ) -> Result<Option<Repository>> {
        Ok(self
            .load(session_id)
            .await?
            .data
            .external_candidates()
            .find(|repo| repo.id == repo_id && !repo.private)
            .cloned())
    }

    pub async fn candidate_readme(&self, session_id: i64, repo_id: i64) -> Result<Option<String>> {
        self.read_candidate_readme(session_id, repo_id, false).await
    }

    async fn read_candidate_readme(
        &self,
        session_id: i64,
        repo_id: i64,
        verified_only: bool,
    ) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT content FROM candidate_readmes
             WHERE session_id = ? AND repository_id = ? AND (NOT ? OR identity_verified)",
        )
        .bind(session_id)
        .bind(repo_id)
        .bind(verified_only)
        .fetch_optional(self.store.pool())
        .await?)
    }

    /// Cache a README fetched anonymously for a public search candidate in this session.
    pub async fn cache_candidate_readme(
        &self,
        session_id: i64,
        repo_id: i64,
        readme: &str,
    ) -> Result<()> {
        ensure!(
            self.public_candidate(session_id, repo_id).await?.is_some(),
            "This repository is not a public search candidate in the conversation"
        );
        sqlx::query(
            "INSERT INTO candidate_readmes (session_id, repository_id, content, identity_verified)
             VALUES (?, ?, ?, TRUE) ON CONFLICT(session_id, repository_id) DO UPDATE SET
                 content = excluded.content, identity_verified = TRUE",
        )
        .bind(session_id)
        .bind(repo_id)
        .bind(readme)
        .execute(self.store.pool())
        .await?;
        Ok(())
    }

    /// Call once per explicit expansion. This operation never invokes the model.
    pub async fn expand(
        &self,
        session_id: i64,
        public_query: &str,
        search: &impl PublicSearch,
    ) -> Result<Session> {
        let public_query = public_query.trim();
        ensure!(
            !public_query.is_empty(),
            "Enter public terms to search GitHub"
        );
        ensure!(
            !self.load(session_id).await?.data.turns.is_empty(),
            "Send a message before expanding to GitHub search"
        );
        let mut seen = HashSet::new();
        let candidates: Vec<_> = search
            .search_public(public_query)
            .await
            .context("GitHub search failed. Retry the public search when GitHub is available")?
            .into_iter()
            .filter(|repo| !repo.private && seen.insert(repo.id))
            .map(|mut repo| {
                repo.note = None;
                repo.starred = false;
                repo
            })
            .collect();
        self.update(session_id, |data| {
            data.expansions.push(Expansion {
                sequence: data.next_sequence(),
                public_query: public_query.to_owned(),
                candidates: candidates.clone(),
            });
        })
        .await
    }

    pub async fn ask(
        &self,
        session_id: i64,
        message: &str,
        library: &impl Library,
        agent: &impl Agent,
    ) -> Result<Session> {
        ensure!(
            !message.trim().is_empty(),
            "Enter a message to send to the agent"
        );
        let mut saved = self.load(session_id).await?;
        let (request, candidates, input_scope) =
            self.assemble_request(&saved, message, library).await?;
        // Cross-process consent edits must finish before this check or retry after the reply is saved.
        let _handoff = self.store.lock_conversation(session_id).await?;
        let revision =
            sqlx::query_scalar::<_, i64>("SELECT revision FROM agent_sessions WHERE id = ?")
                .bind(session_id)
                .fetch_one(self.store.pool())
                .await
                .context("Could not verify the conversation context")?;
        ensure!(
            revision == saved.revision,
            "The conversation changed. Reload it and retry your message"
        );
        let response = agent.respond(&request).await?;
        let candidates = validate_response(&request, &response, candidates)?;
        saved.data.turns.push(Turn {
            sequence: saved.data.next_sequence(),
            message: request.message,
            response,
            candidates,
            input_scope: Some(input_scope),
        });
        ensure!(
            self.save(&mut saved).await?,
            "The conversation changed. Reload it and retry your message"
        );
        Ok(saved.into_session())
    }

    async fn update(&self, session_id: i64, change: impl Fn(&mut SessionData)) -> Result<Session> {
        let _edit = self.store.lock_conversation(session_id).await?;
        let mut saved = self.load(session_id).await?;
        change(&mut saved.data);
        ensure!(
            self.save(&mut saved).await?,
            "The conversation changed. Reload it and retry the change"
        );
        Ok(saved.into_session())
    }

    async fn load(&self, session_id: i64) -> Result<SavedSession> {
        let (title, updated_at, data, revision) =
            sqlx::query_as::<_, (String, String, String, i64)>(
                "SELECT title, updated_at, data, revision FROM agent_sessions WHERE id = ?",
            )
            .bind(session_id)
            .fetch_optional(self.store.pool())
            .await
            .context("Could not read the local conversation")?
            .with_context(|| format!("Conversation {session_id} was not found"))?;
        let data = serde_json::from_str(&data).context("Could not read saved conversation data")?;
        Ok(SavedSession {
            id: session_id,
            title,
            updated_at,
            revision,
            data,
        })
    }

    async fn save(&self, session: &mut SavedSession) -> Result<bool> {
        let updated = sqlx::query_scalar::<_, String>(
            "UPDATE agent_sessions
             SET data = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), revision = revision + 1
             WHERE id = ? AND revision = ? RETURNING updated_at",
        )
        .bind(serde_json::to_string(&session.data)?)
        .bind(session.id)
        .bind(session.revision)
        .fetch_optional(self.store.pool())
        .await
        .context("Could not save the local conversation")?;
        if let Some(updated_at) = updated {
            session.updated_at = updated_at;
            session.revision += 1;
            return Ok(true);
        }
        Ok(false)
    }
}

fn validate_response(
    request: &AgentRequest,
    response: &AgentResponse,
    candidates: Vec<Repository>,
) -> Result<Vec<Repository>> {
    ensure!(
        !response.text.trim().is_empty(),
        "The agent returned an empty reply. Retry your message"
    );
    let supplied: HashMap<_, _> = request
        .repositories
        .iter()
        .map(|repo| (repo.id, repo))
        .collect();
    let mut snapshots: HashMap<_, _> = candidates.into_iter().map(|repo| (repo.id, repo)).collect();
    let mut recommended = HashSet::new();
    let mut ordered = Vec::new();
    for recommendation in &response.recommendations {
        ensure!(
            recommended.insert(recommendation.repo_id),
            "The agent repeated a repository in its reply. Retry your message"
        );
        let repo = supplied.get(&recommendation.repo_id).context(
            "The agent cited a repository outside the supplied context. Retry your message",
        )?;
        ensure!(
            !recommendation.fit.trim().is_empty()
                && !recommendation.evidence.is_empty()
                && recommendation
                    .evidence
                    .iter()
                    .all(|evidence| repo.contains(evidence)),
            "The agent returned a recommendation without verifiable evidence. Retry your message"
        );
        ordered.push(
            snapshots
                .remove(&recommendation.repo_id)
                .context("Could not resolve a recommended repository")?,
        );
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    async fn test_conversations() -> Result<(tempfile::TempDir, Conversations)> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(&dir.path().join("library.sqlite")).await?;
        Ok((dir, Conversations::new(&store)))
    }

    #[tokio::test]
    async fn migrates_legacy_conversations_and_keeps_unverified_readmes_local() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("library.sqlite");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let old_pool = sqlx::SqlitePool::connect_with(options).await?;
        let old_schema = dir.path().join("migrations");
        std::fs::create_dir(&old_schema)?;
        std::fs::write(
            old_schema.join("0001_library.sql"),
            include_str!("../migrations/0001_library.sql"),
        )?;
        sqlx::migrate::Migrator::new(old_schema)
            .await?
            .run(&old_pool)
            .await?;
        sqlx::raw_sql("CREATE TABLE agent_sessions (
            id INTEGER PRIMARY KEY, title TEXT NOT NULL, updated_at TEXT NOT NULL,
            revision INTEGER NOT NULL DEFAULT 0, data TEXT NOT NULL
        );
        INSERT INTO repositories (id, full_name, private, starred, html_url)
            VALUES (1, 'fixture/private', TRUE, TRUE, 'https://github.com/fixture/private');
        INSERT INTO repository_readmes VALUES (1, 'legacy-private-sentinel', 'https://github.com/fixture/private');")
            .execute(&old_pool).await?;
        sqlx::query("INSERT INTO agent_sessions VALUES (7, 'Saved comparison', '2026-01-01', 5, ?)")
            .bind(serde_json::json!({
                "agent_id": "pi", "context": [
                    {"repo_id": 1, "include_private": true, "include_notes": false},
                    {"repo_id": 9, "include_private": false, "include_notes": false}
                ], "turns": [{
                    "sequence": 1, "message": "Find candidates", "candidates": [],
                    "response": {"text": "legacy-reply-sentinel", "recommendations": [], "gaps": []}
                }], "expansions": [{
                    "sequence": 2, "public_query": "find search", "candidates": [repository(9, "fixture/external")]
                }], "external_readmes": {"9": "legacy-external-sentinel"}
            }).to_string())
            .execute(&old_pool).await?;
        old_pool.close().await;

        let store = Store::open(&path).await?;
        let conversations = Conversations::new(&store);
        let loaded = conversations.get(7).await?;
        assert_eq!(loaded.title, "Saved comparison");
        assert_eq!(loaded.turns[0].response.text, "legacy-reply-sentinel");
        assert_eq!(
            conversations.candidate_readme(7, 9).await?.as_deref(),
            Some("legacy-external-sentinel")
        );
        assert_eq!(
            store.get_readme(1).await?.unwrap().content,
            "legacy-private-sentinel"
        );
        let agent = RecordingAgent {
            requests: Mutex::default(),
            answer: Some(AgentResponse {
                text: "Compare next".into(),
                recommendations: vec![],
                gaps: vec![],
            }),
        };
        let library = crate::runtime::AgentLibrary::load(&store).await?;
        let compared = conversations
            .ask(7, "Continue the comparison", &library, &agent)
            .await?;
        assert_eq!(compared.turns[1].sequence, 3);
        let payload = serde_json::to_string(&agent.requests.lock().unwrap()[0])?;
        assert!(payload.contains("fixture/private"));
        assert!(payload.contains("fixture/external"));
        assert!(!payload.contains("legacy-"));
        Ok(())
    }

    #[tokio::test]
    async fn creates_and_lists_separate_conversations() -> anyhow::Result<()> {
        let (_dir, conversations) = test_conversations().await?;
        let first = conversations.create("中文离线搜索").await?;
        let second = conversations.create("Image editor").await?;

        assert_ne!(first.id, second.id);
        assert_eq!(conversations.get(first.id).await?, first);
        assert_eq!(conversations.list().await?[0].title, "Image editor");
        assert!(first.turns.is_empty());
        assert!(first.context.is_empty());
        Ok(())
    }

    struct TestLibrary(Vec<Repository>);

    impl Library for TestLibrary {
        async fn repositories(&self) -> Result<Vec<Repository>> {
            Ok(self.0.clone())
        }

        async fn cached_readme(&self, id: i64) -> Result<Option<String>> {
            Ok((id == 1).then(|| "中文分词 for offline desktop search".to_owned()))
        }
    }

    fn repository(id: i64, full_name: &str) -> Repository {
        Repository {
            id,
            full_name: full_name.into(),
            description: Some("An offline search library".into()),
            language: Some("Rust".into()),
            topics: vec!["search".into()],
            html_url: format!("https://github.com/{full_name}"),
            private: false,
            starred: true,
            note: None,
        }
    }

    #[tokio::test]
    async fn ranks_the_full_library_and_bounds_the_serialized_request() -> Result<()> {
        struct LargeLibrary;
        impl Library for LargeLibrary {
            async fn repositories(&self) -> Result<Vec<Repository>> {
                Ok((1..=750)
                    .map(|id| {
                        let mut repo = repository(id, &format!("example/repo-{id}"));
                        let prefix = if id == 750 {
                            "中文离线搜索"
                        } else {
                            "Other project"
                        };
                        repo.description = Some(format!("{prefix} {}", "\"分\n".repeat(100)));
                        repo.note = Some("分".repeat(10_000));
                        repo
                    })
                    .collect())
            }

            async fn cached_readme(&self, id: i64) -> Result<Option<String>> {
                Ok(Some(format!("README for {id}\n{}", "分".repeat(100_000))))
            }
        }

        let (_dir, conversations) = test_conversations().await?;
        let session = conversations.create("A larger library").await?;
        conversations
            .set_context(
                session.id,
                ContextSelection {
                    repo_id: 30,
                    include_private: false,
                    include_notes: true,
                },
            )
            .await?;
        let agent = RecordingAgent {
            requests: Mutex::default(),
            answer: Some(AgentResponse {
                text: "The library has options.".into(),
                recommendations: vec![],
                gaps: vec![],
            }),
        };
        for _ in 0..11 {
            conversations
                .ask(
                    session.id,
                    &"Long prior prompt ".repeat(100),
                    &LargeLibrary,
                    &agent,
                )
                .await?;
        }
        conversations
            .ask(
                session.id,
                "我想为桌面应用增加中文离线搜索",
                &LargeLibrary,
                &agent,
            )
            .await?;

        let requests = agent.requests.lock().unwrap();
        let request = requests.last().unwrap();
        assert!(serde_json::to_vec(request)?.len() <= 32 * 1024);
        let repositories = &request.repositories;
        assert_eq!(repositories[0].id, 30);
        assert_eq!(repositories[1].id, 750);
        let selected = &repositories[0];
        assert!(selected
            .readme
            .as_deref()
            .unwrap()
            .starts_with("README for 30"));
        assert!(selected.note.is_some());
        assert!(repositories[1].note.is_none());
        Ok(())
    }

    struct RecordingAgent {
        requests: Mutex<Vec<AgentRequest>>,
        answer: Option<AgentResponse>,
    }

    impl Agent for RecordingAgent {
        async fn respond(&self, request: &AgentRequest) -> Result<AgentResponse> {
            self.requests.lock().unwrap().push(request.clone());
            self.answer
                .clone()
                .context("Check the test agent configuration")
        }
    }

    #[tokio::test]
    async fn recommends_a_star_with_checkable_evidence_and_records_gaps() -> Result<()> {
        let (_dir, conversations) = test_conversations().await?;
        let session = conversations.create("中文离线搜索").await?;
        let library = TestLibrary(vec![repository(1, "example/segmenter")]);
        let agent = RecordingAgent {
            requests: Mutex::default(),
            answer: Some(AgentResponse {
                text: "Consider this segmenter.".into(),
                recommendations: vec![Recommendation {
                    repo_id: 1,
                    fit: "Segments Chinese queries offline".into(),
                    evidence: vec![Evidence {
                        source: EvidenceSource::Readme,
                        excerpt: "中文分词 for offline desktop search".into(),
                    }],
                    gaps: vec!["Index size is undocumented".into()],
                }],
                gaps: vec![],
            }),
        };

        conversations
            .ask(
                session.id,
                "我想为桌面应用增加中文离线搜索",
                &library,
                &agent,
            )
            .await?;
        let saved = conversations.get(session.id).await?;
        let turn = &saved.turns[0];
        assert_eq!(turn.message, "我想为桌面应用增加中文离线搜索");
        assert_eq!(
            turn.candidates[0].html_url,
            "https://github.com/example/segmenter"
        );
        assert!(turn.candidates[0].starred);
        let recommendation = &turn.response.recommendations[0];
        assert_eq!(
            recommendation.evidence[0].excerpt,
            "中文分词 for offline desktop search"
        );
        assert_eq!(recommendation.gaps, ["Index size is undocumented"]);
        let requests = agent.requests.lock().unwrap();
        assert_eq!(
            requests[0].repositories[0].readme.as_deref(),
            Some("中文分词 for offline desktop search")
        );
        Ok(())
    }

    #[tokio::test]
    async fn scopes_private_repos_and_notes_to_current_explicit_grants() -> Result<()> {
        let (_dir, conversations) = test_conversations().await?;
        let session = conversations
            .create_with_agent("Scoped", Some("pi"))
            .await?;
        let separate = conversations.create("Separate").await?;
        let mut private = repository(1, "secret/segmenter");
        private.private = true;
        private.note = Some("confidential private note".into());
        let mut public = repository(2, "example/search");
        public.note = Some("confidential public note".into());
        let library = TestLibrary(vec![private, public]);
        #[derive(Default)]
        struct ScopeAgent(Mutex<Vec<AgentRequest>>);
        impl Agent for ScopeAgent {
            async fn respond(&self, request: &AgentRequest) -> Result<AgentResponse> {
                self.0.lock().unwrap().push(request.clone());
                Ok(AgentResponse {
                    text: request
                        .repositories
                        .iter()
                        .find_map(|repo| repo.note.clone())
                        .unwrap_or_else(|| "No note was supplied".into()),
                    recommendations: vec![],
                    gaps: vec!["No documented comparison".into()],
                })
            }
        }
        let agent = ScopeAgent::default();

        conversations
            .ask(session.id, "Find options", &library, &agent)
            .await?;
        conversations
            .set_context(
                session.id,
                ContextSelection {
                    repo_id: 1,
                    include_private: false,
                    include_notes: true,
                },
            )
            .await?;
        conversations
            .set_context(
                session.id,
                ContextSelection {
                    repo_id: 2,
                    include_private: false,
                    include_notes: true,
                },
            )
            .await?;
        conversations
            .ask(session.id, "Which notes apply?", &library, &agent)
            .await?;
        conversations
            .set_context(
                session.id,
                ContextSelection {
                    repo_id: 1,
                    include_private: true,
                    include_notes: false,
                },
            )
            .await?;
        conversations
            .ask(session.id, "Compare private docs", &library, &agent)
            .await?;
        conversations
            .set_context(
                session.id,
                ContextSelection {
                    repo_id: 1,
                    include_private: true,
                    include_notes: true,
                },
            )
            .await?;
        conversations
            .ask(session.id, "Compare both", &library, &agent)
            .await?;
        conversations
            .ask(separate.id, "Find options", &library, &agent)
            .await?;
        conversations.remove_context(session.id, 1).await?;
        conversations.remove_context(session.id, 2).await?;
        let latest = conversations
            .ask(session.id, "Compare again", &library, &agent)
            .await?;

        let requests = agent.0.lock().unwrap();
        for index in [0, 4, 5] {
            let payload = serde_json::to_string(&requests[index])?;
            assert!(!payload.contains("confidential"));
            assert!(!payload.contains("secret/segmenter"));
            assert!(!payload.contains("中文分词"));
        }
        let payload = serde_json::to_string(&requests[1])?;
        assert!(payload.contains("confidential public note"));
        assert!(!payload.contains("secret/segmenter"));
        let payload = serde_json::to_string(&requests[2])?;
        assert!(payload.contains("secret/segmenter"));
        assert!(payload.contains("中文分词"));
        assert!(!payload.contains("confidential private note"));
        assert!(serde_json::to_string(&requests[3])?.contains("confidential private note"));
        assert!(latest.context.is_empty());
        assert_eq!(latest.agent_id.as_deref(), Some("pi"));
        assert_eq!(latest.turns.len(), 5);
        Ok(())
    }

    struct RecordingSearch(Mutex<Vec<String>>, Vec<Repository>);

    impl PublicSearch for RecordingSearch {
        async fn search_public(&self, query: &str) -> Result<Vec<Repository>> {
            self.0.lock().unwrap().push(query.to_owned());
            Ok(self.1.clone())
        }
    }

    #[tokio::test]
    async fn searches_once_with_public_terms_and_reuses_results_after_reopening() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("starchive.sqlite");
        let store = Store::open(&path).await?;
        let conversations = Conversations::new(&store);
        let session = conversations
            .create_with_agent("Search", Some("pi"))
            .await?;
        let library = TestLibrary(vec![]);
        let agent = RecordingAgent {
            requests: Mutex::default(),
            answer: Some(AgentResponse {
                text: "No matches yet.".into(),
                recommendations: vec![],
                gaps: vec![],
            }),
        };
        conversations
            .ask(
                session.id,
                "Find tools for secret/desktop",
                &library,
                &agent,
            )
            .await?;
        let mut public = repository(2, "example/fts");
        public.starred = false;
        public.note = Some("private search result note".into());
        let mut private = repository(3, "secret/hidden");
        private.private = true;
        let search = RecordingSearch(Mutex::default(), vec![public, private]);

        assert!(conversations
            .expand(session.id, "  ", &search)
            .await
            .is_err());
        conversations
            .expand(session.id, "  Rust full-text search  ", &search)
            .await?;
        assert_eq!(*search.0.lock().unwrap(), ["Rust full-text search"]);
        drop(conversations);
        store.pool().close().await;
        let conversations = Conversations::new(&Store::open(&path).await?);
        let loaded = conversations.get(session.id).await?;
        assert_eq!(loaded.agent_id.as_deref(), Some("pi"));
        assert_eq!(loaded.turns[0].message, "Find tools for secret/desktop");
        assert_eq!(loaded.expansions[0].public_query, "Rust full-text search");
        assert_eq!(loaded.external_candidates.len(), 1);
        assert_eq!(loaded.external_candidates[0].full_name, "example/fts");
        assert!(!loaded.external_candidates[0].starred);
        assert!(loaded.external_candidates[0].note.is_none());

        let agent = RecordingAgent {
            requests: Mutex::default(),
            answer: Some(AgentResponse {
                text: "Compare this candidate.".into(),
                recommendations: vec![Recommendation {
                    repo_id: 2,
                    fit: "Full-text search".into(),
                    evidence: vec![Evidence {
                        source: EvidenceSource::Description,
                        excerpt: "offline search library".into(),
                    }],
                    gaps: vec![],
                }],
                gaps: vec![],
            }),
        };
        let compared = conversations
            .ask(session.id, "Compare the public result", &library, &agent)
            .await?;
        assert_eq!(compared.turns[1].candidates[0].id, 2);
        assert_eq!(compared.turns[1].sequence, 3);
        assert_eq!(*search.0.lock().unwrap(), ["Rust full-text search"]);
        conversations
            .expand(session.id, "embedded search", &search)
            .await?;
        assert_eq!(
            *search.0.lock().unwrap(),
            ["Rust full-text search", "embedded search"]
        );
        Ok(())
    }

    #[tokio::test]
    async fn verifies_evidence_and_reuses_prior_recommendations_in_order() -> Result<()> {
        let (_dir, conversations) = test_conversations().await?;
        let session = conversations
            .create("Compare offline search libraries")
            .await?;
        let library = TestLibrary(vec![
            repository(1, "example/first"),
            repository(2, "example/second"),
        ]);
        let mut answer = AgentResponse {
            text: "Two options, in order of fit.".into(),
            recommendations: [2, 1]
                .into_iter()
                .map(|repo_id| Recommendation {
                    repo_id,
                    fit: "Supports offline search".into(),
                    evidence: vec![Evidence {
                        source: EvidenceSource::Description,
                        excerpt: "offline search library".into(),
                    }],
                    gaps: vec![],
                })
                .collect(),
            gaps: vec![],
        };
        let first = RecordingAgent {
            requests: Mutex::default(),
            answer: Some(answer.clone()),
        };
        let saved = conversations
            .ask(session.id, "Find two projects", &library, &first)
            .await?;

        answer.recommendations[0].evidence[0].source = EvidenceSource::Note;
        let invalid = RecordingAgent {
            requests: Mutex::default(),
            answer: Some(answer),
        };
        let error = conversations
            .ask(
                session.id,
                "Tell me more about the first one",
                &library,
                &invalid,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("verifiable evidence"));
        assert_eq!(conversations.get(session.id).await?, saved);
        assert_eq!(
            saved.turns[0]
                .candidates
                .iter()
                .map(|repo| repo.id)
                .collect::<Vec<_>>(),
            [2, 1]
        );
        let requests = invalid.requests.lock().unwrap();
        let payload = serde_json::to_string(&requests[0])?;
        assert!(payload.contains("Two options, in order of fit."));
        assert!(payload.contains("Supports offline search"));
        let second = payload.find("\"repo_id\":2").unwrap();
        assert!(second < payload.find("\"repo_id\":1").unwrap());
        Ok(())
    }

    #[tokio::test]
    async fn an_in_flight_reply_serializes_grants_but_allows_candidate_readme_caching() -> Result<()>
    {
        struct WaitingAgent {
            started: tokio::sync::Notify,
            finish: tokio::sync::Notify,
        }
        impl Agent for WaitingAgent {
            async fn respond(&self, _: &AgentRequest) -> Result<AgentResponse> {
                self.started.notify_one();
                self.finish.notified().await;
                Ok(AgentResponse {
                    text: "Compared".into(),
                    recommendations: vec![],
                    gaps: vec![],
                })
            }
        }

        let dir = tempfile::tempdir()?;
        let path = dir.path().join("library.sqlite");
        let store = crate::Store::open(&path).await?;
        let second_store = crate::Store::open(&path).await?;
        let conversations = Conversations::new(&store);
        let other = Conversations::new(&second_store);
        let session = conversations.create("Review").await?;
        let library = TestLibrary(vec![]);
        let initial = RecordingAgent {
            requests: Mutex::default(),
            answer: Some(AgentResponse {
                text: "Initial".into(),
                recommendations: vec![],
                gaps: vec![],
            }),
        };
        conversations
            .ask(session.id, "Find options", &library, &initial)
            .await?;
        let search = RecordingSearch(Mutex::default(), vec![repository(2, "example/fts")]);
        conversations
            .expand(session.id, "offline search", &search)
            .await?;
        conversations
            .set_context(
                session.id,
                ContextSelection {
                    repo_id: 2,
                    include_private: false,
                    include_notes: false,
                },
            )
            .await?;
        let agent = WaitingAgent {
            started: Default::default(),
            finish: Default::default(),
        };
        let (reply, (revoke, cache)) = tokio::join!(
            conversations.ask(session.id, "Compare the candidate", &library, &agent),
            async {
                agent.started.notified().await;
                let revoke = other.remove_context(session.id, 2).await;
                let cache = other
                    .cache_candidate_readme(session.id, 2, "Cached during reply")
                    .await;
                agent.finish.notify_one();
                (revoke, cache)
            }
        );
        assert!(revoke.unwrap_err().to_string().contains("busy"));
        cache?;
        assert_eq!(reply?.turns[1].response.text, "Compared");
        assert!(other
            .remove_context(session.id, 2)
            .await?
            .context
            .is_empty());
        assert_eq!(
            other.candidate_readme(session.id, 2).await?.as_deref(),
            Some("Cached during reply")
        );
        Ok(())
    }

    #[tokio::test]
    async fn external_readme_enters_context_only_when_the_candidate_is_selected() -> Result<()> {
        let (_dir, conversations) = test_conversations().await?;
        let session = conversations.create("Find search projects").await?;
        let library = TestLibrary(vec![]);
        let agent = RecordingAgent {
            requests: Mutex::default(),
            answer: Some(AgentResponse {
                text: "Search options".into(),
                recommendations: vec![],
                gaps: vec![],
            }),
        };
        conversations
            .ask(session.id, "Find offline libraries", &library, &agent)
            .await?;
        let search = RecordingSearch(Mutex::default(), vec![repository(2, "example/fts")]);
        conversations
            .expand(session.id, "offline search", &search)
            .await?;
        assert!(conversations
            .cache_candidate_readme(session.id, 3, "Unrelated")
            .await
            .is_err());
        conversations
            .cache_candidate_readme(session.id, 2, "Cached candidate README")
            .await?;
        conversations
            .ask(session.id, "What did search return?", &library, &agent)
            .await?;
        conversations
            .set_context(
                session.id,
                ContextSelection {
                    repo_id: 2,
                    include_private: false,
                    include_notes: false,
                },
            )
            .await?;
        conversations
            .ask(
                session.id,
                "Compare the selected candidate",
                &library,
                &agent,
            )
            .await?;

        let requests = agent.requests.lock().unwrap();
        assert!(requests[1].repositories[0].readme.is_none());
        assert_eq!(
            requests[2].repositories[0].readme.as_deref(),
            Some("Cached candidate README")
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pi_subprocess_uses_augmented_path_without_github_tokens() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir()?;
        let path = temp.path().join("pi");
        let helper = temp.path().join("starchive-test-pi-path-helper");
        std::fs::write(&helper, "#!/bin/sh\n")?;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(
            &path,
            r#"#!/bin/sh
[ "$1 $2 $3" = '--print --mode text' ] || exit 7
cat >/dev/null
command -v starchive-test-pi-path-helper >/dev/null || exit 5
[ "${GH_TOKEN+x}${GITHUB_TOKEN+x}${STARCHIVE_GITHUB_TOKEN+x}" = "" ] || exit 6
printf '%s\n' '{"text":"The credentials are absent","recommendations":[],"gaps":[]}'
"#,
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        // Set the sentinel only in a child test process, keeping the test runner's environment safe.
        if std::env::var_os("STARCHIVE_PI_ENV_TEST").is_none() {
            let output = std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "agent::tests::pi_subprocess_uses_augmented_path_without_github_tokens",
                ])
                .env("PATH", "/usr/bin:/bin")
                .env("STARCHIVE_PI_ENV_TEST", "1")
                .env("GH_TOKEN", "fake-gh-token-sentinel")
                .env("GITHUB_TOKEN", "fake-github-token-sentinel")
                .env("STARCHIVE_GITHUB_TOKEN", "fake-legacy-token-sentinel")
                .output()?;
            ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return Ok(());
        }
        let request = AgentRequest {
            message: "Check credentials".into(),
            history: vec![],
            selected_repositories: vec![],
            repositories: vec![],
        };
        assert_eq!(
            PiAgent::new(path).respond(&request).await?.text,
            "The credentials are absent"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn streamed_preview_only_persists_after_validating_the_authoritative_reply() -> Result<()>
    {
        use serde_json::json;
        use std::{os::unix::fs::PermissionsExt, sync::Arc};

        let (dir, conversations) = test_conversations().await?;
        let path = dir.path().join("fixture-pi");
        let session = conversations.create("Local pi").await?;
        let library = TestLibrary(vec![repository(1, "fixture/search")]);
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let captured = chunks.clone();
        let agent =
            PiAgent::new(&path).with_text_delta(move |text| captured.lock().unwrap().push(text));
        let update = json!({"type": "message_update", "assistantMessageEvent": {
            "type": "text_delta", "contentIndex": 0, "delta": r#"{"text":"Provisional prose""#
        }});
        let answer = json!({"text": "Final answer", "recommendations": [], "gaps": []});
        let mut unverified = answer.clone();
        unverified["recommendations"] = json!([{"repo_id":1,"fit":"Offline","evidence":[{
            "source":"description","excerpt":"PRIVATE_MADE_UP_EXCERPT"
        }],"gaps":[]}]);
        for (reason, text, error) in [
            ("error", answer.to_string(), Some("did not complete")),
            ("aborted", answer.to_string(), Some("did not complete")),
            ("length", answer.to_string(), Some("did not complete")),
            (
                "stop",
                "PRIVATE_INVALID_RESPONSE".into(),
                Some("invalid recommendation response"),
            ),
            (
                "stop",
                unverified.to_string(),
                Some("without verifiable evidence"),
            ),
            ("stop", answer.to_string(), None),
        ] {
            let end = json!({"type": "message_end", "message": {
                "role": "assistant", "stopReason": reason, "errorMessage": "PRIVATE_PROVIDER_ERROR",
                "content": [{"type": "text", "text": text}]
            }});
            std::fs::write(
                &path,
                format!("#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{update}' '{end}'\n"),
            )?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            let result = conversations
                .ask(session.id, "Compare", &library, &agent)
                .await;
            if let Some(expected) = error {
                let message = format!("{:#}", result.unwrap_err());
                assert!(message.contains(expected), "{message}");
                assert!(!message.contains("PRIVATE_"));
                assert_eq!(conversations.get(session.id).await?, session);
            } else {
                let saved = result?;
                assert_eq!(saved.turns.len(), 1);
                assert_eq!(saved.turns[0].response.text, "Final answer");
                assert_eq!(conversations.get(session.id).await?, saved);
            }
        }
        assert_eq!(*chunks.lock().unwrap(), ["Provisional prose"; 6]);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pi_process_failure_preserves_history_and_a_configured_executable_can_retry(
    ) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir()?;
        let path = temp.path().join("configured pi with spaces");
        let failure = PiAgent::new(&path);
        let (_dir, conversations) = test_conversations().await?;
        let session = conversations.create("Local pi").await?;
        let library = TestLibrary(vec![]);
        let initial = RecordingAgent {
            requests: Mutex::default(),
            answer: Some(AgentResponse {
                text: "Start with an offline search engine.".into(),
                recommendations: vec![],
                gaps: vec![],
            }),
        };
        let saved = conversations
            .ask(session.id, "Outline the project", &library, &initial)
            .await?;
        let error = conversations
            .ask(session.id, "Can it run offline?", &library, &failure)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Install pi"));
        assert_eq!(conversations.get(session.id).await?, saved);

        std::fs::write(
            &path,
            r#"#!/bin/sh
session=''; tools=''; extensions=''; mcp=''; context=''
for arg do
  case "$arg" in
    --no-session) session=1 ;;
    --no-tools) tools=1 ;;
    --no-extensions) extensions=1 ;;
    --no-mcp) mcp=1 ;;
    --no-context-files) context=1 ;;
    *'Can it run offline?'*) exit 2 ;;
  esac
done
[ "$session$tools$extensions$mcp$context" = 11111 ] || exit 3
cat | grep -Fq 'Can it run offline?' || exit 4
printf '%s\n' '```json' '{"text":"Offline support is undocumented.","recommendations":[],"gaps":["Offline support"]}' '```'
"#,
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        let retry = conversations
            .ask(
                session.id,
                "Can it run offline?",
                &library,
                &PiAgent::new(&path),
            )
            .await?;
        assert_eq!(retry.turns[0], saved.turns[0]);
        assert_eq!(retry.turns[1].response.gaps, ["Offline support"]);
        Ok(())
    }
}
