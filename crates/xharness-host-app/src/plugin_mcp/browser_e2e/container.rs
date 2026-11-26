//! Uses a verified plugin ZIP, two real Host sessions and real Docker Chromium.
#![forbid(unsafe_code)]

use super::*;
use std::collections::BTreeSet;
use xharness_tools::{ToolFailureKind, ToolRequest, ToolResult};

const PLUGIN: &str = "browser-use";
const ORIGIN: &str = "http://approved.test";

async fn tool(executor: &ToolExecutor, envelope: Value) -> ToolResult {
    executor
        .execute(ToolRequest::new("plugin_mcp", envelope.to_string()))
        .await
}
fn call_envelope(arguments: Value) -> Value {
    json!({"action":"call","plugin":PLUGIN,"server":"browser","tool":"browser","arguments":arguments})
}
fn output(result: ToolResult) -> Value {
    assert!(result.is_ok(), "{:?}", result.failure);
    serde_json::from_str(&result.output.unwrap().content).unwrap()
}
async fn browser(executor: &ToolExecutor, arguments: Value) -> Value {
    let envelope = output(tool(executor, call_envelope(arguments)).await);
    serde_json::from_str(envelope["content"][0]["text"].as_str().unwrap()).unwrap()
}
async fn containers(policy: &str) -> BTreeSet<String> {
    let result = tokio::process::Command::new("/usr/bin/docker")
        .args([
            "--host",
            "unix:///var/run/docker.sock",
            "ps",
            "-aq",
            "--no-trunc",
            "--filter",
        ])
        .arg(format!("label=io.xharness.browser.policy={policy}"))
        .output()
        .await
        .unwrap();
    assert!(result.status.success());
    String::from_utf8(result.stdout)
        .unwrap()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}
