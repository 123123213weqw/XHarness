//! Host executable → shared Chromium controller → real page → final model HTTP.
//! Fixtures and a temporary profile only; no Tauri, external site, or paid model.
use std::time::Duration;
use tokio::process::Command;

fn host_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xharness-host"));
    // Keep the platform environment but none of the user's Host/model config.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("XHARNESS_")
            || xharness_process::is_secret_env_name(&name)
        {
            command.env_remove(name);
        }
    }
    command.kill_on_drop(true);
    command
}

#[tokio::test]
async fn standalone_browser_configuration_fails_closed_at_startup() {
    for (options, reason) in [
        (vec![], "requires a standalone authenticated loopback Host"),
        (
            vec![
                "--desktop-token",
                "a-local-fixture-token-long-enough",
                "--bind",
                "0.0.0.0:0",
            ],
            "must bind to a loopback",
        ),
        (
            vec![
                "--desktop-token",
                "a-local-fixture-token-long-enough",
                "--remote-control",
                "enabled",
            ],
            "without hosted mode or remote control",
        ),
        (
            vec![
                "--desktop-token",
                "a-local-fixture-token-long-enough",
                "--hosted-permit-dir",
                "not-opened",
            ],
            "without hosted mode or remote control",
        ),
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            host_command()
                .args(["--local-browser", "enabled"])
                .args(options)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(reason), "{stderr}");
    }
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        host_command()
            .args([
                "--local-browser",
                "enabled",
                "--desktop-token",
                "a-local-fixture-token-long-enough",
            ])
            .env("XHARNESS_NATIVE_BROWSER_ADDRESS", "127.0.0.1:1")
            .env("XHARNESS_NATIVE_BROWSER_TOKEN", "a".repeat(64))
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be combined"));
}

#[cfg(target_os = "linux")]
mod live {
    use super::*;
    use axum::{
        response::Html,
        routing::{get, post},
        Json, Router,
    };
    use serde_json::{json, Value};
    use std::{
        path::PathBuf,
        process::Stdio,
        sync::{Arc, Mutex},
    };
    use xharness_session::{EventData, Store, ToolOutcome, TurnEndReason};

    const TOKEN: &str = "temporary-loopback-authenticated-fixture";
    const SESSION: &str = "no-vision-browser";
    const SCRIPT: &str = r#"
        await new Promise(resolve => setTimeout(resolve, 50));
        const response = await fetch('/items');
        const data = await response.json();
        document.querySelector('output').textContent = data.count + args.extra;
        console.info('browser-async-finished', response.status);
        void fetch('/late?private=debug-query-sentinel', {
            headers: {"Authorization": "Bearer debug-header-sentinel"},
        }).then(async result => {
            await result.text();
            console.error('browser-late-fixture', result.status);
            await fetch('/late-logged');
        });
        return {title: document.title, payload: {total: Number(document.querySelector('output').textContent), items: data.items}};
    "#;

    struct FixtureDir(PathBuf);
    impl FixtureDir {
        fn new() -> Self {
            let mut id = [0; 8];
            getrandom::fill(&mut id).unwrap();
            let path = std::env::temp_dir()
                .join(format!("xh-standalone-browser-{}", u64::from_le_bytes(id)));
            std::fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for FixtureDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn model_receipt(body: &Value, id: &str) -> Value {
        let message = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "tool" && m["tool_call_id"] == id)
            .unwrap();
        let receipt: Value = serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
        assert!(
            receipt.get("isError").is_none() && receipt.get("content").is_none(),
            "{receipt}"
        );
        receipt
    }

