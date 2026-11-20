use std::{path::Path, process::Command};

use anyhow::{ensure, Result};
use serde_json::Value;
use starchive_core::{RepoMetadata, Store};

fn cli(db: &Path, args: &[&str]) -> Result<Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_starchive"))
        .args(["--json", "--db"])
        .arg(db)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[tokio::test]
async fn search_and_edit_use_the_shared_library() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("library.sqlite3");
    let store = Store::open(&db).await?;
    let repo = RepoMetadata {
        id: 10,
        full_name: "fixture/index".into(),
        description: None,
        language: Some("Rust".into()),
        html_url: "https://github.com/fixture/index".into(),
        private: false,
    };
    store
        .try_sync_lease()
        .await?
        .unwrap()
        .complete_snapshot(&[repo])
        .await?;
    cli(
        &db,
        &[
            "edit",
            "fixture/index",
            "--note",
            "离线搜索",
            "--tag",
            "desktop",
        ],
    )?;
    assert_eq!(store.get_repository(10).await?.unwrap().note, "离线搜索");
    let results = cli(
        &db,
        &["search", "离线", "--language", "Rust", "--tag", "desktop"],
    )?;
    assert_eq!(results[0]["full_name"], "fixture/index");
    cli(&db, &["edit", "10", "--clear-tags"])?;
    assert_eq!(cli(&db, &["show", "10"])?["repo"]["note"], "离线搜索");
    assert!(store.get_repository(10).await?.unwrap().tags.is_empty());
    Ok(())
}
