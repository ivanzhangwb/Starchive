use std::{future::Future, time::Duration};

use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

use super::{GitHub, GitHubError};
use crate::{RepoMetadata, Store, SyncResult};

const FIRST_PAGE: &str = "/user/starred?per_page=100";
const SECOND_PAGE: &str = "/user/starred?page=2&per_page=100";
const NEXT_PAGE: &str = "Link: <?page=2&per_page=100>; rel=\"next\"\r\n";

#[derive(Clone, Copy)]
struct Reply<'a> {
    status: u16,
    headers: &'a str,
    body: &'a str,
    truncated: bool,
}

impl<'a> Reply<'a> {
    fn ok(body: &'a str) -> Self {
        Self {
            status: 200,
            headers: "",
            body,
            truncated: false,
        }
    }
}

struct Server(TcpListener);

impl Server {
    async fn start() -> (Self, GitHub) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let github = GitHub::with_base_url(&base, Some("example-token")).unwrap();
        (Self(listener), github)
    }

    async fn run<F: Future>(
        &self,
        operation: F,
        replies: &[Reply<'_>],
    ) -> (F::Output, Vec<String>) {
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut requests = Vec::new();
            let serve = async {
                loop {
                    let (stream, _) = self.0.accept().await.unwrap();
                    let mut stream = BufReader::new(stream);
                    let mut line = String::new();
                    stream.read_line(&mut line).await.unwrap();
                    let path = line.split_whitespace().nth(1).unwrap().to_owned();
                    requests.push(path);
                    let reply = replies.get(requests.len() - 1).unwrap_or_else(|| {
                        panic!("unexpected GitHub request after the fixture replies: {requests:?}");
                    });
                    loop {
                        line.clear();
                        assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                        if line == "\r\n" {
                            break;
                        }
                    }

                    let headers = format!(
                        "HTTP/1.1 {} Response\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                        reply.status,
                        reply.body.len(),
                        reply.headers,
                    );
                    let body = if reply.truncated {
                        // Send a complete star but close the socket before the array's final byte.
                        reply.body.strip_suffix(']').unwrap()
                    } else {
                        reply.body
                    };
                    stream.write_all(headers.as_bytes()).await.unwrap();
                    stream.write_all(body.as_bytes()).await.unwrap();
                    stream.shutdown().await.unwrap();
                }
            };
            let result = tokio::select! {
                result = operation => result,
                () = serve => unreachable!(),
            };
            (result, requests)
        })
        .await
        .expect("GitHub operation and loopback fixture timed out")
    }
}

fn repository(id: i64, full_name: &str) -> RepoMetadata {
    RepoMetadata {
        id,
        full_name: full_name.to_owned(),
        description: None,
        language: None,
        html_url: format!("https://github.com/{full_name}"),
        private: false,
    }
}

fn page(repo: RepoMetadata) -> String {
    json!([{"starred_at": "2025-05-09T10:11:12Z", "repo": repo}]).to_string()
}

#[tokio::test]
async fn truncated_second_page_retries_the_same_url_and_counts_each_page_once() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("library.sqlite"))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER verify_retry_counts BEFORE INSERT ON repositories
         BEGIN
             SELECT RAISE(ABORT, 'incorrect counts after page retry') WHERE NOT EXISTS (
                 SELECT 1 FROM library_sync_status WHERE running
                   AND json_extract(progress, '$.phase') = 'saving'
                   AND json_extract(progress, '$.pages') = 2
                   AND json_extract(progress, '$.repositories') = 2
             );
         END",
    )
    .execute(store.pool())
    .await
    .unwrap();

    let (server, github) = Server::start().await;
    let first = page(repository(1, "acme/first"));
    let second = page(repository(2, "acme/second"));
    let (result, requests) = server
        .run(
            github.sync(&store),
            &[
                Reply {
                    headers: NEXT_PAGE,
                    ..Reply::ok(&first)
                },
                Reply {
                    headers: "Link: <?page=99>; rel=\"next\"\r\n",
                    truncated: true,
                    ..Reply::ok(&second)
                },
                Reply::ok(&second),
            ],
        )
        .await;

    assert_eq!(requests, [FIRST_PAGE, SECOND_PAGE, SECOND_PAGE]);
    assert_eq!(
        result.unwrap(),
        SyncResult {
            starred: 2,
            newly_starred: 2,
            unstarred: 0,
        }
    );
    let mut ids = store
        .list_repositories(&Default::default())
        .await
        .unwrap()
        .into_iter()
        .map(|repo| repo.metadata.id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    assert_eq!(ids, [1, 2]);
}

