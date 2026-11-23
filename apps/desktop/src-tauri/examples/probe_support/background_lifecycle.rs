//! Isolated production-bridge acceptance. No renderer Ready adapter exists.
use super::*;
use base64::Engine;

async fn call(
    connection: &browser_bridge::Connection,
    owner: &str,
    op: &str,
    args: Value,
) -> Result<Value, String> {
    let value = bridge_call(connection, owner, op, args).await?;
    if value["ok"] != true {
        return Err(format!("background {op} failed: {value}"));
    }
    Ok(value["result"].clone())
}
async fn dom(
    connection: &browser_bridge::Connection,
    owner: &str,
    label: &str,
    args: Value,
) -> Result<(), String> {
    let snapshot = call(connection, owner, "observe", json!({})).await?;
    let request =
        serde_json::to_value(action(&snapshot, label, args)?).map_err(|e| e.to_string())?;
    let receipt = call(connection, owner, "perform", request).await?;
    if receipt["effect"] != "applied" {
        return Err(format!("background DOM effect absent: {receipt}"));
    }
    Ok(())
}
async fn state(app: &tauri::AppHandle, id: &str) -> Result<Value, String> {
    let (view, _) = app.state::<browser::BrowserState>().inspection_target(id)?;
    evidence(&view,"({value:document.querySelector('#answer').value,status:document.querySelector('#status').textContent,events:probeEvents,visibility:document.visibilityState,width:innerWidth,height:innerHeight})").await
}
async fn screenshot(
    connection: &browser_bridge::Connection,
    owner: &str,
    name: &str,
) -> Result<Value, String> {
    let capture = call(connection, owner, "screenshot", json!({})).await?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(
            capture["png_base64"]
                .as_str()
                .ok_or("missing background PNG")?,
        )
        .map_err(|e| e.to_string())?;
    if png.len() < 1024 || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("invalid background snapshot".into());
    }
    if let Some(root) = std::env::var_os("XHARNESS_NATIVE_API_EVIDENCE") {
        let directory = std::path::PathBuf::from(root);
        std::fs::create_dir_all(&directory)
            .map_err(|_| "background evidence directory unavailable")?;
        std::fs::write(directory.join(format!("background-{name}.png")), &png)
            .map_err(|_| "background evidence write failed")?;
    }
    Ok(capture)
}

fn slow_fixture() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    // Exercise real navigation beyond the original five-second window, not
    // the retired renderer acknowledgement adapter.
    let delay_ms = if std::env::var("XHARNESS_NATIVE_PROBE_READY_DELAY_MS").as_deref() == Ok("6000")
    {
        6000
    } else {
        2000
    };
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut bytes = [0; 4096];
                let _ = stream.read(&mut bytes);
                std::thread::sleep(Duration::from_millis(delay_ms));
                let _=write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",HTML.len(),HTML);
            });
        }
    });
    format!("http://{address}/")
}
async fn creation_races(
    app: &tauri::AppHandle,
    connection: &browser_bridge::Connection,
    bid: &str,
) -> Result<(), String> {
    let main = app.get_webview("main").ok_or("main absent")?;
    let slow = slow_fixture();
    let opening = call(
        connection,
        "slow-owner",
        "control",
        json!({"action":"open","url":slow}),
    );
    let switching = async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        browser::desktop_browser_activate(main.clone(), app.state(), Some(bid.to_owned())).await?;
        let forged = browser_lifecycle::desktop_browser_control_reply(
            app.clone(),
            main.clone(),
            app.state(),
            "0".repeat(32),
            browser_lifecycle::ControlReply::Failed {},
        )
        .await?;
        if forged {
            return Err("renderer cancelled service-owned creation".into());
        }
        browser::desktop_browser_activate(main.clone(), app.state(), None).await?;
        Ok::<(), String>(())
    };
    let (opened, switched) = tokio::join!(opening, switching);
    switched?;
    let opened = opened?;
    let id = opened["tab_id"].as_str().ok_or("slow page id missing")?;
    dom(
        connection,
        "slow-owner",
        "Answer",
        json!({"action":"fill","text":"ready-in-background"}),
    )
    .await?;
    if state(app, id).await?["value"] != "ready-in-background" {
        return Err("background open effect absent".into());
    }
    call(
        connection,
        "slow-owner",
        "control",
        json!({"action":"close"}),
    )
    .await?;
    // Abandon a real private socket after creation starts. Only that provisional
    // page is cancelled; it cannot survive as an orphan or close B.
    use tokio::io::AsyncWriteExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = std::sync::Mutex::new(Some(tx));
    let listener = app.listen_any("xharness-browser-activity", move |event| {
        if let Ok(value) = serde_json::from_str::<Value>(event.payload()) {
            if value["owner"] == "abandoned-owner" && value["phase"] == "started" {
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(value["tabId"].as_str().unwrap().to_owned());
                }
            }
        }
    });
    let mut socket = tokio::net::TcpStream::connect(&connection.address)
        .await
        .map_err(|e| e.to_string())?;
    let request=serde_json::to_vec(&json!({"token":connection.token,"owner":"abandoned-owner","op":"control","arguments":{"action":"open","url":slow}})).unwrap();
    socket
        .write_u32(request.len() as u32)
        .await
        .map_err(|e| e.to_string())?;
    socket
        .write_all(&request)
        .await
        .map_err(|e| e.to_string())?;
    let provisional = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .map_err(|_| "abandoned open not admitted")?
        .map_err(|_| "admission observer closed")?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while app
            .state::<browser::BrowserState>()
            .inspection_target(&provisional)
            .is_err()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| "provisional page not created")?;
    drop(socket);
    tokio::time::timeout(Duration::from_secs(5), async {
        while app
            .state::<browser::BrowserState>()
            .inspection_target(&provisional)
            .is_ok()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| "abandoned page remained live")?;
    app.unlisten(listener);
    if call(connection, "chat-B", "list", json!({})).await?["bound"] != true {
        return Err("cancellation affected B".into());
    }
    println!(
        "BACKGROUND_CREATION_EVIDENCE {}",
        json!({"passed":true,"switch_during_open":true,"collapse_during_open":true,"forged_renderer_cancel":"ignored","abandoned_socket":"provisional_closed","B_survived":true})
    );
    Ok(())
}

