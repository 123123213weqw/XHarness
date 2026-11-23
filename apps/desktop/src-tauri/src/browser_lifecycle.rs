//! Service-owned WebView lifecycle. The renderer only presents ready pages.
//! Host socket cancellation removes exactly its provisional page; UI selection
//! cannot cancel creation or publish a forged Ready/Closed acknowledgement.
use crate::browser::{self, BrowserState};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Mutex, time::Duration};
use tauri::{AppHandle, Emitter, Manager, State, Webview};
pub(crate) const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
#[derive(Default)]
pub struct BrowserLifecycle(Mutex<HashMap<String, String>>);
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlRequest {
    Status {},
    Open { url: String },
    Close {},
}
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlReply {
    Ready { tab_id: String },
    Failed {},
    Closed { tab_id: String },
}

pub fn descriptor(state: &BrowserState, owner: &str) -> Value {
    json!({"available":true,"bound":state.delegated_tab(owner).is_ok(),"version":2,
        "engine":"tauri-webview","capabilities":{
            "open":true,"close":true,"observe":true,"dom_actions":true,"browser_input_actions":["click","move","key"],"cross_origin_pointer":true,"global_os_input":false,
            "authenticated_requests":true,"response_read":true,"request_redirects":false,"native_input":true,"cross_origin_frames":false,"javascript":true,"async_javascript":true,"console_capture":true,
            "screenshot":true,"screenshot_scope":"system WebView viewport snapshot; accelerated media may be omitted","recording":false,"hidden_tab_actions":cfg!(target_os="linux"),"hidden_dom_actions":true,"hidden_native_input":cfg!(target_os="linux")
        },"constraints":["open/ready are service-owned; selecting a chat or hiding preview never grants/revokes control",
            "operations resolve the authenticated chat binding, not the selected sidebar; native background input needs platform acceptance",
            "evaluate executes arbitrary page JavaScript with the action grant; browser same-origin rules apply; timeout does not roll back effects",
            "perform uses DOM actions; input uses browser-owned native events, not global OS input",
            "cross-origin iframe pixels/pointer input supported; cross-origin DOM traversal and recording unsupported"]})
}