#[tokio::test]
async fn exhausting_three_attempts_preserves_the_snapshot_and_records_a_clean_error() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("library.sqlite"))
        .await
        .unwrap();
    store
        .try_sync_lease()
        .await
        .unwrap()
        .unwrap()
        .complete_snapshot(&[
            repository(1, "acme/old-name"),
            repository(2, "acme/old-star"),
        ])
        .await
        .unwrap();
    store
        .edit_repository(1, Some("Read offline"), Some(&["收藏".to_owned()]))
        .await
        .unwrap();
    let previous = store.list_repositories(&Default::default()).await.unwrap();
    let previous_status = store.sync_status().await.unwrap();

    let (server, github) = Server::start().await;
    let first = page(repository(1, "acme/renamed"));
    let second = page(repository(3, "acme/new-star"));
    let truncated = Reply {
        truncated: true,
        ..Reply::ok(&second)
    };
    let (result, requests) = server
        .run(
            github.sync(&store),
            &[
                Reply {
                    headers: NEXT_PAGE,
                    ..Reply::ok(&first)
                },
                truncated,
                truncated,
                truncated,
            ],
        )
        .await;

    assert_eq!(
        requests,
        [FIRST_PAGE, SECOND_PAGE, SECOND_PAGE, SECOND_PAGE]
    );
    assert_eq!(
        store.list_repositories(&Default::default()).await.unwrap(),
        previous
    );
    let status = store.sync_status().await.unwrap();
    assert!(!status.in_progress);
    assert_eq!(status.progress, None);
    assert_eq!(
        status.last_successful_at,
        previous_status.last_successful_at
    );
    assert_eq!(status.last_result, previous_status.last_result);

    let message = result.unwrap_err().to_string();
    assert!(message.replace(' ', "").contains("第2页"), "{message}");
    assert!(message.contains("重试"), "{message}");
    assert!(
        message.contains("两次") || message.replace(' ', "").contains("2次"),
        "{message}"
    );
    assert_eq!(status.last_error.as_deref(), Some(message.as_str()));
    let lower = message.to_ascii_lowercase();
    for detail in [
        "close_notify",
        "connection closed",
        "response body",
        "end of file",
        "reqwest",
        "127.0.0.1",
        "example-token",
    ] {
        assert!(
            !lower.contains(detail),
            "unexpected {detail:?} in {message}"
        );
    }
}

#[tokio::test]
async fn schema_authentication_and_rate_limit_errors_do_not_retry_a_page() {
    for (status, headers, body) in [
        (
            200,
            "",
            r#"[{"starred_at":"2025-05-09T10:11:12Z","repo":{"id":"wrong type"}}]"#,
        ),
        (401, "", "{}"),
        (
            403,
            "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 12345\r\n",
            "{}",
        ),
        (429, "Retry-After: 60\r\n", "{}"),
    ] {
        let (server, github) = Server::start().await;
        let (result, requests) = server
            .run(
                github.starred(),
                &[
                    Reply {
                        headers: NEXT_PAGE,
                        ..Reply::ok("[]")
                    },
                    Reply {
                        status,
                        headers,
                        ..Reply::ok(body)
                    },
                ],
            )
            .await;

        assert_eq!(requests, [FIRST_PAGE, SECOND_PAGE], "HTTP {status}");
        let error = result.unwrap_err();
        let expected_error = match status {
            200 => matches!(error, GitHubError::InvalidStarPage(2)),
            401 => matches!(error, GitHubError::Unauthorized),
            403 => matches!(
                error,
                GitHubError::RateLimited {
                    retry_after: None,
                    reset_at: Some(12345)
                }
            ),
            429 => matches!(
                error,
                GitHubError::RateLimited {
                    retry_after: Some(60),
                    reset_at: None
                }
            ),
            _ => unreachable!(),
        };
        assert!(expected_error, "unexpected HTTP {status} error: {error}");
    }
}