pub async fn run(app: &tauri::AppHandle, url: String) -> Result<(), String> {
    let main = app.get_webview("main").ok_or("main absent")?;
    let bridge = app.state::<browser_bridge::BrowserBridge>();
    let connection = bridge.start(app).await?;
    assert!(bridge.is_started());
    let a = call(
        connection,
        "chat-A",
        "control",
        json!({"action":"open","url":url}),
    )
    .await?;
    if a["state"] != "ready" {
        return Err(format!("A not ready: {a}"));
    }
    let aid = a["tab_id"].as_str().ok_or("A id missing")?;
    let b = call(
        connection,
        "chat-B",
        "control",
        json!({"action":"open","url":url}),
    )
    .await?;
    let bid = b["tab_id"].as_str().ok_or("B id missing")?;
    println!(
        "BACKGROUND_OPEN_EVIDENCE {}",
        json!({"passed":true,"renderer_ready_adapter":false,"pages":2})
    );
    browser::desktop_browser_bounds(
        main.clone(),
        app.state(),
        serde_json::from_value(json!({"x":0,"y":0,"width":780,"height":560})).unwrap(),
        None,
    )
    .await?;
    // Select A, then B, then collapse. This is exactly the presentation command
    // used by the production pane, not a test-only change to authorization.
    for (name, selected) in [("switch", Some(bid.to_owned())), ("collapsed", None)] {
        browser::desktop_browser_activate(main.clone(), app.state(), Some(aid.to_owned())).await?;
        browser::desktop_browser_activate(main.clone(), app.state(), selected).await?;
        dom(
            connection,
            "chat-A",
            "Answer",
            json!({"action":"fill","text":name}),
        )
        .await?;
        dom(connection, "chat-A", "Apply", json!({"action":"click"})).await?;
        let observed = state(app, aid).await?;
        if observed["status"] != format!("Applied {name}")
            || state(app, bid).await?["value"] != "unchanged"
        {
            return Err(format!("owner effects not isolated: {observed}"));
        }
        if !cfg!(target_os = "linux") {
            let observation = call(connection, "chat-A", "observe", json!({})).await?;
            let hidden_screenshot =
                bridge_call(connection, "chat-A", "screenshot", json!({})).await?;
            if hidden_screenshot["ok"] != false {
                return Err("unverified hidden native snapshot was permitted".into());
            }
            let hidden_input = call(
                connection,
                "chat-A",
                "input",
                json!({
                    "action":"click","frame_id":observation["frame_id"],"x":10,"y":10
                }),
            )
            .await?;
            if hidden_input["effect"] != "not_started" {
                return Err("unverified hidden native input was permitted".into());
            }
            println!(
                "BACKGROUND_OPERATION_EVIDENCE {}",
                json!({"passed":true,"case":name,"dom":true,"visual":"platform_denied","B_unchanged":true})
            );
            continue;
        }
        let snapshot = screenshot(connection, "chat-A", name).await?;
        let wrong = bridge_call(
            connection,
            "unbound-chat",
            "input",
            json!({"action":"click","frame_id":snapshot["frame_id"],"x":10,"y":10}),
        )
        .await?;
        if wrong["ok"] != false && wrong["result"]["effect"] != "not_started" {
            return Err("unbound chat borrowed A frame".into());
        }
        // Use actual element geometry; trusted input must change A, not just return
        // an acknowledgement. No global mouse/keyboard events or selected-page route.
        let (view, _) = app
            .state::<browser::BrowserState>()
            .inspection_target(aid)?;
        let point=evidence(&view,"(()=>{const r=document.querySelector('#answer').getBoundingClientRect();return{x:r.x+10,y:r.y+10}})()").await?;
        let receipt = call(
            connection,
            "chat-A",
            "input",
            json!({"action":"click","frame_id":snapshot["frame_id"],"x":point["x"],"y":point["y"]}),
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        if receipt["effect"] != "applied"
            || evidence(&view, "document.activeElement.id").await? != "answer"
        {
            return Err(format!(
                "hidden pointer did not focus A: {receipt}; point={point}; actual={}",
                state(app, aid).await?
            ));
        }
        let snapshot = call(connection, "chat-A", "observe", json!({})).await?;
        let key = call(
            connection,
            "chat-A",
            "input",
            json!({"action":"key","frame_id":snapshot["frame_id"],"key":"z"}),
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        let actual = state(app, aid).await?;
        if key["effect"] != "applied"
            || !actual["value"].as_str().is_some_and(|s| s.contains('z'))
            || evidence(&view, "probeEvents.some(e=>e.type==='keydown'&&e.trusted)").await? != true
        {
            return Err(format!(
                "hidden trusted key had no field effect: {key}; {actual}"
            ));
        }
        if state(app, bid).await?["value"] != "unchanged" {
            return Err("background key affected B".into());
        }
        println!(
            "BACKGROUND_OPERATION_EVIDENCE {}",
            json!({"passed":true,"case":name,"dom":true,"native_pointer":true,"native_key":true,"trusted_key":true,"screenshot":true,"B_unchanged":true})
        );
    }
    creation_races(app, connection, bid).await?;
    // Retargeting from the trusted UI may not silently transfer an owned page.
    let origin = url::Url::parse(&url)
        .unwrap()
        .origin()
        .ascii_serialization();
    if browser_delegation::desktop_browser_delegate(
        main.clone(),
        app.state(),
        aid.to_owned(),
        Some("chat-B".into()),
        true,
        Some(origin),
    )
    .await
    .is_ok()
    {
        return Err("implicit owner transfer accepted".into());
    }
    let snapshot = call(connection, "chat-A", "observe", json!({})).await?;
    let (_, inspector) = app
        .state::<browser::BrowserState>()
        .inspection_target(aid)?;
    inspector.invalidate();
    let stale = call(
        connection,
        "chat-A",
        "input",
        json!({"action":"key","frame_id":snapshot["frame_id"],"key":"x"}),
    )
    .await?;
    if stale["effect"] != "not_started" {
        return Err("stale epoch accepted".into());
    }
    // Optional real >TTL soak; no UI renewal timer participates.
    let soak = std::env::var("XHARNESS_BACKGROUND_SOAK_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
        .min(720);
    if soak > 0 {
        let old = call(connection, "chat-A", "observe", json!({})).await?;
        tokio::time::sleep(Duration::from_secs(soak)).await;
        if soak > 600 {
            let denied = call(
                connection,
                "chat-A",
                "input",
                json!({"action":"key","frame_id":old["frame_id"],"key":"x"}),
            )
            .await?;
            if denied["effect"] != "not_started" {
                return Err("expired frame resurrected during host renewal".into());
            }
        }
        dom(
            connection,
            "chat-A",
            "Answer",
            json!({"action":"fill","text":"after-soak"}),
        )
        .await?;
        println!(
            "BACKGROUND_SOAK_EVIDENCE {}",
            json!({"passed":true,"idle_seconds":soak,"UI_renewal":false})
        );
    }
    // Revoking site access must deny actions without orphaning the page.
    inspector.revoke();
    if bridge_call(connection, "chat-A", "observe", json!({})).await?["ok"] != false {
        return Err("revoked site remained readable".into());
    }
    // Lifetime ownership cannot survive an explicit transfer to a new chat.
    browser_delegation::desktop_browser_delegate(
        main.clone(),
        app.state(),
        bid.into(),
        None,
        false,
        None,
    )
    .await?;
    browser_delegation::desktop_browser_delegate(
        main.clone(),
        app.state(),
        bid.into(),
        Some("transfer-owner".into()),
        true,
        Some(
            url::Url::parse(&url)
                .unwrap()
                .origin()
                .ascii_serialization(),
        ),
    )
    .await?;
    let former = call(connection, "chat-B", "control", json!({"action":"close"})).await?;
    if former["effect"] != "not_started" {
        return Err("former owner closed transferred page".into());
    }
    for owner in ["chat-A", "transfer-owner"] {
        let result = call(connection, owner, "control", json!({"action":"close"})).await?;
        if result["effect"] != "applied" {
            return Err(format!("close failed: {result}"));
        }
    }
    if call(connection, "chat-A", "list", json!({})).await?["bound"] != false {
        return Err("closed page remained bound".into());
    }
    println!(
        "BACKGROUND_ISOLATION_EVIDENCE {}",
        json!({"passed":true,"implicit_transfer":"denied","stale_epoch":"denied","explicit_close":true,"revoked_site_cleanup":true,"former_owner_close":"denied"})
    );
    Ok(())
}
