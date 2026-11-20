use std::{io::Read, path::Path, process::Stdio, time::Duration};

use anyhow::{bail, Context, Result};
use starchive_core::{Readme, RepoMetadata, RepoQuery, Store, SyncResult};
use tempfile::tempdir;
use tokio::{io::AsyncBufReadExt, process::Command};

fn repo(id: i64, name: &str) -> RepoMetadata {
    RepoMetadata {
        id,
        full_name: name.to_owned(),
        description: None,
        language: None,
        html_url: format!("https://github.com/{name}"),
        private: false,
    }
}

async fn sync(store: &Store, repositories: &[RepoMetadata]) -> Result<SyncResult> {
    store
        .try_sync_lease()
        .await?
        .context("Expected the sync lease")?
        .complete_snapshot(repositories)
        .await
}

#[tokio::test]
async fn snapshot_preserves_organization_by_id_across_rename_and_unstar() -> Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("library.sqlite3");
    let store = Store::open(&path).await?;
    let original = repo(1, "example/old-name");
    let removed = repo(2, "example/removed");
    assert_eq!(
        sync(&store, &[original.clone(), removed.clone()]).await?,
        SyncResult {
            starred: 2,
            newly_starred: 2,
            unstarred: 0,
        }
    );
    store
        .edit_repository(1, Some("中文离线搜索"), Some(&["search".to_owned()]))
        .await?;
    store
        .edit_repository(2, Some("Consider again"), Some(&["review".to_owned()]))
        .await?;

    let renamed = repo(1, "example/new-name");
    let replacement = repo(3, &original.full_name);
    assert_eq!(
        sync(&store, &[renamed.clone(), replacement]).await?,
        SyncResult {
            starred: 2,
            newly_starred: 1,
            unstarred: 1,
        }
    );
    store.pool().close().await;
    let reopened = Store::open(&path).await?;
    let found = reopened
        .get_repository_by_name("EXAMPLE/NEW-NAME")
        .await?
        .expect("renamed repository");
    assert_eq!(found.metadata, renamed);
    assert_eq!(found.note, "中文离线搜索");
    assert_eq!(found.tags, ["search"]);
    let reused_name = reopened
        .get_repository_by_name(&original.full_name)
        .await?
        .expect("repository using the old name");
    assert_eq!(reused_name.metadata.id, 3);
    assert!(reused_name.note.is_empty());

    let unstarred = reopened
        .list_repositories(&RepoQuery {
            starred: Some(false),
            ..RepoQuery::default()
        })
        .await?;
    assert_eq!(unstarred.len(), 1);
    assert_eq!(unstarred[0].metadata, removed);
    assert_eq!(unstarred[0].note, "Consider again");
    assert_eq!(unstarred[0].tags, ["review"]);

    assert_eq!(sync(&reopened, &[]).await?.unstarred, 2);
    assert_eq!(sync(&reopened, &[removed]).await?.newly_starred, 1);
    assert_eq!(
        reopened.get_repository(2).await?.unwrap().note,
        "Consider again"
    );
    Ok(())
}

#[tokio::test]
async fn search_composes_filters_and_edits_can_clear_individual_fields() -> Result<()> {
    let dir = tempdir()?;
    let store = Store::open(&dir.path().join("library.sqlite3")).await?;
    let mut search = repo(1, "example/indexer");
    search.language = Some("Rust".to_owned());
    search.description = Some("离线索引 100% local".to_owned());
    let mut private = repo(2, "example/private");
    private.description.clone_from(&search.description);
    private.language.clone_from(&search.language);
    private.private = true;
    sync(&store, &[search, private, repo(3, "example/other")]).await?;
    store
        .edit_repository(
            1,
            Some("Collection note only"),
            Some(&[" Search ".to_owned(), "search".to_owned(), " ".to_owned()]),
        )
        .await?;

    let by_name = store
        .list_repositories(&RepoQuery {
            query: Some(" INDEXER ".to_owned()),
            ..RepoQuery::default()
        })
        .await?;
    assert_eq!(by_name.len(), 1);
    assert_eq!(by_name[0].tags, ["Search"]);

    let by_description = store
        .list_repositories(&RepoQuery {
            query: Some("离线索引 100%".to_owned()),
            language: Some("rust".to_owned()),
            tag: Some("SEARCH".to_owned()),
            starred: Some(true),
            ..RepoQuery::default()
        })
        .await?;
    assert_eq!(by_description, by_name);
    let public_only = store
        .list_repositories(&RepoQuery {
            query: Some("%".to_owned()),
            public_only: true,
            ..RepoQuery::default()
        })
        .await?;
    assert_eq!(public_only, by_name);

    let mut by_note = RepoQuery {
        query: Some("collection NOTE".to_owned()),
        ..RepoQuery::default()
    };
    assert_eq!(store.list_repositories(&by_note).await?, by_name);
    by_note.exclude_notes = true;
    assert!(store.list_repositories(&by_note).await?.is_empty());

    let cleared_tags = store.edit_repository(1, None, Some(&[])).await?;
    assert_eq!(cleared_tags.note, "Collection note only");
    assert!(cleared_tags.tags.is_empty());
    let cleared_note = store.edit_repository(1, Some(""), None).await?;
    assert!(cleared_note.note.is_empty());
    assert!(store
        .edit_repository(999, Some("missing"), None)
        .await
        .is_err());
    Ok(())
}

