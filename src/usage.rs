//! Codex subscription quotas, collected once per server from its current Codex login.
use std::io;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UsageWindow {
    #[schemars(range(min = 0, max = 100))]
    pub used_percent: u8,
    #[schemars(range(min = 1))]
    pub duration_minutes: u32,
    #[schemars(range(min = 1))]
    pub resets_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CodexUsage {
    pub plan_type: Option<String>,
    #[schemars(length(max = 2))]
    pub windows: Vec<UsageWindow>,
    pub updated_at: i64,
    pub stale: bool,
}

impl CodexUsage {
    pub(crate) fn valid(&self) -> bool {
        self.windows.len() <= 2
            && self.plan_type.as_ref().is_none_or(|s| {
                s.len() <= 32
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
            && self
                .windows
                .iter()
                .all(|w| w.used_percent <= 100 && w.duration_minutes > 0 && w.resets_at > 0)
    }
}

pub(crate) fn parse_limits(result: &serde_json::Value, now: i64) -> io::Result<Option<CodexUsage>> {
    let limits = result
        .get("rateLimitsByLimitId")
        .and_then(|v| v.get("codex"))
        .or_else(|| result.get("rateLimits"))
        .ok_or_else(|| io::Error::other("Codex response has no rate limits"))?;
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let Some(window) = limits.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        let percent = window["usedPercent"]
            .as_f64()
            .filter(|v| v.is_finite() && (0.0..=100.0).contains(v));
        let duration = window["windowDurationMins"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0);
        let reset = window["resetsAt"].as_i64().filter(|n| *n > 0);
        let (Some(percent), Some(duration_minutes), Some(resets_at)) = (percent, duration, reset)
        else {
            return Err(io::Error::other("invalid Codex quota window"));
        };
        windows.push(UsageWindow {
            used_percent: percent.round() as u8,
            duration_minutes,
            resets_at,
        });
    }
    windows.sort_by_key(|w| w.duration_minutes);
    let usage = CodexUsage {
        plan_type: limits
            .get("planType")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        windows,
        updated_at: now,
        stale: false,
    };
    if !usage.valid() {
        return Err(io::Error::other("invalid Codex quota metadata"));
    }
    Ok((!usage.windows.is_empty()).then_some(usage))
}

pub(crate) struct UsageRuntime {
    pub value: Option<CodexUsage>,
    next_refresh: Instant,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Default for UsageRuntime {
    fn default() -> Self {
        Self {
            value: None,
            next_refresh: Instant::now(),
            task: None,
        }
    }
}

impl Drop for UsageRuntime {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl UsageRuntime {
    pub fn refresh_if_due(
        &mut self,
        now: Instant,
        tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) {
        if self.task.is_some() || now < self.next_refresh {
            return;
        }
        self.next_refresh = now + Duration::from_secs(60);
        self.task = Some(tokio::spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(15), collect())
                .await
                .unwrap_or_else(|_| {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Codex usage request timed out",
                    ))
                });
            let _ = tx
                .send(crate::events::AppEvent::CodexUsageRefreshed(result))
                .await;
        }));
    }

    pub fn complete(&mut self, result: io::Result<Option<CodexUsage>>) {
        self.task = None;
        match result {
            Ok(value) => self.value = value,
            Err(error) => {
                tracing::debug!(%error, "Codex usage refresh unavailable");
                if let Some(value) = &mut self.value {
                    value.stale = true;
                }
            }
        }
    }
}

async fn collect() -> io::Result<Option<CodexUsage>> {
    let command = crate::noninteractive_process::command("codex");
    let mut command = tokio::process::Command::from(command);
    let mut child = command
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing Codex stdin"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing Codex stdout"))?;
    let result = exchange(input, output).await;
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

