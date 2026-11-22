//! Opt-in experimental worker, not wired into the shipped Host.
use super::*;
use crate::{
    AgentRuntime, DurableLoopAgentRuntime, HostConfig, PermissionPreset, RecoveryPolicy,
    SessionToolFactory,
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::UnixDatagram;
use xharness_api::{ApiBackend, RpcId, RpcMethod, RpcResult};
use xharness_core::{
    FinishReason, IdentityContextPolicy, ModelProvider, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream, Role,
};
use xharness_tools::{
    ToolDefinition, ToolExecutor, ToolHandlerError, ToolOutput, ToolRegistry, ToolSpec,
};

const SESSION: &str = "job-follow";
const NAMES: [&str; 3] = ["inventory.json", "snapshot.tar.gz", "validation.json"];
const STAGES: [&str; 3] = ["inventory_ready", "archive_ready", "completed"];

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    run_id: String,
    seq: usize,
    stage: String,
    worker_pid: u32,
    sha256: String,
}

fn receipt(root: &Path, run: &str, seq: usize) -> Result<Receipt, String> {
    let i = seq
        .checked_sub(1)
        .filter(|i| *i < NAMES.len())
        .ok_or("bad sequence")?;
    let path = root.join("receipts").join(format!("{seq:03}.json"));
    let data = fs::read(path).map_err(|e| e.to_string())?;
    if data.len() > 1024 {
        return Err("receipt too large".into());
    }
    let r: Receipt = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    if r.run_id != run
        || r.seq != seq
        || r.stage != STAGES[i]
        || r.worker_pid == 0
        || r.sha256.len() != 64
        || !r.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("receipt identity mismatch".into());
    }
    Ok(r)
}

