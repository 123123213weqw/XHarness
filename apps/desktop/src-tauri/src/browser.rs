//! Native, unprivileged child WebViews for the desktop workspace browser.
//! Only the `main` WebView has a Tauri capability; visited pages never inherit
//! its IPC permissions, even though they share the same native window.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::lifecycle_trace::browser as trace;
use serde::Serialize;
use tauri::webview::{DownloadEvent, NewWindowResponse, PageLoadEvent, WebviewBuilder};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, State, Webview, WebviewUrl,
};
use url::Url;
use xharness_diagnostics::BrowserEvent as TraceEvent;

#[path = "browser_viewport.rs"]
pub(super) mod viewport;

const MAX_LIVE_WEBVIEWS: usize = 16;
const MAX_TAB_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;

#[cfg(any(target_os = "macos", test))]
#[path = "browser_user_agent.rs"]
mod user_agent;

/// WebView2 does not deliver fragment changes through Wry's navigation policy.
/// Listen to the native Source property instead. This is only a resample hint:
/// guests still have no IPC permission, and no frame-provided URL is trusted.
#[cfg(windows)]
fn watch_native_location(
    child: &Webview,
    app: AppHandle,
    tab_id: String,
    inspector: Arc<crate::browser_inspect::Inspector>,
) -> Result<(), String> {
    child
        .with_webview(move |platform| {
            // SAFETY: with_webview runs on the WebView2 owning UI apartment.
            // Neither the controller nor view is moved to another thread.
            let registration = unsafe { platform.controller().CoreWebView2() }.and_then(|view| {
                let event_app = app.clone();
                let event_id = tab_id.clone();
                let callback =
                    webview2_com::SourceChangedEventHandler::create(Box::new(move |_, _| {
                        inspector.invalidate();
                        emit(&event_app, &event_id, "navigation-policy", "");
                        Ok(())
                    }));
                let mut token = 0;
                // SAFETY: the token pointer is valid during this synchronous
                // call. WebView2 retains the callback until the view closes.
                // The callback owns no COM view/controller (no reference cycle).
                unsafe { view.add_SourceChanged(&callback, &mut token) }
            });
            if registration.is_err() {
                // Queuing with_webview is not proof that COM registration
                // succeeded. Surface failure without exposing native details.
                eprintln!("browser native location observer registration failed");
                emit(
                    &app,
                    &tab_id,
                    "location-error",
                    "Native address updates are unavailable",
                );
            }
        })
        .map_err(|_| "could not schedule native address observer".to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl BrowserBounds {
    fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
            && self.x >= 0.0
            && self.y >= 0.0
            && self.width >= 1.0
            && self.height >= 1.0
            && self.width <= 10000.0
            && self.height <= 10000.0
    }

    fn rect(self) -> tauri::Rect {
        tauri::Rect {
            position: tauri::Position::Logical(LogicalPosition::new(self.x, self.y)),
            size: tauri::Size::Logical(LogicalSize::new(self.width, self.height)),
        }
    }
}

#[derive(Default)]
struct BrowserInner {
    tabs: HashMap<String, BrowserTab>,
    active: Option<String>,
    owner_tabs: HashMap<String, String>,
    bounds: Option<BrowserBounds>,
    presentation: Option<viewport::Presentation>,
    next_label: u64,
    clock: u64,
}

impl BrowserInner {
    fn detach_tab(&mut self, tab_id: &str) -> Option<BrowserTab> {
        if self.active.as_deref() == Some(tab_id) {
            self.active = None;
        }
        self.owner_tabs.retain(|_, id| id != tab_id);
        self.tabs.remove(tab_id)
    }
}

struct BrowserTab {
    webview: Webview,
    // Keep lifetime ownership even after site access is revoked or another
    // page becomes this chat's current action target.
    lifetime_owner: Option<String>,
    last_used: u64,
    inspector: Arc<crate::browser_inspect::Inspector>,
    loaded: Arc<AtomicBool>,
    applied_layout: Option<viewport::Presentation>,
}

#[derive(Default)]
pub struct BrowserState(Mutex<BrowserInner>);

impl BrowserState {
    pub(super) fn verify_viewport(
        &self,
        tab_id: &str,
        value: &serde_json::Value,
    ) -> Result<(), String> {
        let inner = self.0.lock().map_err(|_| "browser state unavailable")?;
        let tab = inner.tabs.get(tab_id).ok_or("browser tab closed")?;
        let layout = tab.applied_layout.ok_or("browser layout is not ready")?;
        layout.verify(
            value["width"].as_f64().ok_or("missing viewport width")?,
            value["height"].as_f64().ok_or("missing viewport height")?,
        )
    }

    async fn apply_layout(&self) -> Result<(), String> {
        let target = {
            let inner = self.0.lock().map_err(|_| "browser state unavailable")?;
            inner
                .active
                .as_ref()
                .zip(inner.presentation)
                .and_then(|(id, layout)| {
                    inner.tabs.get(id).map(|tab| {
                        (
                            id.clone(),
                            tab.webview.clone(),
                            Arc::clone(&tab.inspector),
                            layout,
                        )
                    })
                })
        };
        let Some((id, view, inspector, requested)) = target else {
            return Ok(());
        };
        let _gate = tokio::time::timeout(std::time::Duration::from_secs(8), inspector.gate.lock())
            .await
            .map_err(|_| "browser layout is busy")?;
        {
            let mut inner = self.0.lock().map_err(|_| "browser state unavailable")?;
            if inner.active.as_deref() != Some(&id) || inner.presentation != Some(requested) {
                return Err("browser layout changed while fitting".into());
            }
            if let Some(cached) = inner
                .tabs
                .get(&id)
                .and_then(|tab| tab.applied_layout)
                .filter(|layout| {
                    layout.pane == requested.pane && layout.viewport == requested.viewport
                })
            {
                inner.bounds = Some(cached.bounds);
                return Ok(());
            }
        }
        inspector.invalidate();
        let fitted: Result<viewport::Presentation, String> = async {
            let mut layout = requested;
            // Moving a WK child can itself change its automatic titlebar inset.
            // Measure AFTER applying each rectangle, with bounded calibration;
            // never compensate using geometry from the previous sidebar position.
            for _ in 0..4 {
                {
                    let inner = self.0.lock().map_err(|_| "browser state unavailable")?;
                    if inner.active.as_deref() != Some(&id) || inner.presentation != Some(requested)
                    {
                        return Err("browser layout changed while fitting".into());
                    }
                }
                view.set_bounds(layout.bounds.rect())
                    .and_then(|()| view.set_zoom(layout.zoom))
                    .map_err(|error| error.to_string())?;
                crate::browser_native::layout(
                    &view,
                    (
                        layout.bounds.x,
                        layout.bounds.y,
                        layout.bounds.width,
                        layout.bounds.height,
                    ),
                )
                .await?;
                let inset = crate::browser_native::content_inset(&view).await?;
                let next = requested.with_inset(inset)?;
                if next == layout {
                    return Ok(layout);
                }
                layout = next;
            }
            Err("native browser content geometry did not settle".into())
        }
        .await;
        let layout = match fitted {
            Ok(layout) => layout,
            Err(error) => {
                let _ = view.hide();
                return Err(error);
            }
        };
        let mut inner = self.0.lock().map_err(|_| "browser state unavailable")?;
        if inner.active.as_deref() != Some(&id) || inner.presentation != Some(requested) {
            let _ = view.hide();
            return Err("browser layout changed while fitting".into());
        }
        inner.bounds = Some(layout.bounds);
        if let Some(tab) = inner.tabs.get_mut(&id) {
            tab.applied_layout = Some(layout);
        }
        Ok(())
    }

    pub(super) fn closed_receipt(&self, tab_id: &str) -> Result<serde_json::Value, String> {
        let inner = self.0.lock().map_err(|_| "browser state unavailable")?;
        if inner.tabs.contains_key(tab_id) || inner.active.as_deref() == Some(tab_id) {
            return Err("browser tab is not closed".into());
        }
        Ok(serde_json::json!({"ok":true,"effect":"applied","state":"closed","tab_id":tab_id}))
    }
    pub(super) fn inspection_target(
        &self,
        tab_id: &str,
    ) -> Result<(Webview, Arc<crate::browser_inspect::Inspector>), String> {
        if !valid_tab_id(tab_id) {
            return Err("invalid browser tab".into());
        }
        let inner = self.0.lock().map_err(|_| "browser state unavailable")?;
        let tab = inner.tabs.get(tab_id).ok_or("browser tab is not open")?;
        if tab.applied_layout.is_none() {
            return Err("browser page layout is not ready".into());
        }
        Ok((tab.webview.clone(), Arc::clone(&tab.inspector)))
    }
    pub(super) fn loaded_receipt(
        &self,
        owner: &str,
        tab_id: &str,
    ) -> Result<serde_json::Value, String> {
        // Native getters can rendezvous with the UI thread. Never hold the
        // registry mutex while waiting for one: the UI may be validating input.
        let (view, inspector, loaded) = {
            let inner = self.0.lock().map_err(|_| "browser state unavailable")?;
            let tab = inner.tabs.get(tab_id).ok_or("browser tab closed")?;
            if tab.applied_layout.is_none() || !tab.loaded.load(Ordering::SeqCst) {
                return Err("browser page is not ready".into());
            }
            (
                tab.webview.clone(),
                Arc::clone(&tab.inspector),
                Arc::clone(&tab.loaded),
            )
        };
        let epoch = inspector.navigation_epoch.load(Ordering::SeqCst);
        let url = view.url().map_err(|_| "browser URL unavailable")?;
        let (current, other) = self.inspection_target(tab_id)?;
        if current.label() != view.label()
            || !Arc::ptr_eq(&inspector, &other)
            || !loaded.load(Ordering::SeqCst)
            || inspector.navigation_epoch.load(Ordering::SeqCst) != epoch
            || !inspector.delegation.permits(owner, &url, false)
        {
            return Err("browser binding changed".into());
        }
        Ok(
            serde_json::json!({"ok":true,"effect":"applied","state":"ready","tab_id":tab_id,"url":url.to_string()}),
        )
    }
    pub(super) fn bind_owner(&self, owner: &str, tab_id: &str) -> Result<(), String> {
        let mut inner = self.0.lock().map_err(|_| "browser state unavailable")?;
        let tab = inner
            .tabs
            .get_mut(tab_id)
            .ok_or("browser tab is not open")?;
        if !tab.inspector.delegation.bound(owner) {
            return Err("browser ownership was not granted".into());
        }
        tab.lifetime_owner = Some(owner.into());
        // Explicit revoke + rebind transfers lifetime ownership too. A former
        // owner must not retain the ability to close the new owner's page.
        inner
            .owner_tabs
            .retain(|bound, id| id != tab_id || bound == owner);
        if let Some(previous) = inner.owner_tabs.insert(owner.into(), tab_id.into()) {
            if previous != tab_id {
                if let Some(old) = inner.tabs.get(&previous) {
                    old.inspector.invalidate();
                }
            }
        }
        Ok(())
    }
    /// Lifetime ownership permits cleanup after a load failure or site grant
    /// revocation. It does not authorize DOM, input, screenshots or HTTP.
    pub(super) fn owned_tab(&self, owner: &str) -> Result<String, String> {
        let inner = self.0.lock().map_err(|_| "browser state unavailable")?;
        let id = inner
            .owner_tabs
            .get(owner)
            .ok_or("no browser is owned by this chat")?;
        if !inner.tabs.contains_key(id) {
            return Err("browser tab is not open".into());
        }
        Ok(id.clone())
    }
    pub(super) fn delegated_tab(&self, owner: &str) -> Result<String, String> {
        let (id, view, inspector) = {
            let inner = self.0.lock().map_err(|_| "browser state unavailable")?;
            let id = inner
                .owner_tabs
                .get(owner)
                .ok_or("no browser is bound to this chat")?;
            let tab = inner.tabs.get(id).ok_or("browser tab is not open")?;
            (id.clone(), tab.webview.clone(), Arc::clone(&tab.inspector))
        };
        let url = view.url().map_err(|_| "browser URL unavailable")?;
        let expired = inspector.delegation.renew_host(owner, &url)?;
        if expired {
            inspector.invalidate();
        }
        self.loaded_receipt(owner, &id)?;
        Ok(id)
    }
    /// Main-thread dispatch guard must never wait for a registry lock held by
    /// a worker awaiting native UI creation/getters. A busy registry rejects
    /// dispatch rather than freezing the event loop. No native call under lock.
    pub(super) fn native_dispatch_target(
        &self,
        label: &str,
        inspector: &Arc<crate::browser_inspect::Inspector>,
        visual: bool,
    ) -> Result<(), String> {
        let inner = self
            .0
            .try_lock()
            .map_err(|_| "browser state busy before native dispatch")?;
        let (tab_id, tab) = inner
            .tabs
            .iter()
            .find(|(_, tab)| tab.webview.label() == label && Arc::ptr_eq(inspector, &tab.inspector))
            .ok_or("native browser closed")?;
        // Only GTK has a verified offscreen render host. This does not limit
        // background DOM calls, which never use the native visual dispatcher.
        if visual && !cfg!(target_os = "linux") && inner.active.as_ref() != Some(tab_id) {
            return Err("background native visual actions are unavailable on this platform".into());
        }
        if tab.applied_layout.is_none()
            || !tab.loaded.load(Ordering::SeqCst)
            || tab.webview.label() != label
            || !Arc::ptr_eq(inspector, &tab.inspector)
        {
            return Err("browser binding changed before native dispatch".into());
        }
        Ok(())
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserEvent {
    tab_id: String,
    kind: &'static str,
    value: String,
}

fn emit(app: &AppHandle, tab_id: &str, kind: &'static str, value: impl Into<String>) {
    let _ = app.emit_to(
        "main",
        "xharness-browser-event",
        BrowserEvent {
            tab_id: tab_id.to_owned(),
            kind,
            value: value.into(),
        },
    );
}

#[derive(Serialize)]
pub struct BrowserPageState {
    url: String,
    loaded: bool,
}

/// Frame-agnostic navigation callbacks are hints, never address authority.
/// Only the trusted shell may sample the native top-level location/readiness.
#[tauri::command]
pub async fn desktop_browser_page_state(
    caller: Webview,
    state: State<'_, BrowserState>,
    tab_id: String,
) -> Result<BrowserPageState, String> {
    ensure_main(&caller)?;
    let inner = state.0.lock().map_err(|_| "browser state unavailable")?;
    if inner.active.as_deref() != Some(&tab_id) || inner.bounds.is_none() {
        return Err("only the active, laid-out browser tab can be sampled".into());
    }
    let tab = inner.tabs.get(&tab_id).ok_or("browser tab is not open")?;
    Ok(BrowserPageState {
        url: tab
            .webview
            .url()
            .map_err(|_| "browser URL unavailable")?
            .to_string(),
        loaded: tab.loaded.load(Ordering::SeqCst),
    })
}

pub(super) fn ensure_main(webview: &Webview) -> Result<(), String> {
    if webview.label() == "main" {
        Ok(())
    } else {
        Err("browser controls are available only to the XHarness UI".into())
    }
}

fn valid_tab_id(tab_id: &str) -> bool {
    !tab_id.is_empty()
        && tab_id.len() <= 64
        && tab_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_'))
}

pub(super) use xharness_browser::web_url;

fn safe_download_name(url: &Url) -> String {
    let raw = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|name| !name.is_empty())
        .unwrap_or("download");
    let name: String = raw
        .chars()
        .take(96)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if name == "." || name == ".." || name.is_empty() {
        "download".into()
    } else {
        name
    }
}

fn tab_snapshot_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|error| error.to_string())?
        .join("browser-tabs.json"))
}

