use anyhow::Result;
use serde_json::json;
use starchive_core::{Readme, RepoMetadata, Store};

use super::Desktop;

fn repo(id: i64, name: &str, private: bool) -> RepoMetadata {
    RepoMetadata {
        id,
        full_name: name.into(),
        description: Some("Offline search".into()),
        language: Some("Rust".into()),
        html_url: format!("https://github.com/{name}"),
        private,
    }
}

#[tokio::test]
async fn lists_and_edits_the_shared_library_with_lossless_ids() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("library.sqlite3");
    let store = Store::open(&path).await?;
    let large_id = 9_007_199_254_740_993;
    let meta = repo(large_id, "fixture/index", false);
    store
        .try_sync_lease()
        .await?
        .unwrap()
        .complete_snapshot(&[meta.clone(), repo(2, "fixture/other", true)])
        .await?;
    store
        .edit_repository(2, None, Some(&["other".into()]))
        .await?;
    store
        .cache_readme(
            &meta,
            &Readme {
                content: "Public README".into(),
                source_url: meta.html_url.clone(),
            },
        )
        .await?;
    let app = Desktop::open(&path).await?;
    let id = large_id.to_string();
    let updated = app
        .dispatch(
            "repo_update",
            json!({"repo_id": id, "note": {"expected": "", "value": "中文索引"}, "tags": {"expected": [], "value": ["desktop"]}}),
        )
        .await?["repo"].clone();
    assert_eq!(updated["id"], id);
    assert_eq!(
        store.get_repository(large_id).await?.unwrap().note,
        "中文索引"
    );
    let page = app.dispatch("repo_list", json!({"query": "中文", "language": null, "tag": null, "status": "starred", "offset": 0, "limit": 1})).await?;
    assert_eq!(page["total"], 1);
    assert_eq!(page["repos"], json!([updated]));
    assert_eq!(page["tags"], json!(["desktop", "other"]));
    let detail = app.dispatch("repo_get", json!({"repo_id": id})).await?;
    assert_eq!(detail["readme"], "Public README");
    assert!(app
        .dispatch("repo_get", json!({"repo_id": "999"}))
        .await?
        .is_null());
    let status = app.dispatch("sync_status", json!({})).await?;
    assert_eq!(status["running"], false);
    assert!(status.get("progress").is_none());
    assert_eq!(
        status["last_result"],
        json!({"state": "success", "starred_count": 2})
    );
    let lease = store.try_sync_lease().await?.unwrap();
    let preparing = app.dispatch("sync_status", json!({})).await?;
    assert_eq!(preparing["running"], true);
    assert_eq!(
        preparing["progress"],
        json!({"phase": "preparing", "pages": 0, "repositories": 0})
    );
    assert_eq!(preparing["last_success_at"], status["last_success_at"]);
    lease.record_failure("GitHub page 2 timed out").await?;
    let failed = app.dispatch("sync_status", json!({})).await?;
    assert_eq!(failed["running"], false);
    assert!(failed.get("progress").is_none());
    assert_eq!(
        failed["last_result"],
        json!({"state": "error", "message": "GitHub page 2 timed out"})
    );
    assert!(app
        .dispatch(
            "open_github",
            json!({"url": "https://github.com.example.org/fixture/index"})
        )
        .await
        .is_err());
    Ok(())
}

#[tokio::test]
async fn desktop_patches_only_dirty_fields_and_rejects_stale_fields_atomically() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("library.sqlite3");
    let cli_store = Store::open(&path).await?;
    cli_store
        .try_sync_lease()
        .await?
        .unwrap()
        .complete_snapshot(&[repo(1, "fixture/edit", false)])
        .await?;
    let desktop = Desktop::open(&path).await?;

    cli_store
        .edit_repository(1, Some("new CLI note"), None)
        .await?;
    let saved = desktop
        .dispatch(
            "repo_update",
            json!({
                "repo_id": "1", "tags": {"expected": [], "value": [" Desk ", "desk"]}
            }),
        )
        .await?;
    assert_eq!(saved["status"], "saved");
    assert_eq!(saved["repo"]["note"], "new CLI note");
    assert_eq!(saved["repo"]["tags"], json!(["Desk"]));

    let conflict = desktop
        .dispatch(
            "repo_update",
            json!({
                "repo_id": "1", "note": {"expected": "", "value": "my draft"},
                "tags": {"expected": ["Desk"], "value": []}
            }),
        )
        .await?;
    assert_eq!(conflict["status"], "conflict");
    assert_eq!(conflict["repo"]["note"], "new CLI note");
    assert_eq!(cli_store.get_repository(1).await?.unwrap().tags, ["Desk"]);

    let cleared = desktop
        .dispatch(
            "repo_update",
            json!({
                "repo_id": "1", "note": {"expected": "new CLI note", "value": ""},
                "tags": {"expected": ["Desk"], "value": []}
            }),
        )
        .await?;
    assert_eq!(cleared["status"], "saved");
    assert_eq!(cleared["repo"]["note"], "");
    assert_eq!(cleared["repo"]["tags"], json!([]));

    let other = Desktop::open(&path).await?;
    let (first, second) = tokio::join!(
        desktop.dispatch(
            "repo_update",
            json!({"repo_id": "1", "note": {"expected": "", "value": "first"}})
        ),
        other.dispatch(
            "repo_update",
            json!({"repo_id": "1", "note": {"expected": "", "value": "second"}})
        )
    );
    let (first, second) = (first?, second?);
    let persisted = cli_store.get_repository(1).await?.unwrap();
    let (saved, conflicted, value) = if first["status"] == "saved" {
        (first, second, "first")
    } else {
        (second, first, "second")
    };
    assert_eq!(saved["status"], "saved");
    assert_eq!(conflicted["status"], "conflict");
    assert_eq!(conflicted["repo"]["note"], value);
    assert_eq!(persisted.note, value);
    Ok(())
}

