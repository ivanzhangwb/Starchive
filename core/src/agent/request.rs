use super::*;
use std::collections::BTreeMap;

const REQUEST_BYTES: usize = 32 * 1024;
const MESSAGE_BYTES: usize = 8 * 1024;
const HISTORY_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, Serialize)]
pub(super) struct AgentHistory {
    message: String,
    recommended_ids: Vec<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply: Option<AgentResponse>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct InputScope(BTreeMap<i64, SourceGrant>);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct SourceGrant {
    private: bool,
    note: bool,
    external_readme: bool,
}

impl InputScope {
    fn include(&mut self, id: i64, grant: SourceGrant) {
        let combined = self.0.entry(id).or_default();
        combined.private |= grant.private;
        combined.note |= grant.note;
        combined.external_readme |= grant.external_readme;
    }

    fn allowed(
        &self,
        eligible: &HashSet<i64>,
        selections: &HashMap<i64, &ContextSelection>,
    ) -> bool {
        self.0.iter().all(|(id, grant)| {
            let selection = selections.get(id);
            eligible.contains(id)
                && (!grant.private || selection.is_some_and(|item| item.include_private))
                && (!grant.note || selection.is_some_and(|item| item.include_notes))
                && (!grant.external_readme || selection.is_some())
        })
    }
}

impl Conversations {
    pub(super) async fn assemble_request(
        &self,
        saved: &SavedSession,
        message: &str,
        library: &impl Library,
    ) -> Result<(AgentRequest, Vec<Repository>, InputScope)> {
        ensure!(
            json_bytes(&message)? <= MESSAGE_BYTES,
            "The message is too long. Shorten it or split it into several turns"
        );
        let mut candidates = library
            .repositories()
            .await
            .context("Could not read the library")?;
        let local_ids: HashSet<_> = candidates.iter().map(|repo| repo.id).collect();
        let selections: HashMap<_, _> = saved
            .data
            .context
            .iter()
            .map(|item| (item.repo_id, item))
            .collect();
        candidates.retain(|repo| {
            !repo.private
                || selections
                    .get(&repo.id)
                    .is_some_and(|item| item.include_private)
        });
        // Current local visibility takes precedence over a public search snapshot.
        candidates.extend(
            saved
                .data
                .external_candidates()
                .filter(|repo| !local_ids.contains(&repo.id))
                .cloned(),
        );
        let eligible = candidates.iter().map(|repo| repo.id).collect();
        let previous_candidates: HashMap<_, _> = saved
            .data
            .turns
            .iter()
            .filter(|turn| {
                turn.input_scope
                    .as_ref()
                    .is_none_or(|scope| scope.allowed(&eligible, &selections))
            })
            .flat_map(|turn| turn.candidates.iter().map(|repo| (repo.id, turn.sequence)))
            .collect();
        let turns = &saved.data.turns;
        let recent_sequence = turns
            .get(turns.len().saturating_sub(3))
            .map(|turn| turn.sequence);
        candidates.retain(|repo| {
            !local_ids.contains(&repo.id)
                || repo.starred
                || selections.contains_key(&repo.id)
                || previous_candidates.contains_key(&repo.id)
        });
        let (history, mut input_scope) = history(&saved.data.turns, &eligible, &selections)?;
        let terms = search_terms(message);
        candidates.sort_by_cached_key(|repo| {
            let note = repo.note.as_deref().filter(|_| {
                selections
                    .get(&repo.id)
                    .is_some_and(|item| item.include_notes)
            });
            let metadata = format!(
                "{} {} {} {} {}",
                repo.full_name,
                repo.description.as_deref().unwrap_or_default(),
                repo.language.as_deref().unwrap_or_default(),
                repo.topics.join(" "),
                note.unwrap_or_default()
            )
            .to_lowercase();
            let matches = terms
                .iter()
                .filter(|term| metadata.contains(term.as_str()))
                .count();
            std::cmp::Reverse((
                selections.contains_key(&repo.id),
                previous_candidates
                    .get(&repo.id)
                    .copied()
                    .filter(|sequence| recent_sequence.is_some_and(|recent| *sequence >= recent))
                    .unwrap_or_default(),
                matches,
                repo.starred,
            ))
        });

        let mut request = AgentRequest {
            message: message.to_owned(),
            history,
            selected_repositories: vec![],
            repositories: vec![],
        };
        let mut remaining = REQUEST_BYTES - json_bytes(&request)?;
        for repo in &candidates {
            let selection = selections.get(&repo.id);
            let local = local_ids.contains(&repo.id);
            let mut input = AgentRepository {
                id: repo.id,
                full_name: excerpt(repo.full_name.clone(), 256),
                description: repo.description.clone().map(|text| excerpt(text, 512)),
                language: repo.language.clone().map(|text| excerpt(text, 128)),
                topics: repo
                    .topics
                    .iter()
                    .take(10)
                    .cloned()
                    .map(|text| excerpt(text, 96))
                    .collect(),
                starred: repo.starred,
                readme: None,
                note: None,
            };
            // Reserve space for commas in both arrays and the selected repository ID.
            let cost = json_bytes(&input)? + 2 + json_bytes(&repo.id)?;
            if cost > remaining {
                break;
            }
            remaining -= cost;
            if let Some(note) = repo
                .note
                .as_ref()
                .filter(|_| selection.is_some_and(|item| item.include_notes))
            {
                input.note = pack_excerpt("note", note.clone(), 1536, &mut remaining)?;
            }
            if remaining > 32 {
                let readme = if local {
                    library
                        .cached_readme(repo.id)
                        .await
                        .context("Could not read the cached README")?
                } else if selection.is_some() {
                    self.read_candidate_readme(saved.id, repo.id, true).await?
                } else {
                    None
                };
                if let Some(readme) = readme {
                    input.readme = pack_excerpt("readme", readme, 2048, &mut remaining)?;
                }
            }
            input_scope.include(
                repo.id,
                SourceGrant {
                    private: repo.private,
                    note: input.note.is_some(),
                    external_readme: !local && input.readme.is_some(),
                },
            );
            if selection.is_some() {
                request.selected_repositories.push(repo.id);
            }
            request.repositories.push(input);
        }
        // Recommendation snapshots carry metadata only. Personal notes remain in the library.
        for repo in &mut candidates {
            repo.note = None;
        }
        Ok((request, candidates, input_scope))
    }
}

fn history(
    turns: &[Turn],
    eligible: &HashSet<i64>,
    selections: &HashMap<i64, &ContextSelection>,
) -> Result<(Vec<AgentHistory>, InputScope)> {
    let mut history = Vec::new();
    let mut remaining = HISTORY_BYTES;
    let mut input_scope = InputScope::default();
    for turn in turns.iter().rev().take(12) {
        let mut item = AgentHistory {
            message: excerpt(turn.message.clone(), 1024),
            recommended_ids: turn
                .candidates
                .iter()
                .map(|repo| repo.id)
                .filter(|id| eligible.contains(id))
                .take(32)
                .collect(),
            reply: None,
        };
        let basic_cost = json_bytes(&item)? + 1;
        if basic_cost > remaining {
            break;
        }
        // A reply can use any source that was sent, including sources from earlier replies.
        if let Some(scope) = &turn.input_scope {
            if scope.allowed(eligible, selections) {
                let budget = remaining.saturating_sub(basic_cost + 9).min(4096);
                item.reply = history_reply(&turn.response, budget)?;
                if item.reply.is_some() {
                    for (&id, &grant) in &scope.0 {
                        input_scope.include(id, grant);
                    }
                }
            } else {
                item.recommended_ids.clear();
            }
        }
        for &id in &item.recommended_ids {
            if let Some(repo) = turn.candidates.iter().find(|repo| repo.id == id) {
                input_scope.include(
                    id,
                    SourceGrant {
                        private: repo.private,
                        ..SourceGrant::default()
                    },
                );
            }
        }
        remaining -= json_bytes(&item)? + 1;
        history.push(item);
    }
    history.reverse();
    Ok((history, input_scope))
}

fn history_reply(response: &AgentResponse, budget: usize) -> Result<Option<AgentResponse>> {
    if json_bytes(response)? <= budget {
        return Ok(Some(response.clone()));
    }
    let mut reply = AgentResponse {
        text: excerpt(response.text.clone(), budget.min(512)),
        recommendations: vec![],
        gaps: vec![],
    };
    if reply.text.is_empty() || json_bytes(&reply)? > budget {
        return Ok(None);
    }
    for recommendation in &response.recommendations {
        reply.recommendations.push(recommendation.clone());
        if json_bytes(&reply)? > budget {
            reply.recommendations.pop();
            break;
        }
    }
    for gap in &response.gaps {
        reply.gaps.push(gap.clone());
        if json_bytes(&reply)? > budget {
            reply.gaps.pop();
            break;
        }
    }
    Ok(Some(reply))
}

fn search_terms(message: &str) -> HashSet<String> {
    let is_cjk = |ch| matches!(ch, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}' | '\u{ac00}'..='\u{d7af}');
    let mut terms = HashSet::new();
    for word in message.split(|ch: char| !ch.is_alphanumeric() || is_cjk(ch)) {
        if word.chars().count() >= 2 {
            terms.insert(word.to_lowercase());
        }
    }
    for group in message
        .split(|ch| !is_cjk(ch))
        .filter(|word| !word.is_empty())
    {
        let mut chars = group.chars();
        let mut previous = chars.next().unwrap();
        for ch in chars {
            terms.insert(String::from_iter([previous, ch]));
            previous = ch;
        }
    }
    terms
}

fn json_bytes<T: Serialize>(value: &T) -> Result<usize> {
    Ok(serde_json::to_vec(value)?.len())
}

fn excerpt(mut text: String, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

fn pack_excerpt(
    field: &str,
    text: String,
    limit: usize,
    remaining: &mut usize,
) -> Result<Option<String>> {
    let text = excerpt(text, limit);
    let cost = json_bytes(&text)? + field.len() + 4;
    if text.is_empty() || cost > *remaining {
        return Ok(None);
    }
    *remaining -= cost;
    Ok(Some(text))
}