fn validate_tab_snapshot(snapshot: &str) -> Result<(), String> {
    if snapshot.len() > MAX_TAB_SNAPSHOT_BYTES {
        return Err("browser tab snapshot is too large".into());
    }
    if !serde_json::from_str::<serde_json::Value>(snapshot)
        .map_err(|error| error.to_string())?
        .is_object()
    {
        return Err("browser tab snapshot must be a JSON object".into());
    }
    Ok(())
}

/// Restore tab metadata from the app's stable config path, not origin-scoped
/// localStorage (the authenticated Host selects a fresh loopback port).
#[tauri::command]
pub async fn desktop_browser_restore(
    app: AppHandle,
    caller: Webview,
) -> Result<Option<String>, String> {
    ensure_main(&caller)?;
    let path = tab_snapshot_path(&app)?;
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.len() > MAX_TAB_SNAPSHOT_BYTES as u64 => {
            return Err("browser tab snapshot is too large".into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        _ => {}
    }
    let snapshot = match std::fs::read_to_string(path) {
        Ok(snapshot) => snapshot,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    validate_tab_snapshot(&snapshot)?;
    Ok(Some(snapshot))
}

#[tauri::command]
pub async fn desktop_browser_persist(
    app: AppHandle,
    caller: Webview,
    snapshot: String,
) -> Result<(), String> {
    ensure_main(&caller)?;
    validate_tab_snapshot(&snapshot)?;
    let path = tab_snapshot_path(&app)?;
    let parent = path.parent().ok_or("invalid browser tab snapshot path")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temp.write_all(snapshot.as_bytes())
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    temp.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

/// Create a tab on first navigation; reuse its native WebView afterwards.
/// This command is async so `add_child` never blocks the UI event thread.
#[tauri::command]
pub async fn desktop_browser_navigate(
    app: AppHandle,
    caller: Webview,
    state: State<'_, BrowserState>,
    tab_id: String,
    url: String,
) -> Result<(), String> {
    ensure_main(&caller)?;
    navigate_page(app, caller, &state, tab_id, url, None).await
}

/// Creation/readiness are service-owned; no pane activation or UI receipt.
pub(super) async fn open_owned(
    app: &AppHandle,
    tab_id: String,
    url: String,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let caller = app
        .get_webview("main")
        .ok_or("main renderer is unavailable")?;
    let state = app.state::<BrowserState>();
    navigate_page(app.clone(), caller, &state, tab_id, url, Some(cancelled)).await
}

fn background_layout() -> Result<viewport::Presentation, String> {
    viewport::Presentation::new(
        BrowserBounds {
            x: 0.0,
            y: 0.0,
            width: 1280.0,
            height: 720.0,
        },
        Some(viewport::Viewport {
            width: 1280,
            height: 720,
        }),
    )
}

async fn navigate_page(
    app: AppHandle,
    caller: Webview,
    state: &BrowserState,
    tab_id: String,
    url: String,
    cancelled: Option<&AtomicBool>,
) -> Result<(), String> {
    let owned = cancelled.is_some();
    #[cfg(not(target_os = "macos"))]
    let _ = &caller;
    if !valid_tab_id(&tab_id) {
        return Err("invalid browser tab".into());
    }
    let target = web_url(&url)?;
    trace(&app, TraceEvent::Navigate, None);
    // Complete WKWebView's identity before the very first guest HTTP request.
    // Failure keeps the system default; it must not block ordinary navigation.
    #[cfg(target_os = "macos")]
    let user_agent = user_agent::for_macos(&caller).await;
    {
        let mut inner = state.0.lock().map_err(|_| "browser state unavailable")?;
        // Retirement signals cancellation before detaching under this same
        // registry lock, so a provisional tab cannot be inserted behind it.
        if cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Err("browser creation cancelled during session retirement".into());
        }
        if inner.tabs.contains_key(&tab_id) {
            inner.clock += 1;
            let clock = inner.clock;
            let tab = inner.tabs.get_mut(&tab_id).expect("tab was present");
            tab.last_used = clock;
            // Explicit UI navigation is a top-level intent. Invalidate before it
            // enters native dispatch, even if the old document is still visible.
            tab.inspector.revoke();
            tab.loaded.store(false, Ordering::SeqCst);
            return tab
                .webview
                .navigate(target)
                .map_err(|error| error.to_string());
        }
        if inner.tabs.len() >= MAX_LIVE_WEBVIEWS {
            // Suspend the least recently used hidden page, not its persisted URL.
            // Selecting that tab later creates a fresh WebView on demand.
            let oldest = inner
                .tabs
                .iter()
                .filter(|(id, _)| {
                    inner.active.as_deref() != Some(id.as_str())
                        && !inner.owner_tabs.values().any(|bound| bound == *id)
                })
                .min_by_key(|(_, tab)| tab.last_used)
                .map(|(id, _)| id.clone())
                .ok_or("no inactive browser tab can be suspended")?;
            if let Some(tab) = inner.tabs.remove(&oldest) {
                tab.inspector.revoke();
                trace(&app, TraceEvent::Evicted, Some(tab.webview.label()));
                tab.webview.close().map_err(|error| error.to_string())?;
                emit(&app, &oldest, "suspended", "");
            }
        }
        let bounds = if owned {
            background_layout()?.bounds
        } else {
            inner.bounds.ok_or("browser pane is not laid out yet")?
        };
        if !bounds.valid() {
            return Err("invalid browser pane bounds".into());
        }
        let window = app.get_window("main").ok_or("main window is unavailable")?;
        let browser_data = app
            .path()
            .app_local_data_dir()
            .map_err(|error| error.to_string())?
            .join("browser-webviews");
        std::fs::create_dir_all(&browser_data).map_err(|error| error.to_string())?;
        inner.next_label += 1;
        let label = format!("browser-{}", inner.next_label);
        let load_label = label.clone();
        let event_app = app.clone();
        let event_id = tab_id.clone();
        let title_app = app.clone();
        let title_id = tab_id.clone();
        let load_app = app.clone();
        let load_id = tab_id.clone();
        let popup_app = app.clone();
        let popup_id = tab_id.clone();
        let download_app = app.clone();
        let download_id = tab_id.clone();
        let inspector = Arc::new(crate::browser_inspect::Inspector::default());
        let navigation_inspector = Arc::clone(&inspector);
        let load_inspector = Arc::clone(&inspector);
        let loaded = Arc::new(AtomicBool::new(false));
        let load_ready = Arc::clone(&loaded);
        let builder = WebviewBuilder::new(label, WebviewUrl::External(target))
            .data_directory(browser_data)
            .on_navigation(move |url| {
                if matches!(url.scheme(), "http" | "https") {
                    // Wry 0.55 also calls this policy for subframe navigations on
                    // WKWebView. It provides no main-frame bit. Do not publish its
                    // URL, reset top-level readiness or revoke the page's origin
                    // binding here. Still invalidate action refs conservatively.
                    navigation_inspector.invalidate();
                    // Hash/history navigations need not emit a page-load callback.
                    // Ask the UI to sample WebView.url(), never this frame's URL.
                    emit(&event_app, &event_id, "navigation-policy", "");
                    true
                } else {
                    emit(&event_app, &event_id, "blocked-url", url.as_str());
                    false
                }
            })
            .on_document_title_changed(move |_, title| {
                emit(&title_app, &title_id, "title", title);
            })
            .on_page_load(move |_, payload| {
                load_ready.store(
                    matches!(payload.event(), PageLoadEvent::Finished),
                    Ordering::SeqCst,
                );
                let status = match payload.event() {
                    PageLoadEvent::Started => {
                        trace(&load_app, TraceEvent::Loading, Some(&load_label));
                        load_inspector.delegation.navigate(payload.url());
                        load_inspector.invalidate();
                        "loading"
                    }
                    PageLoadEvent::Finished => {
                        trace(&load_app, TraceEvent::Loaded, Some(&load_label));
                        load_inspector.delegation.navigate(payload.url());
                        "loaded"
                    }
                };
                // Page-load callbacks carry the native top-level URL, unlike the
                // navigation policy. Publish it before readiness so redirects are
                // synchronized before the UI attempts an exact-origin binding.
                emit(&load_app, &load_id, "url", payload.url().as_str());
                emit(&load_app, &load_id, status, payload.url().as_str());
            })
            .on_new_window(move |url, _| {
                if web_url(url.as_str()).is_ok() {
                    emit(&popup_app, &popup_id, "popup", url.as_str());
                }
                NewWindowResponse::Deny
            })
            .on_download(move |_, event| match event {
                DownloadEvent::Requested { url, destination } => {
                    let Ok(directory) = download_app.path().download_dir() else {
                        emit(
                            &download_app,
                            &download_id,
                            "download-error",
                            "Downloads folder is unavailable",
                        );
                        return false;
                    };
                    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
                        return false;
                    };
                    let name = safe_download_name(&url);
                    *destination = directory.join(format!("xh-{}-{name}", now.as_nanos()));
                    emit(
                        &download_app,
                        &download_id,
                        "download-start",
                        destination.display().to_string(),
                    );
                    true
                }
                DownloadEvent::Finished { path, success, .. } => {
                    let kind = if success {
                        "download-complete"
                    } else {
                        "download-error"
                    };
                    emit(
                        &download_app,
                        &download_id,
                        kind,
                        path.map(|path| path.display().to_string())
                            .unwrap_or_default(),
                    );
                    true
                }
                _ => true,
            });
        #[cfg(target_os = "macos")]
        let builder = match user_agent {
            Some(identity) => builder.user_agent(identity),
            None => builder,
        };
        let child = window
            .add_child(
                builder,
                LogicalPosition::new(bounds.x, bounds.y),
                LogicalSize::new(bounds.width, bounds.height),
            )
            .map_err(|error| error.to_string())?;
        trace(&app, TraceEvent::Created, Some(child.label()));
        #[cfg(windows)]
        if let Err(error) =
            watch_native_location(&child, app.clone(), tab_id.clone(), Arc::clone(&inspector))
        {
            let _ = child.close();
            return Err(error);
        }
        // Fit/reparent before showing a new guest. In particular, a GTK box
        // child must not remain packed beside the main renderer.
        child.hide().map_err(|error| error.to_string())?;
        inner.clock += 1;
        let last_used = inner.clock;
        inner.tabs.insert(
            tab_id.clone(),
            BrowserTab {
                webview: child,
                lifetime_owner: None,
                last_used,
                inspector,
                loaded,
                applied_layout: None,
            },
        );
    }
    if owned {
        let view = {
            let inner = state.0.lock().map_err(|_| "browser state unavailable")?;
            inner
                .tabs
                .get(&tab_id)
                .ok_or("browser creation was cancelled")?
                .webview
                .clone()
        };
        let requested = background_layout()?;
        view.set_zoom(requested.zoom).map_err(|e| e.to_string())?;
        crate::browser_native::layout(
            &view,
            (
                requested.bounds.x,
                requested.bounds.y,
                requested.bounds.width,
                requested.bounds.height,
            ),
        )
        .await?;
        let layout = requested.with_inset(crate::browser_native::content_inset(&view).await?)?;
        view.set_bounds(layout.bounds.rect())
            .map_err(|e| e.to_string())?;
        view.set_zoom(layout.zoom).map_err(|e| e.to_string())?;
        crate::browser_native::layout(
            &view,
            (
                layout.bounds.x,
                layout.bounds.y,
                layout.bounds.width,
                layout.bounds.height,
            ),
        )
        .await?;
        crate::browser_native::present(&view, false).await?;
        let mut inner = state.0.lock().map_err(|_| "browser state unavailable")?;
        inner
            .tabs
            .get_mut(&tab_id)
            .ok_or("browser creation was cancelled")?
            .applied_layout = Some(layout);
        return Ok(());
    }
    state.apply_layout().await?;
    let inner = state.0.lock().map_err(|_| "browser state unavailable")?;
    if inner.active.as_deref() == Some(&tab_id) {
        if let Some(tab) = inner.tabs.get(&tab_id) {
            tab.webview.show().map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// Select a tab. An empty ID hides all child WebViews (settings, modal, etc.).
#[tauri::command]
pub async fn desktop_browser_activate(
    caller: Webview,
    state: State<'_, BrowserState>,
    tab_id: Option<String>,
) -> Result<bool, String> {
    ensure_main(&caller)?;
    if tab_id.as_deref().is_some_and(|id| !valid_tab_id(id)) {
        return Err("invalid browser tab".into());
    }
    let requested_tab = tab_id.clone();
    let (active_view, hidden_views) = {
        let mut inner = state.0.lock().map_err(|_| "browser state unavailable")?;
        if inner.active != tab_id {
            trace(
                caller.app_handle(),
                if tab_id.is_some() {
                    TraceEvent::Activate
                } else {
                    TraceEvent::Hidden
                },
                None,
            );
            // Presentation changes never revoke another chat's control lease.
            // apply_layout invalidates only the page whose geometry changes.
        }
        inner.clock += 1;
        let clock = inner.clock;
        inner.active = tab_id;
        let active = inner.active.clone();
        if let Some(id) = active.as_deref() {
            if let Some(tab) = inner.tabs.get_mut(id) {
                tab.last_used = clock;
            }
        }
        // Bounds may have changed while this tab was hidden. The frontend lays
        // out the pane before selecting its tab; updating only the previously
        // active view leaves a reselected view at its old rectangle. Hide the old
        // views first, then apply the current logical rectangle before showing.
        let hidden_views = inner
            .tabs
            .iter()
            .filter(|(id, _)| {
                inner.active.as_deref() != Some(id.as_str()) || inner.bounds.is_none()
            })
            .map(|(_, tab)| tab.webview.clone())
            .collect::<Vec<_>>();
        let active_view = inner
            .active
            .as_ref()
            .and_then(|id| inner.tabs.get(id))
            .map(|tab| tab.webview.clone());
        (active_view, hidden_views)
    };
    for view in hidden_views {
        crate::browser_native::present(&view, false).await?;
    }
    if let Some(view) = &active_view {
        crate::browser_native::present(view, true).await?;
    }
    state.apply_layout().await?;
    if let Some(view) = &active_view {
        let inner = state.0.lock().map_err(|_| "browser state unavailable")?;
        if inner.active != requested_tab {
            return Err("browser tab changed before presentation".into());
        }
        view.show().map_err(|error| error.to_string())?;
    }
    Ok(active_view.is_some())
}

#[tauri::command]
pub async fn desktop_browser_bounds(
    caller: Webview,
    state: State<'_, BrowserState>,
    bounds: BrowserBounds,
    viewport: Option<viewport::Viewport>,
) -> Result<(), String> {
    ensure_main(&caller)?;
    let layout = viewport::Presentation::new(bounds, viewport)?;
    {
        let mut inner = state.0.lock().map_err(|_| "browser state unavailable")?;
        inner.presentation = Some(layout);
        inner.bounds = Some(layout.bounds);
    }
    state.apply_layout().await
}

#[tauri::command]
pub async fn desktop_browser_action(
    caller: Webview,
    state: State<'_, BrowserState>,
    tab_id: String,
    action: String,
) -> Result<(), String> {
    ensure_main(&caller)?;
    let inner = state.0.lock().map_err(|_| "browser state unavailable")?;
    let tab = inner.tabs.get(&tab_id).ok_or("browser tab is not open")?;
    let event = match action.as_str() {
        "back" => TraceEvent::Back,
        "forward" => TraceEvent::Forward,
        "reload" => TraceEvent::Reload,
        "stop" => TraceEvent::Stop,
        _ => return Err("unknown browser action".into()),
    };
    trace(caller.app_handle(), event, Some(tab.webview.label()));
    match action.as_str() {
        "back" => tab.webview.eval("history.back()"),
        "forward" => tab.webview.eval("history.forward()"),
        "reload" => tab.webview.reload(),
        "stop" => tab.webview.eval("window.stop()"),
        _ => return Err("unknown browser action".into()),
    }
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn desktop_browser_close(
    caller: Webview,
    state: State<'_, BrowserState>,
    tab_id: String,
) -> Result<(), String> {
    ensure_main(&caller)?;
    if !valid_tab_id(&tab_id) {
        return Err("invalid browser tab".into());
    }
    state.close_tab(caller.app_handle(), &tab_id)
}
impl BrowserState {
    pub(super) fn close_tab(&self, app: &AppHandle, tab_id: &str) -> Result<(), String> {
        let tab = self
            .0
            .lock()
            .map_err(|_| "browser state unavailable")?
            .detach_tab(tab_id);
        if let Some(tab) = tab {
            destroy_tab(app, tab)?;
        }
        Ok(())
    }

    pub(super) fn close_tab_for_owner(
        &self,
        app: &AppHandle,
        owner: &str,
        tab_id: &str,
    ) -> Result<(), String> {
        let tab = {
            let mut inner = self.0.lock().map_err(|_| "browser state unavailable")?;
            if inner.owner_tabs.get(owner).map(String::as_str) != Some(tab_id) {
                return Err("browser ownership changed before close".into());
            }
            inner.detach_tab(tab_id).ok_or("browser tab is not open")?
        };
        destroy_tab(app, tab)
    }

    pub(super) fn retire_owned(
        &self,
        app: &AppHandle,
        owner: &str,
        provisional: &[String],
    ) -> Result<(), String> {
        let tabs = {
            let mut inner = self.0.lock().map_err(|_| "browser state unavailable")?;
            let ids: Vec<_> = inner
                .tabs
                .iter()
                .filter(|(id, tab)| match tab.lifetime_owner.as_deref() {
                    Some(current) => current == owner,
                    None => provisional.contains(id),
                })
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| inner.detach_tab(&id).map(|tab| (id, tab)))
                .collect::<Vec<_>>()
        };
        let mut failed = false;
        for (id, tab) in tabs {
            let was_ready = tab.lifetime_owner.is_some();
            match destroy_tab(app, tab) {
                Ok(()) if was_ready => crate::browser_lifecycle::notify_closed(app, owner, &id),
                Ok(()) => {}
                Err(_) => failed = true,
            }
        }
        if failed {
            return Err("native WebView cleanup not confirmed during session retirement".into());
        }
        Ok(())
    }
}

fn destroy_tab(app: &AppHandle, tab: BrowserTab) -> Result<(), String> {
    // Native destruction can rendezvous with the UI. The registry is already
    // unlocked, and its exact tab has been removed before dispatch.
    tab.inspector.revoke();
    trace(app, TraceEvent::Close, Some(tab.webview.label()));
    let result = tab.webview.close();
    trace(
        app,
        if result.is_ok() {
            TraceEvent::CloseAccepted
        } else {
            TraceEvent::CloseFailed
        },
        Some(tab.webview.label()),
    );
    result.map_err(|error| error.to_string())
}

pub fn close_all(app: &AppHandle) {
    let Some(state) = app.try_state::<BrowserState>() else {
        return;
    };
    let tabs = if let Ok(mut inner) = state.0.lock() {
        inner.active = None;
        inner.owner_tabs.clear();
        inner.tabs.drain().map(|(_, tab)| tab).collect::<Vec<_>>()
    } else {
        return;
    };
    trace(app, TraceEvent::ClearAll, None);
    for tab in tabs {
        tab.inspector.revoke();
        trace(app, TraceEvent::Close, Some(tab.webview.label()));
        let result = tab.webview.close();
        trace(
            app,
            if result.is_ok() {
                TraceEvent::CloseAccepted
            } else {
                TraceEvent::CloseFailed
            },
            Some(tab.webview.label()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{safe_download_name, valid_tab_id, validate_tab_snapshot, web_url, BrowserBounds};

    #[test]
    fn main_thread_native_guard_never_waits_for_worker_registry_lock() {
        use super::{Arc, BrowserState};
        let state = Arc::new(BrowserState::default());
        let held = state.0.lock().unwrap();
        let copy = state.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = sender.send(copy.native_dispatch_target(
                "unused",
                &Arc::new(crate::browser_inspect::Inspector::default()),
                false,
            ));
        });
        let result = receiver.recv_timeout(std::time::Duration::from_millis(200));
        drop(held);
        worker.join().unwrap();
        assert!(result.unwrap().unwrap_err().contains("state busy"));
    }

    #[test]
    fn only_web_pages_and_safe_tab_ids() {
        assert!(web_url("https://example.com/path").is_ok());
        assert!(web_url("http://localhost:3090/").is_ok());
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
            "https://u:p@example.com",
        ] {
            assert!(web_url(url).is_err(), "{url}");
        }
        assert!(valid_tab_id("browser:12"));
        assert!(!valid_tab_id("../../main"));
    }

    #[test]
    fn rejects_invalid_bounds() {
        assert!(BrowserBounds {
            x: 10.0,
            y: 10.0,
            width: 500.0,
            height: 300.0
        }
        .valid());
        assert!(!BrowserBounds {
            x: f64::NAN,
            y: 0.0,
            width: 500.0,
            height: 300.0
        }
        .valid());
        assert!(!BrowserBounds {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 300.0
        }
        .valid());
    }

    #[test]
    fn download_filename_cannot_escape_download_directory() {
        let url = url::Url::parse("https://example.com/path/evil%2Fname.zip").unwrap();
        assert!(!safe_download_name(&url).contains('/'));
    }

    #[test]
    fn persisted_tabs_require_a_bounded_json_object() {
        assert!(validate_tab_snapshot(r#"{"session":{"items":[]}}"#).is_ok());
        for bad in ["[]", "null", "not json"] {
            assert!(validate_tab_snapshot(bad).is_err());
        }
        assert!(validate_tab_snapshot(&"x".repeat(super::MAX_TAB_SNAPSHOT_BYTES + 1)).is_err());
    }
}
