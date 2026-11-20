use std::{path::PathBuf, process::ExitCode};

use anyhow::{ensure, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::json;
use starchive_core::{github::GitHub, runtime, RepoQuery, Repository, Store};

#[derive(Parser)]
#[command(
    version,
    about = "Sync and organize your GitHub stars in a local library"
)]
struct Cli {
    /// Override the shared SQLite file (default: STARCHIVE_DB_PATH or the OS data directory).
    #[arg(long, global = true, value_name = "FILE")]
    db: Option<PathBuf>,
    /// Print JSON for scripts.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Fetch all GitHub stars and update the local library.
    Sync,
    /// Show the current and last successful sync status.
    Status,
    /// List saved repositories.
    List {
        #[arg(long)]
        query: Option<String>,
        #[command(flatten)]
        filters: Filters,
    },
    /// Search names, descriptions and personal notes locally.
    Search {
        query: String,
        #[command(flatten)]
        filters: Filters,
    },
    /// View a saved repository and its cached README by ID or owner/repo.
    Show {
        repo: String,
        /// Request a new copy of the README from GitHub.
        #[arg(long)]
        refresh_readme: bool,
    },
    /// Replace the note, tags, or both on a saved repository.
    Edit {
        repo: String,
        #[arg(long)]
        note: Option<String>,
        /// Replace tags. Repeat this option for each tag.
        #[arg(long, value_name = "TAG")]
        tag: Vec<String>,
        #[arg(long, conflicts_with = "tag")]
        clear_tags: bool,
    },
}

#[derive(Args)]
struct Filters {
    #[arg(long)]
    language: Option<String>,
    #[arg(long)]
    tag: Option<String>,
    #[arg(long, value_enum, default_value = "starred")]
    status: Status,
}

#[derive(Clone, ValueEnum)]
enum Status {
    Starred,
    Unstarred,
    All,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let path = cli.db.map_or_else(runtime::database_path, Ok)?;
    let store = Store::open(&path).await?;
    match cli.command {
        Action::Sync => {
            let result = GitHub::from_env()?.sync(&store).await?;
            if cli.json {
                print_json(&result)?;
            } else {
                println!(
                    "{} starred ({} added, {} unstarred).",
                    result.starred, result.newly_starred, result.unstarred
                );
            }
        }
        Action::Status => {
            let status = store.sync_status().await?;
            if cli.json {
                print_json(&status)?;
            } else {
                if status.in_progress {
                    println!("GitHub sync is running.");
                }
                println!(
                    "Last successful sync: {}",
                    status.last_successful_at.as_deref().unwrap_or("never")
                );
                if let Some(result) = status.last_result {
                    println!(
                        "{} starred ({} added, {} unstarred).",
                        result.starred, result.newly_starred, result.unstarred
                    );
                }
                if let Some(error) = status.last_error {
                    println!("Last sync error: {error}");
                }
            }
        }
        Action::List { query, filters } => list(&store, query, filters, cli.json).await?,
        Action::Search { query, filters } => list(&store, Some(query), filters, cli.json).await?,
        Action::Show {
            repo,
            refresh_readme,
        } => {
            let repo = find_repo(&store, &repo).await?;
            if refresh_readme {
                runtime::refresh_readme(&store, &repo.metadata).await?;
            }
            let meta = &repo.metadata;
            let readme = store
                .get_readme_for(meta.id, &meta.full_name, meta.private)
                .await?;
            if cli.json {
                print_json(&json!({"repo": repo, "readme": readme}))?;
            } else {
                print_repo(&repo);
                println!("{}", meta.html_url);
                if !repo.tags.is_empty() {
                    println!("Tags: {}", repo.tags.join(", "));
                }
                if !repo.note.is_empty() {
                    println!("Note: {}", repo.note);
                }
                if let Some(readme) = readme {
                    println!("\n{}", readme.content);
                }
            }
        }
        Action::Edit {
            repo,
            note,
            tag,
            clear_tags,
        } => {
            let tags = (!tag.is_empty() || clear_tags).then_some(tag);
            ensure!(
                note.is_some() || tags.is_some(),
                "Provide --note, --tag, or --clear-tags to edit a repository"
            );
            let id = find_repo(&store, &repo).await?.metadata.id;
            let updated = store
                .edit_repository(id, note.as_deref(), tags.as_deref())
                .await?;
            if cli.json {
                print_json(&updated)?;
            } else {
                println!("Saved notes and tags for {}.", updated.metadata.full_name);
            }
        }
    }
    Ok(())
}

async fn list(store: &Store, query: Option<String>, filters: Filters, json: bool) -> Result<()> {
    let repos = store
        .list_repositories(&RepoQuery {
            query,
            language: filters.language,
            tag: filters.tag,
            starred: match filters.status {
                Status::Starred => Some(true),
                Status::Unstarred => Some(false),
                Status::All => None,
            },
            ..RepoQuery::default()
        })
        .await?;
    if json {
        print_json(&repos)?;
    } else {
        for repo in &repos {
            print_repo(repo);
        }
    }
    Ok(())
}

async fn find_repo(store: &Store, value: &str) -> Result<Repository> {
    match value.parse::<i64>() {
        Ok(id) => store.get_repository(id).await?,
        Err(_) => store.get_repository_by_name(value).await?,
    }
    .with_context(|| format!("Repository {value} was not found in the local library"))
}

fn print_repo(repo: &Repository) {
    let meta = &repo.metadata;
    let status = if repo.starred { "starred" } else { "unstarred" };
    let visibility = if meta.private { ", private" } else { "" };
    println!("{} [{status}{visibility}]", meta.full_name);
    if let Some(description) = &meta.description {
        println!("  {description}");
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}