fn verify(root: &Path, run: &str, seq: usize) -> Result<Value, String> {
    let r = receipt(root, run, seq)?;
    let artifact = root.join("artifacts").join(NAMES[seq - 1]);
    if fs::metadata(&artifact).map_err(|e| e.to_string())?.len() > 2 * 1024 * 1024 {
        return Err("artifact too large".into());
    }
    let bytes = fs::read(artifact).map_err(|e| e.to_string())?;
    if format!("{:x}", Sha256::digest(&bytes)) != r.sha256 {
        return Err("artifact digest does not match the worker receipt".into());
    }
    let bytes = serde_json::to_vec(&r).map_err(|e| e.to_string())?;
    let path = root.join("verified").join(format!("{seq:03}.json"));
    if path.exists() {
        if fs::read(path).map_err(|e| e.to_string())? != bytes {
            return Err("a different follow-up already exists".into());
        }
    } else {
        let tmp = path.with_extension("tmp");
        let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(tmp, &path).map_err(|e| e.to_string())?;
        fs::File::open(root.join("verified"))
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(json!({"seq":seq,"stage":r.stage,"verified":true}))
}

struct Tools {
    root: PathBuf,
    run: String,
}
#[async_trait]
impl SessionToolFactory for Tools {
    async fn executor(
        &self,
        id: &str,
        _: &str,
        _: PermissionPreset,
    ) -> Result<ToolExecutor, String> {
        if id != SESSION {
            return Err("unexpected session".into());
        }
        let registry = Arc::new(ToolRegistry::new());
        let root = self.root.clone();
        let run = self.run.clone();
        registry
            .register(ToolSpec::new(
                ToolDefinition::new(
                    "verify_followed_job",
                    "Verify a checkpoint receipt and its known artifact in the isolated run.",
                    json!({
                        "type":"object","additionalProperties":false,"required":["seq"],
                        "properties":{"seq":{"type":"integer","minimum":1,"maximum":3}}
                    }),
                ),
                move |ctx| {
                    let (root, run) = (root.clone(), run.clone());
                    async move {
                        let seq = ctx.arguments["seq"]
                            .as_u64()
                            .filter(|n| (1..=3).contains(n))
                            .ok_or_else(|| ToolHandlerError::new("invalid sequence"))?;
                        let result =
                            verify(&root, &run, seq as usize).map_err(ToolHandlerError::new)?;
                        Ok(ToolOutput::text(result.to_string()))
                    }
                },
            ))
            .await
            .map_err(|e| e.to_string())?;
        Ok(ToolExecutor::new(registry))
    }
}

struct Model {
    counter: StdMutex<fs::File>,
}
#[async_trait]
impl ModelProvider for Model {
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let trigger = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.as_str())
            .unwrap_or("");
        let seq = trigger
            .strip_prefix("WATCH_CHECKPOINT ")
            .and_then(|n| n.parse::<usize>().ok());
        let events = {
            let mut out = self.counter.lock().unwrap();
            writeln!(
                &mut *out,
                "{}",
                json!({"host_pid":std::process::id(),"seq":seq})
            )
            .unwrap();
            out.sync_data().unwrap();
            if let Some(n) = seq.filter(|_| {
                !request
                    .messages
                    .last()
                    .is_some_and(|m| m.role == Role::Tool)
            }) {
                vec![
                    ProviderEvent::ToolCallDelta {
                        index: 0,
                        id: format!("watch-check-{n}"),
                        name: "verify_followed_job".into(),
                        arguments_delta: json!({"seq":n}).to_string(),
                    },
                    ProviderEvent::Completed {
                        finish_reason: Some(FinishReason::ToolCalls),
                        usage: None,
                        provider_items: vec![],
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta(match seq {
                        Some(n) => format!("CHECKPOINT_FOLLOWUP_{n}"),
                        None => "FOLLOW_REGISTERED".into(),
                    }),
                    ProviderEvent::Completed {
                        finish_reason: Some(FinishReason::Stop),
                        usage: None,
                        provider_items: vec![],
                    },
                ]
            }
        };
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

async fn rpc(host: &BasicHost, id: &str, method: RpcMethod, payload: Value) {
    let res = host
        .call(RpcId::new(id), method, payload, CancellationToken::new())
        .await;
    assert!(matches!(res, RpcResult::Success { .. }), "{res:?}");
}

async fn reconcile(root: &Path, run: &str, host: &BasicHost, store: &dyn Store) {
    for seq in 1..=NAMES.len() {
        if !root
            .join("receipts")
            .join(format!("{seq:03}.json"))
            .is_file()
        {
            break;
        }
        let r = receipt(root, run, seq).expect("valid supervisor receipt");
        let id = format!("watch:{run}:{seq}");
        assert!(host.enqueue_internal_once(
            store, SESSION, &id, &format!("WATCH_CHECKPOINT {seq}"),
            json!({"kind":"job-settlement","experiment":"follow-job","jobId":run,"status":r.stage,"seq":seq}),
        ).await.expect("durable inbox admission"));
    }
}

#[tokio::test]
#[ignore = "Experimental child process for run_job_watch.py; requires isolated scratch data"]
async fn isolated_host_worker() {
    let root = PathBuf::from(std::env::var_os("XH_WATCH_ROOT").expect("set only by harness"));
    assert!(root.join("EXPERIMENT_ONLY").is_file());
    let run = fs::read_to_string(root.join("EXPERIMENT_ONLY")).unwrap();
    assert_eq!(run.len(), 32);
    let store: Arc<dyn Store> =
        Arc::new(xharness_session_jsonl::JsonlSessionStore::new(root.join("sessions")).unwrap());
    let counter = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(root.join("provider.jsonl"))
        .unwrap();
    let runtime = Arc::new(DurableLoopAgentRuntime::new(
        "watch-fixture",
        "fixture",
        Some(Arc::new(Model {
            counter: StdMutex::new(counter),
        })),
        Arc::new(Tools {
            root: root.clone(),
            run: run.clone(),
        }),
        Arc::new(IdentityContextPolicy),
        store.clone(),
        Arc::new(xharness_agent::MemoryLeaseManager::default()),
        2048,
    ));
    let mut config = HostConfig::new(&root);
    config.provider_id = "watch-fixture".into();
    config.model_id = "fixture".into();
    let host = BasicHost::with_agent_runtime(config, runtime.clone());
    let socket_path = root.join("watch.sock");
    if socket_path.exists() {
        fs::remove_file(&socket_path).unwrap();
    }
    let socket = UnixDatagram::bind(&socket_path).unwrap();
    if std::env::var("XH_WATCH_PHASE").as_deref() == Ok("initial") {
        host.start_background_turn_listener();
        rpc(
            &host,
            "create",
            RpcMethod::SessionCreate,
            json!({"sessionId":SESSION}),
        )
        .await;
        rpc(&host,"follow",RpcMethod::SessionPrompt,json!({"sessionId":SESSION,"mode":"queue","content":[{"type":"text","text":"Register follow-up for the source snapshot job. There is no independent work while waiting for a checkpoint."}]})).await;
    } else {
        let report = host
            .restore_from_store_with_policy(store.clone(), RecoveryPolicy::PauseIncompleteTools)
            .await
            .unwrap();
        assert!(report.issues.is_empty(), "{report:?}");
    }
    // Bind before replay: a live signal arriving during replay is harmless and
    // no source checkpoint can be lost during the startup/live hand-off.
    reconcile(&root, &run, &host, store.as_ref()).await;
    println!("WATCH_READY {}", std::process::id());
    loop {
        let mut bytes = [0u8; 16];
        let read = socket.recv(&mut bytes).await.unwrap();
        match &bytes[..read] {
            b"wake" => {
                reconcile(&root, &run, &host, store.as_ref()).await;
                println!("WATCH_SCAN");
            }
            b"stop" => {
                rpc(
                    &host,
                    "stop",
                    RpcMethod::SessionCancel,
                    json!({"sessionId":SESSION}),
                )
                .await;
                println!("WATCH_STOPPED");
            }
            b"quit" => break,
            _ => panic!("unexpected experimental signal"),
        }
    }
    assert!(runtime.shutdown(Duration::from_secs(5)).await.is_graceful());
}
