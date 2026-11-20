use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::json;
use tokio::sync::mpsc;

use super::*;

fn fixture(dir: &Path, body: &str) -> Result<PathBuf> {
    let script = dir.join("fixture-pi");
    std::fs::write(&script, format!("#!/bin/sh\n{body}\n"))?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
    Ok(script)
}

fn request(message: String) -> AgentRequest {
    AgentRequest {
        message,
        history: vec![],
        selected_repositories: vec![],
        repositories: vec![],
    }
}

#[tokio::test]
async fn streams_prose_before_pi_can_exit_and_returns_authoritative_final() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut live = vec![
        json!({"type": "session", "version": 3}),
        json!({"type": "message_start", "message": {"role": "assistant"}}),
        json!({"type": "message_update", "assistantMessageEvent": {"type": "thinking_delta", "delta": "PRIVATE_REASONING"}}),
        json!({"type": "message_update", "assistantMessageEvent": {"type": "toolcall_delta", "delta": "PRIVATE_TOOL_DATA"}}),
        json!({"type": "tool_execution_end", "result": {"content": [{"type": "text", "text": "PRIVATE_TOOL_RESULT"}]}}),
    ];
    let delta = |delta: &str| {
        json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 1, "delta": delta},
        })
    };
    live.extend([
        delta("```j"),
        delta(
            r#"son
{"recommendations":[{"text":"PRIVATE_EVIDENCE"}],"te"#,
        ),
        delta(r#"xt":"选\uD8"#),
        delta(r#"3D\u"#),
        delta(r#"DE00\"#),
        delta(r#"n\"#),
        delta(r#""quoted\"","gaps":["PRIVATE_GAP"]}"#),
    ]);
    let live = live
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let final_text = json!({"text": "Final corrected answer", "recommendations": [], "gaps": []});
    let final_event = json!({"type": "message_end", "message": {"role": "assistant", "stopReason": "stop", "content": [
        {"type": "thinking", "thinking": "PRIVATE_FINAL_REASONING"},
        {"type": "text", "text": final_text.to_string()}
    ]}});
    let path = fixture(
        dir.path(),
        &format!(
            r#"
[ "$1 $2 $3" = '--print --mode json' ] || exit 7
head -c 262144 /dev/zero >&2 || exit 8
cat >/dev/null || exit 9
printf '%s\n' '{live}'
while [ ! -f "$0.release" ]; do :; done
printf '%s\n' '{final_event}' '{{"type":"agent_settled","aborted":false}}'
"#
        ),
    )?;
    let (sender, mut updates) = mpsc::unbounded_channel();
    let agent = PiAgent::new(&path).with_text_delta(move |text| sender.send(text).unwrap());
    let input = request("x".repeat(262_144));
    let response = agent.respond(&input);
    tokio::pin!(response);
    let chunks = tokio::time::timeout(Duration::from_secs(10), async {
        let mut chunks = Vec::new();
        while chunks.len() < 4 {
            tokio::select! {
                result = &mut response => panic!("Pi finished before the live deltas arrived: {result:?}"),
                chunk = updates.recv() => chunks.push(chunk.unwrap()),
            }
        }
        chunks
    }).await?;
    assert_eq!(chunks, ["选", "😀", "\n", "\"quoted\""]);
    std::fs::write(path.with_extension("release"), "")?;
    let final_response = tokio::time::timeout(Duration::from_secs(10), response).await??;
    assert_eq!(final_response.text, "Final corrected answer");
    assert!(updates.try_recv().is_err());
    Ok(())
}

#[test]
fn automatic_retry_uses_final_message_without_appending_a_second_preview() -> Result<()> {
    let mut stream = EventStream::default();
    let previews = std::cell::RefCell::new(Vec::new());
    let callback = |text| previews.borrow_mut().push(text);
    let start = json!({"type": "message_start", "message": {"role": "assistant"}});
    let delta = |prose: &str| {
        json!({"type": "message_update", "assistantMessageEvent": {
            "type": "text_delta", "contentIndex": 0, "delta": format!("{{\"text\":\"{prose}\"")
        }})
    };
    let end = |reason: &str, text: &str| {
        json!({"type": "message_end", "message": {
            "role": "assistant", "stopReason": reason, "content": [{"type": "text", "text": text}]
        }})
    };
    for event in [
        start.clone(),
        delta("First attempt"),
        end("error", ""),
        start,
        delta("Second attempt"),
        end(
            "stop",
            r#"{"text":"Final answer","recommendations":[],"gaps":[]}"#,
        ),
    ] {
        stream.handle(event, &callback);
    }
    assert_eq!(previews.into_inner(), ["First attempt"]);
    assert_eq!(
        Output::Events(stream.completed).response()?.text,
        "Final answer"
    );
    Ok(())
}

#[tokio::test]
async fn streaming_mode_accepts_flat_fenced_response_without_replaying_final_text() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = fixture(
        dir.path(),
        r#"
cat >/dev/null
printf '%s\n' '```json' '{' '"text":"Flat response","recommendations":[],"gaps":[]' '}' '```'
"#,
    )?;
    let agent = PiAgent::new(path).with_text_delta(|chunk| panic!("Unexpected delta: {chunk}"));
    assert_eq!(
        agent.respond(&request("Compare".into())).await?.text,
        "Flat response"
    );
    Ok(())
}

#[tokio::test]
async fn timed_out_pi_child_is_reaped() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = fixture(
        dir.path(),
        r#"
cat >/dev/null
printf '%s\n' "$$" > "$0.pid"
exec /bin/sleep 30
"#,
    )?;
    let agent = PiAgent::new(&path).with_text_delta(|_| {});
    let error = agent
        .run(&request("Compare".into()), Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Pi timed out"));
    let pid = std::fs::read_to_string(path.with_extension("pid"))?;
    let output = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .output()?;
    assert!(!output.status.success(), "timed out Pi child still exists");
    Ok(())
}

#[tokio::test]
async fn unterminated_oversized_stdout_is_rejected() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = fixture(
        dir.path(),
        r#"
cat >/dev/null
exec head -c 20000000 /dev/zero
"#,
    )?;
    let agent = PiAgent::new(path).with_text_delta(|_| panic!("Non-JSON output reached callback"));
    let error = agent.respond(&request("Compare".into())).await.unwrap_err();
    assert!(error.to_string().contains("output limit"));
    Ok(())
}
