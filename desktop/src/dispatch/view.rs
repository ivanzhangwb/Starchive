use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use starchive_core::{agent, Repository, Store, SyncStatus};

#[derive(Serialize)]
pub(super) struct Repo<'a> {
    id: String,
    full_name: &'a str,
    description: Option<&'a str>,
    language: Option<&'a str>,
    url: &'a str,
    starred: bool,
    private: bool,
}

impl<'a> From<&'a Repository> for Repo<'a> {
    fn from(repo: &'a Repository) -> Self {
        let meta = &repo.metadata;
        Self {
            id: meta.id.to_string(),
            full_name: &meta.full_name,
            description: meta.description.as_deref(),
            language: meta.language.as_deref(),
            url: &meta.html_url,
            starred: repo.starred,
            private: meta.private,
        }
    }
}

pub(super) fn candidate_repo<'a>(
    repo: &'a agent::Repository,
    current: Option<&'a Repository>,
) -> Repo<'a> {
    if let Some(current) = current {
        return current.into();
    }
    Repo {
        id: repo.id.to_string(),
        full_name: &repo.full_name,
        description: repo.description.as_deref(),
        language: repo.language.as_deref(),
        url: &repo.html_url,
        starred: false,
        private: repo.private,
    }
}

pub fn library_repo(repo: &Repository) -> impl Serialize + '_ {
    #[derive(Serialize)]
    struct LibraryRepo<'a> {
        #[serde(flatten)]
        repo: Repo<'a>,
        note: &'a str,
        tags: &'a [String],
    }

    LibraryRepo {
        repo: repo.into(),
        note: &repo.note,
        tags: &repo.tags,
    }
}

pub fn sync_status(status: SyncStatus) -> Value {
    let last_result = if let Some(message) = status.last_error {
        json!({"state": "error", "message": message})
    } else {
        json!(status
            .last_result
            .map(|result| json!({"state": "success", "starred_count": result.starred})))
    };
    json!({"running": status.in_progress, "last_success_at": status.last_successful_at, "last_result": last_result})
}

pub fn session_summary(session: &agent::SessionSummary) -> Value {
    json!({"id": session.id.to_string(), "title": session.title, "updated_at": session.updated_at})
}

pub async fn session(store: &Store, session: agent::Session) -> Result<Value> {
    let ids: HashSet<_> = session
        .context
        .iter()
        .map(|item| item.repo_id)
        .chain(
            session
                .turns
                .iter()
                .flat_map(|turn| turn.candidates.iter().map(|repo| repo.id)),
        )
        .chain(
            session
                .expansions
                .iter()
                .flat_map(|expansion| expansion.candidates.iter().map(|repo| repo.id)),
        )
        .collect();
    let ids: Vec<_> = ids.into_iter().collect();
    let library: HashMap<_, _> = store
        .repositories_by_id(&ids)
        .await?
        .into_iter()
        .map(|repo| (repo.metadata.id, repo))
        .collect();
    let external: HashMap<_, _> = session
        .external_candidates
        .iter()
        .map(|repo| (repo.id, repo))
        .collect();
    let context: Vec<_> = session.context.iter().filter_map(|selection| {
        let repo = if let Some(repo) = library.get(&selection.repo_id) {
            Repo::from(repo)
        } else {
            candidate_repo(external.get(&selection.repo_id)?, None)
        };
        Some(json!({"repo": repo, "include_private": selection.include_private, "include_notes": selection.include_notes}))
    }).collect();

    // These saved turns are rendered on this device. Conversations::ask builds its own model input.
    let mut messages = Vec::new();
    for turn in &session.turns {
        messages.push((
            turn.sequence,
            json!({
                "id": format!("{}-user", turn.sequence), "role": "user", "text": turn.message,
                "candidates": [], "gaps": [],
            }),
        ));
        let candidates: Vec<_> = turn
            .response
            .recommendations
            .iter()
            .zip(&turn.candidates)
            .map(|(recommendation, repo)| {
                json!({
                    "repo": candidate_repo(repo, library.get(&repo.id)), "fit": recommendation.fit,
                    "evidence": recommendation.evidence, "gaps": recommendation.gaps,
                })
            })
            .collect();
        messages.push((turn.sequence, json!({
            "id": format!("{}-assistant", turn.sequence), "role": "assistant", "text": turn.response.text,
            "candidates": candidates, "gaps": turn.response.gaps,
        })));
    }
    for expansion in &session.expansions {
        let candidates: Vec<_> = expansion.candidates.iter().map(|repo| json!({
            "repo": candidate_repo(repo, library.get(&repo.id)), "fit": repo.description.as_deref().unwrap_or_default(),
            "evidence": [], "gaps": [],
        })).collect();
        messages.push((expansion.sequence, json!({
            "id": format!("{}-search", expansion.sequence), "role": "assistant",
            "text": format!("GitHub 搜索「{}」返回 {} 个候选。", expansion.public_query, candidates.len()),
            "candidates": candidates, "gaps": [],
        })));
    }
    messages.sort_by_key(|(sequence, _)| *sequence);
    Ok(json!({
        "id": session.id.to_string(), "title": session.title, "updated_at": session.updated_at,
        "agent_id": session.agent_id, "context": context,
        "messages": messages.into_iter().map(|(_, message)| message).collect::<Vec<_>>(),
        "can_expand": !session.turns.is_empty(),
    }))
}