    fn response(step: usize, origin: &str, body: &Value) -> String {
        let debug_id = || model_receipt(body, "call-1")["capture_id"].clone();
        let tool = match step {
            0 => Some((
                "browser_control",
                json!({"backend":"browser","action":"open","url":origin}),
            )),
            1 => Some((
                "browser_debug",
                json!({"backend":"browser","action":"start"}),
            )),
            2 => Some((
                "browser_evaluate",
                json!({"backend":"browser","code":SCRIPT,"args":{"extra":5}}),
            )),
            3 => Some((
                "browser_debug",
                json!({"backend":"browser","action":"read","capture_id":debug_id(),"after":0}),
            )),
            4 => Some((
                "browser_evaluate",
                json!({"backend":"browser","code":"throw new Error('fixture diagnostic');"}),
            )),
            5 => Some((
                "browser_debug",
                json!({"backend":"browser","action":"stop","capture_id":debug_id()}),
            )),
            6 => Some((
                "browser_debug",
                json!({"backend":"browser","action":"read","capture_id":debug_id()}),
            )),
            7 => None,
            _ => panic!("unexpected extra generation call"),
        };
        let (delta, reason) = if let Some((name, arguments)) = tool {
            (
                json!({"tool_calls":[{"index":0,"id":format!("call-{step}"),"type":"function","function":{"name":name,"arguments":arguments.to_string()}}]}),
                "tool_calls",
            )
        } else {
            (
                json!({"content":"Read the fixture without images."}),
                "stop",
            )
        };
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]})
        )
    }

    async fn rpc(client: &reqwest::Client, host: &str, method: &str, payload: Value) -> Value {
        let result: Value = client.post(format!("{host}/api/{method}"))
            .header("x-xharness-desktop-token", TOKEN)
            .json(&json!({"type":"client-request","rpcId":format!("test-{method}"),"method":method,"payload":payload}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(result["result"]["ok"], true, "{result}");
        result["result"]["value"].clone()
    }

    #[tokio::test]
    #[ignore = "explicit real Chrome/Edge acceptance; Linux CI runs this without a desktop or model key"]
    async fn standalone_host_executes_async_js_and_sends_only_structured_text() {
        assert!(
            xharness_browser::installed_available(),
            "CI must have real Chrome/Edge"
        );
        let dir = FixtureDir::new();
        let tmp = dir.0.join("tmp");
        std::fs::create_dir(&tmp).unwrap();
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let requests = captured.clone();
        let fixture = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", fixture.local_addr().unwrap());
        let page = origin.clone();
        let release_late = Arc::new(tokio::sync::Notify::new());
        let late_logged = Arc::new(tokio::sync::Notify::new());
        let late_wait = release_late.clone();
        let logged = late_logged.clone();
        let routes = Router::new()
            .route(
                "/",
                get(|| async { Html("<title>Async browser fixture</title><output>0</output>") }),
            )
            .route(
                "/items",
                get(|| async { Json(json!({"count":37,"items":["one","two"]})) }),
            )
            .route(
                "/late",
                get(move || {
                    let late_wait = late_wait.clone();
                    async move {
                        tokio::time::timeout(Duration::from_secs(25), late_wait.notified())
                            .await
                            .expect("provider must receive the evaluate result first");
                        (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            "debug-body-sentinel",
                        )
                    }
                }),
            )
            .route(
                "/late-logged",
                get(move || {
                    let logged = logged.clone();
                    async move {
                        logged.notify_one();
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            )
            .route(
                "/v1/chat/completions",
                post(move |Json(body): Json<Value>| {
                    let requests = requests.clone();
                    let page = page.clone();
                    let release_late = release_late.clone();
                    let late_logged = late_logged.clone();
                    async move {
                        let (step, sse) = {
                            let mut requests = requests.lock().unwrap();
                            let step = requests.len();
                            let sse = response(step, &page, &body);
                            requests.push(body);
                            (step, sse)
                        };
                        if step == 3 {
                            // This response cannot happen during browser_evaluate:
                            // the model endpoint has already received its final receipt.
                            release_late.notify_one();
                            tokio::time::timeout(Duration::from_secs(10), late_logged.notified())
                                .await
                                .expect("the page must log after the delayed network response");
                        }
                        ([("content-type", "text/event-stream")], sse)
                    }
                }),
            );
        let fixture = tokio::spawn(async move { axum::serve(fixture, routes).await.unwrap() });
        let ready = dir.0.join("ready");
        let stop = dir.0.join("stop");
        let state = dir.0.join("state");
        let mut host = host_command()
            .current_dir(&dir.0)
            .env("TMPDIR", &tmp)
            .env("HOME", &dir.0)
            .env("XHARNESS_DESKTOP_TOKEN", TOKEN)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .args([
                "--bind",
                "127.0.0.1:0",
                "--local-browser",
                "enabled",
                "--base-url",
                &format!("{origin}/v1"),
                "--model",
                "text-only-fixture",
                "--context-window",
                "65536",
                "--compaction-config",
                "off",
            ])
            .arg("--ready-file")
            .arg(&ready)
            .arg("--shutdown-file")
            .arg(&stop)
            .arg("--state-dir")
            .arg(&state)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let base = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                assert!(
                    host.try_wait().unwrap().is_none(),
                    "Host exited during boot"
                );
                if let Ok(address) = tokio::fs::read_to_string(&ready).await {
                    let base = format!("http://{address}");
                    if client
                        .get(format!("{base}/health/ready"))
                        .send()
                        .await
                        .is_ok_and(|r| r.status().is_success())
                    {
                        break base;
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();

        assert_eq!(
            client
                .post(format!("{base}/api/session.create"))
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        rpc(
            &client,
            &base,
            "session.create",
            json!({"sessionId":SESSION}),
        )
        .await;
        // Explicit test operator approval in this isolated Host; no production defaults change.
        rpc(
            &client,
            &base,
            "commands/execute",
            json!({"args":{"agentId":SESSION,"line":"/permission danger-full-access","images":[]}}),
        )
        .await;
        rpc(&client,&base,"session.prompt",json!({"sessionId":SESSION,"mode":"queue","content":[{"type":"text","text":"Read the localhost dataset using async JavaScript; no screenshots."}]})).await;

        let store = xharness_session_jsonl::JsonlSessionStore::new(state.join("sessions")).unwrap();
        let (reason, outcomes) = tokio::time::timeout(Duration::from_secs(65), async {
            loop {
                if let Some(session) = store.load(SESSION).await.unwrap() {
                    if let Some(reason) = session.events().iter().find_map(|e| match e.data() {
                        EventData::TurnEnd { reason, .. } => Some(reason.clone()),
                        _ => None,
                    }) {
                        let outcomes = session
                            .events()
                            .iter()
                            .filter_map(|e| match e.data() {
                                EventData::ToolResult { result, .. } => Some(result.outcome),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        break (reason, outcomes);
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        let requests = captured.lock().unwrap().clone();
        assert_eq!(reason, TurnEndReason::Completed);
        assert_eq!(requests.len(), 8, "unexpected generation count");
        // This transcript is from the loopback fixture only. Preserve the exact
        // tool failure on CI instead of reporting just three Error variants.
        let tool_receipts: Vec<_> = requests[7]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "tool")
            .map(|message| (&message["tool_call_id"], &message["content"]))
            .collect();
        assert_eq!(
            outcomes,
            [
                ToolOutcome::Success,
                ToolOutcome::Success,
                ToolOutcome::Success,
                ToolOutcome::Success,
                ToolOutcome::Error,
                ToolOutcome::Success,
                ToolOutcome::Error,
            ],
            "unexpected loopback browser receipts: {tool_receipts:?}"
        );
        for body in &requests {
            let text = body.to_string();
            for forbidden in ["image_url", "input_image", "data:image/", "png_base64"] {
                assert!(!text.contains(forbidden), "{forbidden} in {body}");
            }
            let tools = body["tools"].as_array().unwrap();
            let eval = tools
                .iter()
                .find(|t| t["function"]["name"] == "browser_evaluate")
                .unwrap();
            assert_eq!(
                eval["function"]["parameters"]["properties"]["backend"]["enum"],
                json!(["browser"])
            );
            let debug = tools
                .iter()
                .find(|t| t["function"]["name"] == "browser_debug")
                .unwrap();
            assert_eq!(
                debug["function"]["parameters"]["properties"]["backend"]["enum"],
                json!(["browser"])
            );
            assert!(tools.iter().all(|t| !matches!(
                t["function"]["name"].as_str(),
                Some("browser_request" | "browser_read_response")
            )));
        }
        let receipt = |id: &str| model_receipt(&requests[7], id);
        assert_eq!(receipt("call-0")["state"], "ready");
        let capture = receipt("call-1");
        assert_eq!(capture["state"], "recording");
        let eval = receipt("call-2");
        assert_eq!(eval["ok"], true);
        assert_eq!(
            eval["value"],
            json!({"title":"Async browser fixture","payload":{"total":42,"items":["one","two"]}})
        );
        assert!(eval["console"][0]["text"]
            .as_str()
            .unwrap()
            .contains("browser-async-finished"));
        assert!(!eval.to_string().contains("browser-late-fixture"));
        let debug = receipt("call-3");
        assert_eq!(debug["capture_id"], capture["capture_id"]);
        assert_eq!(debug["gap"], false);
        assert_eq!(debug["untrusted"], true);
        let events = debug["events"].as_array().unwrap();
        assert!(
            events.iter().any(|event| event["kind"] == "console"
                && event["level"] == "error"
                && event["text"] == "browser-late-fixture 500"),
            "{debug}"
        );
        assert!(
            events.iter().any(|event| event["kind"] == "network"
                && event["phase"] == "finished"
                && event["status"] == 500
                && event["url"] == format!("{origin}/late")),
            "{debug}"
        );
        for forbidden in [
            "debug-query-sentinel",
            "debug-header-sentinel",
            "debug-body-sentinel",
        ] {
            assert!(!debug.to_string().contains(forbidden), "{debug}");
        }
        let failure = receipt("call-4");
        assert_eq!(failure["ok"], false);
        assert_eq!(failure["effect"], "unknown");
        assert_eq!(failure["error"]["message"], "fixture diagnostic");
        assert_eq!(receipt("call-5")["state"], "stopped");
        assert_eq!(receipt("call-6")["ok"], false);

        let owned_profiles = || {
            std::fs::read_dir(&tmp)
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("xharness-owned-browser-")
                })
                .count()
        };
        assert_eq!(
            owned_profiles(),
            1,
            "browser should still be open before Host shutdown"
        );
        tokio::fs::write(stop, b"stop").await.unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(20), host.wait())
            .await
            .unwrap()
            .unwrap()
            .success());
        assert_eq!(
            owned_profiles(),
            0,
            "Host shutdown must close the browser and remove its profile"
        );
        fixture.abort();
        assert!(fixture.await.unwrap_err().is_cancelled());
    }
}
