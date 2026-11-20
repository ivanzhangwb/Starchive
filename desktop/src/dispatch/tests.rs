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
    let app = Desktop::open(&path, dir.path().join("missing pi")).await?;
    let id = large_id.to_string();
    let updated = app
        .dispatch(
            "repo_update",
            json!({"repo_id": id, "note": "中文索引", "tags": ["desktop"]}),
        )
        .await?;
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
    assert_eq!(
        app.dispatch("agent_list", json!({})).await?[0]["available"],
        false
    );
    let status = app.dispatch("sync_status", json!({})).await?;
    assert_eq!(
        status["last_result"],
        json!({"state": "success", "starred_count": 2})
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

#[cfg(unix)]
#[tokio::test]
async fn restored_history_stays_local_after_revoking_context() -> Result<()> {
    use std::{fs, os::unix::fs::PermissionsExt};

    let dir = tempfile::tempdir()?;
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

    let app = Desktop::open(&path, pi.clone()).await?;
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

    let app = Desktop::open(&path, pi).await?;
    let restored = app
        .dispatch("session_get", json!({"session_id": id}))
        .await?;
    assert_eq!(restored["context"], json!([]));
    let old_reply = &restored["messages"][1];
    assert_eq!(old_reply["text"], "synthetic-assistant-sentinel");
    assert_eq!(old_reply["candidates"][0]["repo"]["starred"], false);
    assert!(old_reply["candidates"][0]["repo"].get("note").is_none());
    let followup = app
        .dispatch(
            "agent_send",
            json!({"session_id": id, "text": "Follow-up request"}),
        )
        .await?;
    assert_eq!(followup["messages"][3]["text"], "Public follow-up");
    let request = fs::read_to_string(request_path)?;
    assert!(request.contains("fixture/public"));
    assert!(request.contains("First request"));
    assert!(!request.contains("fixture/private"));
    assert!(!request.contains("sentinel"));
    Ok(())
}