async fn exchange(
    mut input: impl tokio::io::AsyncWrite + Unpin,
    output: impl tokio::io::AsyncRead + Unpin,
) -> io::Result<Option<CodexUsage>> {
    // Bound total output as well as request duration, including unsolicited notifications.
    let mut lines = BufReader::new(output.take(1024 * 1024)).lines();
    input.write_all(b"{\"id\":0,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"herdr_limits\",\"version\":\"1.0\"}}}\n").await?;

    while let Some(line) = lines.next_line().await? {
        let message: serde_json::Value = serde_json::from_str(&line).map_err(io::Error::other)?;
        match message.get("id").and_then(|id| id.as_u64()) {
            Some(0) => {
                if message.get("error").is_some() {
                    return Err(io::Error::other("Codex initialization rejected"));
                }
                input.write_all(b"{\"method\":\"initialized\"}\n{\"id\":1,\"method\":\"account/read\",\"params\":{\"refreshToken\":false}}\n").await?;
            }
            Some(1) => {
                if message.get("error").is_some() {
                    return Err(io::Error::other("Codex account request failed"));
                }
                let account = &message["result"]["account"];
                if account.is_null() || account["type"].as_str() == Some("apiKey") {
                    return Ok(None);
                }
                input
                    .write_all(b"{\"id\":2,\"method\":\"account/rateLimits/read\"}\n")
                    .await?;
            }
            Some(2) => {
                return parse_limits(
                    &message["result"],
                    time::OffsetDateTime::now_utc().unix_timestamp(),
                )
            }
            _ => {}
        }
    }
    Err(io::Error::other("Codex exited before returning usage"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn window(duration: u32) -> serde_json::Value {
        json!({"usedPercent": 41.2, "windowDurationMins": duration, "resetsAt": 1900000000})
    }

    #[tokio::test]
    async fn collector_is_single_flight_and_aborted_with_its_owner() {
        let mut runtime = UsageRuntime::default();
        let task = tokio::spawn(std::future::pending());
        let abort = task.abort_handle();
        let id = task.id();
        runtime.task = Some(task);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        runtime.refresh_if_due(Instant::now(), tx);
        assert_eq!(runtime.task.as_ref().unwrap().id(), id);
        drop(runtime);
        tokio::task::yield_now().await;
        assert!(abort.is_finished());
    }

    #[tokio::test]
    async fn collector_handshake_reads_account_before_quotas() {
        let (client, server) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(client);
        let mock = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server);
            let mut lines = BufReader::new(read).lines();
            let init: serde_json::Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(init["method"], "initialize");
            write
                .write_all(b"{\"id\":0,\"result\":{}}\n")
                .await
                .unwrap();
            let initialized: serde_json::Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(initialized["method"], "initialized");
            let account: serde_json::Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(account["method"], "account/read");
            write
                .write_all(b"{\"id\":1,\"result\":{\"account\":{\"type\":\"chatgpt\"}}}\n")
                .await
                .unwrap();
            let quota: serde_json::Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(quota["method"], "account/rateLimits/read");
            let response = json!({"id":2, "result":{"rateLimits":{"primary":window(10080)}}})
                .to_string()
                + "\n";
            write.write_all(response.as_bytes()).await.unwrap();
        });
        let usage = exchange(write, read).await.unwrap().unwrap();
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].duration_minutes, 10080);
        mock.await.unwrap();
    }

    #[tokio::test]
    async fn api_key_and_logged_out_accounts_have_no_subscription_widget() {
        for account in [serde_json::Value::Null, json!({"type":"apiKey"})] {
            let responses = format!(
                "{}\n{}\n",
                json!({"id":0,"result":{}}),
                json!({"id":1,"result":{"account":account}})
            );
            assert_eq!(
                exchange(tokio::io::sink(), responses.as_bytes())
                    .await
                    .unwrap(),
                None
            );
        }
        assert!(exchange(tokio::io::sink(), b"".as_slice()).await.is_err());
    }

    #[test]
    fn windows_follow_payload_not_plan_or_primary_position() {
        for plan in ["plus", "pro", "free", "future_plan"] {
            let result = json!({"rateLimits": {"planType": plan, "primary": window(10080), "secondary": null}});
            let usage = parse_limits(&result, 1).unwrap().unwrap();
            assert_eq!(usage.windows.len(), 1);
            assert_eq!(usage.windows[0].duration_minutes, 10080);
            let result = json!({"rateLimits": {"planType": plan, "primary": window(10080), "secondary": window(300)}});
            let usage = parse_limits(&result, 1).unwrap().unwrap();
            assert_eq!(
                usage
                    .windows
                    .iter()
                    .map(|w| w.duration_minutes)
                    .collect::<Vec<_>>(),
                vec![300, 10080]
            );
        }
    }

    #[test]
    fn bucket_map_takes_precedence_and_credits_do_not_invent_windows() {
        let result = json!({"rateLimits": {"primary": window(300)}, "rateLimitsByLimitId": {"codex": {"primary": null, "secondary": null, "credits": {"unlimited": true}}}});
        assert_eq!(parse_limits(&result, 1).unwrap(), None);
        for invalid in [
            json!({}),
            json!({"rateLimits": {"primary": {"usedPercent": 101}}}),
            json!({"rateLimits": {"primary": {"usedPercent": 5, "windowDurationMins": 0, "resetsAt": 1}}}),
        ] {
            assert!(parse_limits(&invalid, 1).is_err());
        }
    }

    #[test]
    fn failure_retains_last_success_as_stale_and_logout_clears_it() {
        let mut runtime = UsageRuntime::default();
        runtime.complete(parse_limits(
            &json!({"rateLimits": {"primary": window(300)}}),
            1,
        ));
        runtime.complete(Err(io::Error::other("offline")));
        assert!(runtime.value.as_ref().unwrap().stale);
        runtime.complete(Ok(None));
        assert!(runtime.value.is_none());
    }
}