#[tokio::test]
async fn remote_and_storage_failures_preserve_last_success_and_allow_retry() -> Result<()> {
    let dir = tempdir()?;
    let store = Store::open(&dir.path().join("library.sqlite3")).await?;
    let original = repo(1, "example/original");
    let original_result = sync(&store, std::slice::from_ref(&original)).await?;
    let original_status = store.sync_status().await?;
    assert!(original_status.last_successful_at.is_some());
    assert_eq!(original_status.last_result, Some(original_result));

    let lease = store.try_sync_lease().await?.unwrap();
    assert!(store.sync_status().await?.in_progress);
    assert!(store.try_sync_lease().await?.is_none());
    lease.record_failure("GitHub page 2 timed out").await?;
    let failed = store.sync_status().await?;
    assert!(!failed.in_progress);
    assert!(failed.last_attempt_at.is_some());
    assert_eq!(
        failed.last_error.as_deref(),
        Some("GitHub page 2 timed out")
    );
    assert_eq!(
        failed.last_successful_at,
        original_status.last_successful_at
    );
    assert_eq!(failed.last_result, original_status.last_result);

    let transient = repo(2, "example/new");
    let invalid = repo(0, "example/invalid");
    let failure = sync(&store, &[transient.clone(), invalid])
        .await
        .unwrap_err();
    assert!(
        failure.to_string().contains("Save GitHub repository 0"),
        "{failure:#}"
    );
    assert!(store.get_repository(2).await?.is_none());
    let previous = store.get_repository(original.id).await?.unwrap();
    assert_eq!(previous.metadata, original);
    assert!(previous.starred);
    let failed_write = store.sync_status().await?;
    assert!(!failed_write.in_progress);
    assert!(failed_write
        .last_error
        .unwrap()
        .contains("Save GitHub repository 0"));
    assert_eq!(
        failed_write.last_successful_at,
        original_status.last_successful_at
    );
    assert_eq!(failed_write.last_result, original_status.last_result);

    let result = sync(&store, &[transient]).await?;
    let retried = store.sync_status().await?;
    assert!(!retried.in_progress);
    assert!(retried.last_error.is_none());
    assert_eq!(retried.last_result, Some(result));
    Ok(())
}

#[tokio::test]
async fn cached_readme_survives_unstar_but_expires_on_identity_or_privacy_change() -> Result<()> {
    let dir = tempdir()?;
    let store = Store::open(&dir.path().join("library.sqlite3")).await?;
    let original = repo(1, "example/old-name");
    let readme = Readme {
        content: "# Library README".to_owned(),
        source_url: "https://github.com/example/old-name/blob/main/README.md".to_owned(),
    };
    assert!(!store.cache_readme(&original, &readme).await?);
    sync(&store, std::slice::from_ref(&original)).await?;
    assert!(store.cache_readme(&original, &readme).await?);
    sync(&store, &[]).await?;
    assert_eq!(store.get_readme(1).await?, Some(readme.clone()));

    let mut renamed = repo(1, "example/new-name");
    sync(&store, std::slice::from_ref(&renamed)).await?;
    assert!(store.get_readme(1).await?.is_none());
    assert!(!store.cache_readme(&original, &readme).await?);

    renamed.private = true;
    sync(&store, std::slice::from_ref(&renamed)).await?;
    let private = renamed.clone();
    assert!(store.cache_readme(&private, &readme).await?);
    assert!(store
        .get_readme_for(1, &private.full_name, false)
        .await?
        .is_none());
    assert!(store
        .get_readme_for(1, &original.full_name, true)
        .await?
        .is_none());
    assert_eq!(
        store.get_readme_for(1, &private.full_name, true).await?,
        Some(readme.clone())
    );
    renamed.private = false;
    sync(&store, std::slice::from_ref(&renamed)).await?;
    assert!(store.get_readme(1).await?.is_none());
    assert!(!store.cache_readme(&private, &readme).await?);
    Ok(())
}

#[tokio::test]
async fn separate_process_can_edit_while_syncing_and_a_crash_releases_the_lease() -> Result<()> {
    const CHILD_DB: &str = "STARCHIVE_TEST_LEASE_DB";
    const READY: &str = "STARCHIVE_TEST_LEASE_READY";

    if let Some(path) = std::env::var_os(CHILD_DB) {
        let child = Store::open(Path::new(&path)).await?;
        let _lease = child.try_sync_lease().await?.context("Child sync lease")?;
        child
            .edit_repository(1, Some("edited from child"), None)
            .await?;
        println!("{READY}");
        std::io::stdin().read_exact(&mut [0])?;
        return Ok(());
    }

    let dir = tempdir()?;
    let path = dir.path().join("library.sqlite3");
    let (store, second) = tokio::try_join!(Store::open(&path), Store::open(&path))?;
    let original = repo(1, "example/indexer");
    sync(&store, std::slice::from_ref(&original)).await?;

    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "separate_process_can_edit_while_syncing_and_a_crash_releases_the_lease",
            "--nocapture",
        ])
        .env(CHILD_DB, &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(line) = lines.next_line().await? {
            if line.ends_with(READY) {
                return Ok(());
            }
        }
        bail!("Child exited before acquiring the sync lease")
    })
    .await??;

    assert!(store.sync_status().await?.in_progress);
    assert!(second.try_sync_lease().await?.is_none());
    assert_eq!(
        store.get_repository(1).await?.unwrap().note,
        "edited from child"
    );
    second
        .edit_repository(1, None, Some(&["edited from parent".to_owned()]))
        .await?;
    child.kill().await?;

    let interrupted = store.sync_status().await?;
    assert!(!interrupted.in_progress);
    assert!(interrupted.last_error.unwrap().contains("interrupted"));
    let result = sync(&second, &[original]).await?;
    assert_eq!(result.newly_starred, 0);
    let saved = store.get_repository(1).await?.unwrap();
    assert_eq!(saved.note, "edited from child");
    assert_eq!(saved.tags, ["edited from parent"]);
    assert!(store.sync_status().await?.last_error.is_none());
    Ok(())
}