struct Guard {
    app: AppHandle,
    request_id: String,
    completed: bool,
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(state) = self.app.try_state::<BrowserLifecycle>() {
            if let Ok(mut pending) = state.0.lock() {
                pending.remove(&self.request_id);
            }
        }
        if !self.completed {
            let app = self.app.clone();
            let tab = format!("browser:{}", self.request_id);
            if let Some(state) = app.try_state::<BrowserState>() {
                if state.close_tab(&app, &tab).is_err() {
                    eprintln!("cancelled browser creation cleanup failed");
                }
            }
        }
    }
}
#[cfg(test)]
fn interrupted_receipt(started: bool) -> Value {
    json!({"ok":false,"effect":if started {"unknown"}else{"not_started"},"message":"browser creation was interrupted; inspect state before deciding; never replay an uncertain effect"})
}
impl BrowserLifecycle {
    // Kept for the ordinary UI navigation adapter; no renderer acknowledgement
    // participates in service-owned creation.
    pub(super) fn mark_started(&self, _: &str) {}
    pub async fn open(&self, app: &AppHandle, owner: &str, url: String) -> Result<Value, String> {
        let url = browser::web_url(&url)?.to_string();
        if owner.is_empty() || owner.len() > 128 || owner.chars().any(char::is_control) {
            return Err("invalid browser owner".into());
        }
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| "browser entropy unavailable")?;
        let request_id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        {
            let mut pending = self.0.lock().map_err(|_| "browser lifecycle unavailable")?;
            if pending.len() >= 4 || pending.values().any(|o| o == owner) {
                return Err("browser open already pending or capacity reached".into());
            }
            pending.insert(request_id.clone(), owner.into());
        }
        let tab_id = format!("browser:{request_id}");
        let mut guard = Guard {
            app: app.clone(),
            request_id,
            completed: false,
        };
        let _activity = crate::browser_activity::Activity::start(
            owner,
            &tab_id,
            crate::browser_activity::Operation::Open,
            |event| {
                if app
                    .emit_to("main", "xharness-browser-activity", event)
                    .is_err()
                {
                    eprintln!("browser activity notification failed");
                }
            },
        );
        let ready = tokio::time::timeout(OPEN_TIMEOUT, async {
            browser::open_owned(app, tab_id.clone(), url).await?;
            let state = app.state::<BrowserState>();
            loop {
                let (view, inspector) = state.inspection_target(&tab_id)?;
                let actual = view.url().map_err(|_| "native URL unavailable")?;
                if browser::web_url(actual.as_str()).is_ok() {
                    inspector.delegation.grant(owner.into(), &actual, true)?;
                    if let Ok(receipt) = state.loaded_receipt(owner, &tab_id) {
                        state.bind_owner(owner, &tab_id)?;
                        return Ok::<Value, String>(receipt);
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| "browser creation readiness timed out; provisional page cancelled")??;
        guard.completed = true;
        // Presentation delivery cannot roll back a page already opened.
        if app
            .emit_to(
                "main",
                "xharness-browser-owned-page",
                json!({"owner":owner,"tabId":tab_id,"url":ready["url"],"closed":false}),
            )
            .is_err()
        {
            eprintln!("browser ready presentation notification failed");
        }
        Ok(ready)
    }
    pub async fn close(&self, app: &AppHandle, owner: &str) -> Result<Value, String> {
        let tab_id = app.state::<BrowserState>().owned_tab(owner)?;
        let _activity = crate::browser_activity::Activity::start(
            owner,
            &tab_id,
            crate::browser_activity::Operation::Close,
            |event| {
                if app
                    .emit_to("main", "xharness-browser-activity", event)
                    .is_err()
                {
                    eprintln!("browser activity notification failed");
                }
            },
        );
        let main = app.get_webview("main").ok_or("main renderer unavailable")?;
        browser::desktop_browser_close(main, app.state(), tab_id.clone()).await?;
        let receipt = app.state::<BrowserState>().closed_receipt(&tab_id)?;
        if app
            .emit_to(
                "main",
                "xharness-browser-owned-page",
                json!({"owner":owner,"tabId":tab_id,"closed":true}),
            )
            .is_err()
        {
            eprintln!("browser close presentation notification failed");
        }
        Ok(receipt)
    }
}
/// Legacy renderer receipts cannot complete or cancel service-owned operations.
/// Keep the command's closed wire shape for installed UI compatibility.
#[tauri::command]
pub async fn desktop_browser_control_reply(
    app: AppHandle,
    caller: Webview,
    _state: State<'_, BrowserLifecycle>,
    request_id: String,
    reply: ControlReply,
) -> Result<bool, String> {
    browser::ensure_main(&caller)?;
    match reply {
        ControlReply::Ready { tab_id } | ControlReply::Closed { tab_id } => {
            let _ = tab_id;
        }
        ControlReply::Failed {} => {}
    }
    let _ = (app, request_id);
    Ok(false)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closing_an_admitted_navigation_is_not_reported_as_unstarted() {
        assert_eq!(interrupted_receipt(false)["effect"], "not_started");
        assert_eq!(interrupted_receipt(true)["effect"], "unknown");
    }
    #[test]
    fn lifecycle_contract_rejects_unknown_controls_and_fields() {
        assert!(serde_json::from_value::<ControlRequest>(json!({"action":"close"})).is_ok());
        assert!(serde_json::from_value::<ControlRequest>(
            json!({"action":"open","url":"https://example.com"})
        )
        .is_ok());
        for value in [
            json!({"action":"open","url":"x","owner":"other"}),
            json!({"action":"eval","script":"x"}),
            json!({"action":"close","tab_id":"x"}),
            json!({"action":"close","owner":"other"}),
            json!({"action":"status","tab_id":"x"}),
        ] {
            assert!(serde_json::from_value::<ControlRequest>(value).is_err());
        }
        assert!(serde_json::from_value::<ControlReply>(
            json!({"status":"ready","tab_id":"x","owner":"other"})
        )
        .is_err());
    }
    #[test]
    fn empty_browser_is_discoverable_with_precise_native_capability_limits() {
        let status = descriptor(&BrowserState::default(), "chat");
        assert_eq!(status["available"], true);
        assert_eq!(status["capabilities"]["native_input"], true);
        assert_eq!(status["capabilities"]["close"], true);
        assert_eq!(status["capabilities"]["screenshot"], true);
        assert_eq!(status["bound"], false);
        assert_eq!(
            status["capabilities"]["hidden_native_input"],
            cfg!(target_os = "linux")
        );
        assert_eq!(status["capabilities"]["hidden_dom_actions"], true);
        for capability in ["cross_origin_frames", "recording", "global_os_input"] {
            assert_eq!(status["capabilities"][capability], false);
        }
    }
}