#[tokio::test]
async fn reopens_session_candidates_with_offline_readmes_only_in_their_own_session() -> Result<()> {
    use starchive_core::agent::{Agent, AgentRequest, AgentResponse, PublicSearch, Repository};

    struct Fixture;
    impl Agent for Fixture {
        async fn respond(&self, _: &AgentRequest) -> Result<AgentResponse> {
            Ok(AgentResponse {
                text: "Search options".into(),
                recommendations: vec![],
                gaps: vec![],
            })
        }
    }
    impl PublicSearch for Fixture {
        async fn search_public(&self, _: &str) -> Result<Vec<Repository>> {
            Ok(vec![Repository {
                id: 9,
                full_name: "fixture/external".into(),
                description: None,
                language: None,
                html_url: "https://github.com/fixture/external".into(),
                topics: vec![],
                private: false,
                starred: false,
                note: None,
            }])
        }
    }
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("library.sqlite3");
    let store = Store::open(&path).await?;
    let conversations = starchive_core::agent::Conversations::new(&store);
    let session = conversations.create("Saved search").await?;
    let library = starchive_core::runtime::AgentLibrary::load(&store).await?;
    conversations
        .ask(session.id, "Find options", &library, &Fixture)
        .await?;
    conversations
        .expand(session.id, "offline search", &Fixture)
        .await?;
    conversations
        .cache_candidate_readme(session.id, 9, "README saved offline")
        .await?;
    store.pool().close().await;

    let app = Desktop::open(&path).await?;
    let id = session.id.to_string();
    let session = app
        .dispatch("session_get", json!({"session_id": id}))
        .await?;
    assert_eq!(session["messages"][2]["candidates"][0]["repo"]["id"], "9");
    let detail = app
        .dispatch("repo_get", json!({"repo_id": "9", "session_id": id}))
        .await?;
    assert_eq!(detail["repo"]["full_name"], "fixture/external");
    assert_eq!(detail["readme"], "README saved offline");
    assert_eq!(detail["editable"], false);
    assert!(app
        .dispatch("repo_get", json!({"repo_id": "9"}))
        .await?
        .is_null());
    let another = app.dispatch("session_create", json!({})).await?;
    assert!(app
        .dispatch(
            "repo_get",
            json!({"repo_id": "9", "session_id": another["id"]})
        )
        .await?
        .is_null());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn restored_history_stays_local_after_revoking_context() -> Result<()> {
    use std::{env, fs, os::unix::fs::PermissionsExt, process::Command};

    let dir = tempfile::tempdir()?;
    // Run settings writes against a temporary home without changing other tests' environment.
    if env::var_os("STARCHIVE_DESKTOP_SETTINGS_TEST").is_none() {
        let output = Command::new(env::current_exe()?)
            .args([
                "--exact",
                "dispatch::tests::restored_history_stays_local_after_revoking_context",
            ])
            .env("STARCHIVE_DESKTOP_SETTINGS_TEST", "1")
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path())
            .env("GH_TOKEN", "synthetic-settings-token")
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return Ok(());
    }

    let path = dir.path().join("library.sqlite3");
    let pi = dir.path().join("test pi");
    let request_path = dir.path().join("test pi.request");
    fs::write(
        &pi,
        r#"#!/bin/sh
cat > "$0.request"
if grep -q 'synthetic-note-sentinel' "$0.request"; then
    printf '%s\n' '{"text":"synthetic-assistant-sentinel","recommendations":[{"repo_id":2,"fit":"Test private source","evidence":[{"source":"note","excerpt":"synthetic-note-sentinel"},{"source":"readme","excerpt":"synthetic-readme-sentinel"}],"gaps":[]}],"gaps":[]}'
else
    printf '%s\n' '{"text":"Public follow-up","recommendations":[],"gaps":[]}'
fi
"#,
    )?;
    fs::set_permissions(&pi, fs::Permissions::from_mode(0o700))?;
    let store = Store::open(&path).await?;
    let public = repo(1, "fixture/public", false);
    let private = repo(2, "fixture/private", true);
    store
        .try_sync_lease()
        .await?
        .unwrap()
        .complete_snapshot(&[public.clone(), private.clone()])
        .await?;
    store
        .edit_repository(
            2,
            Some("synthetic-note-sentinel"),
            Some(&["synthetic-tag-sentinel".into()]),
        )
        .await?;
    store
        .cache_readme(
            &private,
            &Readme {
                content: "synthetic-readme-sentinel".into(),
                source_url: private.html_url.clone(),
            },
        )
        .await?;

    let app = Desktop::open(&path).await?;
    let status = app
        .dispatch("settings_save_pi", json!({"path": pi}))
        .await?;
    assert_eq!(
        status["github"],
        json!({"available": true, "source": "environment"})
    );
    assert!(!status.to_string().contains("synthetic-settings-token"));
    assert_eq!(
        status["pi"],
        json!({"available": true, "source": "saved", "path": pi})
    );
    assert_eq!(app.dispatch("settings_get", json!({})).await?, status);
    let error = app
        .dispatch("settings_save_github", json!({"token": "invalid sentinel"}))
        .await
        .unwrap_err();
    assert!(!format!("{error:#}").contains("sentinel"));
    assert!(app
        .dispatch("settings_save_github", json!({}))
        .await
        .is_err());
    assert_eq!(
        app.dispatch("settings_save_github", json!({"token": null}))
            .await?,
        status
    );
    assert_eq!(
        app.dispatch("agent_list", json!({})).await?[0]["available"],
        true
    );
    let created = app
        .dispatch("session_create", json!({"agent_id": null}))
        .await?;
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["can_expand"], false);
    app.dispatch(
        "session_set_agent",
        json!({"session_id": id, "agent_id": "pi"}),
    )
    .await?;
    let error = app
        .dispatch(
            "agent_expand",
            json!({"session_id": id, "public_query": "public search"}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Send a message"));
    app.dispatch(
        "session_set_context",
        json!({"session_id": id, "repo_id": "2", "include_private": true, "include_notes": true}),
    )
    .await?;
    let first = app
        .dispatch(
            "agent_send",
            json!({"session_id": id, "text": "First request"}),
        )
        .await?;
    assert_eq!(first["can_expand"], true);
    assert_eq!(
        first["messages"][1]["candidates"][0]["repo"]["private"],
        true
    );
    let request = fs::read_to_string(&request_path)?;
    assert!(request.contains("synthetic-note-sentinel"));
    assert!(!request.contains("synthetic-tag-sentinel"));
    app.dispatch(
        "session_remove_context",
        json!({"session_id": id, "repo_id": "2"}),
    )
    .await?;
    drop(app);
    store
        .try_sync_lease()
        .await?
        .unwrap()
        .complete_snapshot(&[public])
        .await?;

    let app = Desktop::open(&path).await?;
    let restored = app
        .dispatch("session_get", json!({"session_id": id}))
        .await?;
    assert_eq!(restored["context"], json!([]));
    let old_reply = &restored["messages"][1];
    assert_eq!(old_reply["text"], "synthetic-assistant-sentinel");
    assert_eq!(old_reply["candidates"][0]["repo"]["starred"], false);
    assert!(old_reply["candidates"][0]["repo"].get("note").is_none());
    let next_pi = dir.path().join("replacement pi");
    fs::copy(&pi, &next_pi)?;
    let status = app
        .dispatch("settings_save_pi", json!({"path": next_pi}))
        .await?;
    assert_eq!(status["pi"]["path"], json!(next_pi));
    let followup = app
        .dispatch(
            "agent_send",
            json!({"session_id": id, "text": "Follow-up request"}),
        )
        .await?;
    assert_eq!(followup["messages"][3]["text"], "Public follow-up");
    let request = fs::read_to_string(dir.path().join("replacement pi.request"))?;
    assert!(request.contains("fixture/public"));
    assert!(request.contains("First request"));
    assert!(!request.contains("fixture/private"));
    assert!(!request.contains("sentinel"));
    assert!(app.dispatch("settings_save_pi", json!({})).await.is_err());
    let cleared = app
        .dispatch("settings_save_pi", json!({"path": null}))
        .await?;
    assert_ne!(cleared["pi"]["source"], "saved");
    Ok(())
}