async fn until<T: std::future::Future<Output = bool>>(mut condition: impl FnMut() -> T) {
    tokio::time::timeout(Duration::from_secs(22), async {
        while !condition().await {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("container acceptance condition not met");
}
fn check_page(value: &Value, text: &str) -> String {
    let observed = &value["observation"];
    assert_eq!(observed["title"], "Container browser acceptance");
    assert!(observed["text"].as_str().unwrap().contains(text));
    observed["tabId"].as_str().unwrap().into()
}
async fn rejected(executor: &ToolExecutor, arguments: Value, code: &str) {
    let failed = tool(executor, call_envelope(arguments))
        .await
        .failure
        .unwrap();
    assert!(!failed.retryable);
    assert!(
        failed.message.contains(code),
        "unexpected tool error: {failed:?}"
    );
}

#[tokio::test]
#[ignore = "isolated Linux Docker acceptance; run through the operator fixture, never the installed Host"]
async fn real_host_starts_reuses_isolates_and_reclaims_chat_containers() {
    assert_eq!(
        std::env::var("XHARNESS_BROWSER_TEST_SERVER").as_deref(),
        Ok("1")
    );
    let operator = PathBuf::from(std::env::var_os("XHARNESS_CONTAINER_TEST_DIR").unwrap());
    let config = PathBuf::from(std::env::var_os("XHARNESS_BROWSER_CONTAINER_CONFIG").unwrap());
    assert_eq!(config.parent(), Some(operator.as_path()));
    let policy = format!("{:x}", Sha256::digest(std::fs::read(config).unwrap()));
    let data = PathBuf::from(std::env::var_os("XHARNESS_CONTAINER_TEST_ZIP").unwrap());
    let root = Root(operator.join("host"));
    std::fs::create_dir(&root.0).unwrap();
    let manager = install_archive(&root, PLUGIN, "0.2.0", &std::fs::read(data).unwrap()).await;
    let mcp = McpRuntime::new();
    let provider = Arc::new(ScriptedProvider {
        url: format!("{ORIGIN}/"),
        plugin: PLUGIN,
        requests: Mutex::new(Vec::new()),
    });
    let tools = Arc::new(BrowserTools {
        host: OnceLock::new(),
        manager,
        mcp: mcp.clone(),
    });
    let store =
        Arc::new(xharness_session_jsonl::JsonlSessionStore::new(root.0.join("sessions")).unwrap());
    let runtime = Arc::new(DurableLoopAgentRuntime::new(
        "p",
        "m",
        Some(provider.clone()),
        tools.clone(),
        Arc::new(xharness_core::IdentityContextPolicy),
        store.clone(),
        Arc::new(xharness_agent::MemoryLeaseManager::default()),
        32,
    ));
    let mut registry = xharness_host::ModelRegistry::new();
    registry
        .register(xharness_host::RegisteredModel::new(
            xharness_host::ModelDescriptor::new(
                "p",
                "Scripted provider",
                "m",
                "Container inspector",
            )
            .with_image_input(
                Some(true),
                json!({"state":"supported","source":"test-fixture"}),
            ),
            provider.clone(),
        ))
        .unwrap();
    runtime.replace_model_registry(registry);
    let mut host_config = HostConfig::new(&root.0);
    host_config.home = root.0.clone();
    host_config.provider_id = "p".into();
    host_config.model_id = "m".into();
    host_config.attachment_store = Arc::new(
        xharness_attachments::FileAttachmentStore::new(root.0.join("attachments")).unwrap(),
    );
    let host = BasicHost::with_agent_runtime(host_config, runtime.clone());
    tools.host.set(Arc::downgrade(&host)).unwrap();
    let a = rpc(&host, RpcMethod::SessionCreate, json!({"cwd":root.0})).await;
    let b = rpc(&host, RpcMethod::SessionCreate, json!({"cwd":root.0})).await;
    let sid_a = a["sessionId"].as_str().unwrap();
    let sid_b = b["sessionId"].as_str().unwrap();
    assert_ne!(sid_a, sid_b);
    let cwd = root.0.to_str().unwrap();
    let exec_a = tools
        .executor(sid_a, cwd, Default::default())
        .await
        .unwrap();
    let exec_b = tools
        .executor(sid_b, cwd, Default::default())
        .await
        .unwrap();

    let index = output(tool(&exec_a, json!({"action":"list"})).await);
    assert_eq!(index["capabilities"][0]["backend"], "container");
    let list = output(
        tool(
            &exec_a,
            json!({"action":"list","plugin":PLUGIN,"server":"browser"}),
        )
        .await,
    );
    assert_eq!(list["tools"].as_array().unwrap().len(), 1);
    assert_eq!(list["tools"][0]["name"], "browser");
    let descriptor = output(
        tool(
            &exec_a,
            json!({"action":"describe","plugin":PLUGIN,"server":"browser","tool":"browser"}),
        )
        .await,
    );
    assert_eq!(descriptor["tool"]["inputSchema"]["type"], "object");
    assert_eq!(
        browser(&exec_a, json!({"action":"tabs"})).await["result"],
        json!([])
    );
    rejected(
        &exec_a,
        json!({"action":"navigate","url":"http://not-approved.test/"}),
        "origin_not_allowed",
    )
    .await;
    assert!(
        containers(&policy).await.is_empty(),
        "discovery/rejection must not allocate"
    );
    println!("PASS discovery and denied origin allocate zero containers");

    let opened = browser(&exec_a, json!({"action":"navigate","url":ORIGIN})).await;
    let tab_a = check_page(&opened, "state-fresh");
    let only_a = containers(&policy).await;
    assert_eq!(only_a.len(), 1);
    let saved = browser(
        &exec_a,
        json!({"action":"click","tabId":tab_a,
        "target":{"by":"role","role":"button","name":"Store test state","exact":true}}),
    )
    .await;
    assert_eq!(saved["outcome"], "applied");
    let same = browser(
        &exec_a,
        json!({"action":"navigate","url":ORIGIN,"tabId":tab_a}),
    )
    .await;
    assert_eq!(check_page(&same, "state-retained"), tab_a);
    assert_eq!(containers(&policy).await, only_a);
    println!("PASS authorized site and same-chat reuse (container ID, tab ID, cookie)");

    let opened = browser(&exec_b, json!({"action":"navigate","url":ORIGIN})).await;
    let tab_b = check_page(&opened, "state-fresh");
    let both = containers(&policy).await;
    assert_eq!(both.len(), 2);
    assert_ne!(tab_a, tab_b);
    let only_b = both.difference(&only_a).cloned().collect::<BTreeSet<_>>();
    assert_eq!(only_b.len(), 1);
    rejected(
        &exec_b,
        json!({"action":"observe","tabId":tab_a}),
        "operation_rejected",
    )
    .await;
    println!("PASS two chats isolate container, cookie and tab handle");

    // The fixture proves the request crossed the proxy before we cancel it.
    // Never infer dispatch from a sleep, or retry a cancelled effect.
    let cancelled = CancellationToken::new();
    let hanging = exec_a.execute(
        ToolRequest::new(
            "plugin_mcp",
            call_envelope(
                json!({"action":"navigate","url":format!("{ORIGIN}/slow"),"tabId":tab_a}),
            )
            .to_string(),
        )
        .with_cancellation(cancelled.clone()),
    );
    tokio::pin!(hanging);
    let marker = operator.join("slow-started");
    tokio::select! {
        result = &mut hanging => panic!("slow navigation finished before cancellation: {result:?}"),
        _ = until(|| async { marker.is_file() }) => {},
    }
    assert_eq!(
        check_page(
            &browser(&exec_b, json!({"action":"observe","tabId":tab_b})).await,
            "state-fresh"
        ),
        tab_b
    );
    cancelled.cancel();
    assert_eq!(
        hanging.await.failure_kind(),
        Some(ToolFailureKind::Cancelled)
    );
    until(|| async { containers(&policy).await == only_b }).await;
    assert_eq!(
        check_page(
            &browser(&exec_b, json!({"action":"observe","tabId":tab_b})).await,
            "state-fresh"
        ),
        tab_b
    );
    println!("PASS cancel retires only the dispatched chat; the other container and tab survive");

    until(|| async { containers(&policy).await.is_empty() }).await;
    rejected(
        &exec_b,
        json!({"action":"observe","tabId":tab_b}),
        "browser_context_not_open",
    )
    .await;
    assert!(
        containers(&policy).await.is_empty(),
        "stale handle must not start a browser"
    );
    let reopened = browser(&exec_b, json!({"action":"navigate","url":ORIGIN})).await;
    assert_ne!(check_page(&reopened, "state-fresh"), tab_b);
    assert_eq!(containers(&policy).await.len(), 1);
    mcp.disconnect_owner(sid_b).await;
    until(|| async { containers(&policy).await.is_empty() }).await;
    println!("PASS idle reclamation, stale handle rejected, fresh start, owner disconnect");

    rpc(
        &host,
        RpcMethod::SessionPrompt,
        json!({"sessionId":sid_a,"mode":"queue",
        "content":[{"type":"text","text":"Capture the authorized fixture"}]}),
    )
    .await;
    until(|| async {
        store
            .load(sid_a)
            .await
            .unwrap()
            .unwrap()
            .events()
            .iter()
            .any(|event| matches!(event.data(), EventData::TurnEnd { .. }))
    })
    .await;
    let attachment = {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        requests[2]
            .messages
            .iter()
            .filter(|message| message.role == xharness_core::Role::Tool)
            .flat_map(|message| &message.content_blocks)
            .find_map(|block| match block {
                ContentBlock::Image { attachment } => Some(attachment.clone()),
                _ => None,
            })
            .expect("the real Host must project the screenshot into model input")
    };
    assert_eq!(attachment.session_id, sid_a);
    assert_eq!((attachment.width, attachment.height), (1280, 800));
    let resolved = host
        .attachment_store()
        .resolve(sid_a, &attachment.id)
        .await
        .unwrap();
    let pixels = image::load_from_memory(&resolved.data).unwrap().to_rgb8();
    assert_eq!(pixels.get_pixel(20, 20).0, [255, 0, 0]);
    assert!(host
        .attachment_store()
        .resolve(sid_b, &attachment.id)
        .await
        .is_err());
    store.flush(sid_a).await.unwrap();
    let history = rpc(&host, RpcMethod::SessionHistory, json!({"sessionId":sid_a}))
        .await
        .to_string();
    assert!(history.contains(&attachment.id));
    assert!(!history.contains("iVBOR"));
    assert!(runtime.shutdown(Duration::from_secs(5)).await.is_graceful());
    mcp.shutdown().await;
    until(|| async { containers(&policy).await.is_empty() }).await;
    println!("PASS real Host model-turn screenshot, chat-scoped attachment/history, shutdown reclamation");
}
