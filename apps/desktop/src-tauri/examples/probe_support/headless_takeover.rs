//! Opt-in real Chrome/Edge probe. Disposable loopback site and owned profiles only.
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use xharness_desktop::headless_browser::Browsers;

async fn fixture() -> (
    String,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let report = Arc::new(tokio::sync::Notify::new());
    let observed = Arc::clone(&report);
    let task = tokio::spawn(async move {
        let page=br#"<!doctype html><meta charset=utf-8><style>body{margin:0}input{position:absolute;left:20px;top:20px;width:200px;height:40px}</style><input id="field" aria-label="Field"><script>document.querySelector('input').addEventListener('input',e=>window.lastTrusted=e.isTrusted)</script>"#;
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut request = [0; 8192];
            let Ok(Ok(n)) =
                tokio::time::timeout(Duration::from_secs(2), socket.read(&mut request)).await
            else {
                continue;
            };
            if n == 0 {
                continue;
            }
            if request[..n].starts_with(b"GET /dispatched ") {
                observed.notify_one();
            }
            let header=format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",page.len());
            if socket.write_all(header.as_bytes()).await.is_ok()
                && socket.write_all(page).await.is_ok()
            {
                let _ = socket.shutdown().await;
            }
        }
    });
    (url, report, task)
}

async fn preview(b: &Browsers, owner: &str, tab: &str) -> Value {
    let image = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let image = b.preview(owner, tab).await.unwrap();
            if image["busy"] != true {
                return image;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(image["width"], 640);
    assert_eq!(image["height"], 360);
    assert_eq!(image["tabId"], tab);
    assert!(image["image"]
        .as_str()
        .is_some_and(|s| s.starts_with("/9j/") && s.len() > 1024));
    image
}

pub async fn run() {
    let (url, dispatched, server) = fixture().await;
    let b = Browsers::default();
    let a = b
        .dispatch("a", "control", json!({"action":"open","url":url}))
        .await
        .unwrap();
    let c = b
        .dispatch("b", "control", json!({"action":"open","url":url}))
        .await
        .unwrap();
    let tab = a["tab_id"].as_str().unwrap();
    let other = c["tab_id"].as_str().unwrap();
    assert_ne!(tab, other);
    // No sidebar, then switching preview to another chat must not consume A's frame.
    let observe = b.dispatch("a", "observe", json!({})).await.unwrap();
    preview(&b, "b", other).await;
    b.dispatch(
        "a",
        "input",
        json!({"action":"click","frame_id":observe["frame_id"],"x":40,"y":40}),
    )
    .await
    .unwrap()
    .get("ok")
    .filter(|ok| *ok == true)
    .expect("A input while only B previewed");
    let f = preview(&b, "a", tab).await;
    assert_eq!(f["control"]["mode"], "agent");
    assert!(f.get("manualFrame").is_none());
    assert!(b
        .manual("b", tab, json!({"action":"takeover","generation":"0"}))
        .await
        .is_err());

    let observed = b.dispatch("a", "observe", json!({})).await.unwrap();
    // The real page reports the first script has already been dispatched.
    // It will not finish normally, so takeover must drain and expose unknown.
    let mut inflight = std::pin::pin!(b.dispatch(
        "a",
        "evaluate",
        json!({
            "code":"await fetch('/dispatched'); await new Promise(()=>{});",
            "timeout_ms":2000
        })
    ));
    tokio::select! {
        _ = &mut inflight => panic!("in-flight command returned before the fixture reported dispatch"),
        signal = tokio::time::timeout(Duration::from_secs(6),dispatched.notified())=>{signal.unwrap();}
    }
    let queued = b.dispatch(
        "a",
        "evaluate",
        json!({"code":"document.querySelector('input').value='stale'; return true;"}),
    );
    let mut queued = std::pin::pin!(queued);
    tokio::select! { biased; _ = &mut queued => panic!("queued effect was not gated"), _ = std::future::ready(()) => {} }
    let mut takeover =
        std::pin::pin!(b.manual("a", tab, json!({"action":"takeover","generation":"0"})));
    tokio::select! { biased; _ = &mut takeover => panic!("transfer did not drain the active gate"), _=std::future::ready(())=>{} }
    assert_eq!(
        b.dispatch("a", "control", json!({"action":"status"}))
            .await
            .unwrap()["control"],
        "switching"
    );
    let (active, denied, seat) = tokio::join!(inflight, queued, takeover);
    assert_eq!(active.unwrap()["effect"], "unknown");
    let denied = denied.unwrap();
    assert_eq!(denied["effect"], "not_started");
    assert!(denied["message"]
        .as_str()
        .unwrap()
        .contains("control changed"));
    let seat = seat.unwrap();
    assert_eq!(seat["prior_effect_uncertain"], true);
    assert_eq!(seat["mode"], "human");
    assert_eq!(
        b.dispatch(
            "a",
            "evaluate",
            json!({"code":"throw Error('must not run')"})
        )
        .await
        .unwrap()["effect"],
        "not_started"
    );
    for arguments in [
        json!({"action":"close"}),
        json!({"action":"open","url":url}),
    ] {
        assert_eq!(
            b.dispatch("a", "control", arguments).await.unwrap()["effect"],
            "not_started"
        );
    }
    let next = preview(&b, "a", tab).await;
    let generation = next["control"]["generation"].clone();
    let point = json!({"action":"click","generation":generation,"frame_id":next["manualFrame"],"x":40,"y":40});
    assert_eq!(
        b.manual("a", tab, point.clone()).await.unwrap()["effect"],
        "applied"
    );
    assert_eq!(
        b.manual("a", tab, point).await.unwrap()["effect"],
        "not_started"
    );
    let frame = preview(&b, "a", tab).await["manualFrame"].clone();
    assert_eq!(
        b.manual(
            "a",
            tab,
            json!({"action":"text","generation":generation,"frame_id":frame,"text":"人工 β"})
        )
        .await
        .unwrap()["effect"],
        "applied"
    );
    let stale = preview(&b, "a", tab).await;
    assert_eq!(
        b.manual("a", tab, json!({"action":"return","generation":generation}))
            .await
            .unwrap()["mode"],
        "agent"
    );
    assert!(b.manual("a",tab,json!({"action":"click","generation":generation,"frame_id":stale["manualFrame"],"x":40,"y":40})).await.is_err());
    assert_eq!(
        b.dispatch(
            "a",
            "input",
            json!({"action":"click","frame_id":observed["frame_id"],"x":40,"y":40})
        )
        .await
        .unwrap()["effect"],
        "not_started"
    );
    let premature = b
        .dispatch(
            "a",
            "evaluate",
            json!({"code":"document.querySelector('input').value='must not run'"}),
        )
        .await
        .unwrap();
    assert_eq!(premature["effect"], "not_started");
    let _fresh_observation = b.dispatch("a", "observe", json!({})).await.unwrap();
    let result=b.dispatch("a","evaluate",json!({"code":"return {text:document.querySelector('input').value, trusted:window.lastTrusted};"})).await.unwrap();
    assert_eq!(
        result["value"],
        json!({"text":"人工 β","trusted":true}),
        "{result}"
    );
    let fresh = b.dispatch("a", "observe", json!({})).await.unwrap();
    let reference = fresh["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["label"] == "Field")
        .unwrap()["ref"]
        .clone();
    let after=b.dispatch("a","perform",json!({"action":"fill","frame_id":fresh["frame_id"],"ref":reference,"text":"agent resumed"})).await.unwrap();
    assert_eq!(after["effect"], "applied");
    let other_value = b
        .dispatch(
            "b",
            "evaluate",
            json!({"code":"return document.querySelector('input').value;"}),
        )
        .await
        .unwrap();
    assert_eq!(other_value["value"], "");
    assert_eq!(
        b.dispatch("a", "control", json!({"action":"close"}))
            .await
            .unwrap()["effect"],
        "applied"
    );
    assert!(b
        .manual("a", tab, json!({"action":"status"}))
        .await
        .is_err());
    // A user/Host retiring a chat is not an Agent control-close operation.
    let b_seat = b
        .manual("b", other, json!({"action":"takeover","generation":"0"}))
        .await
        .unwrap();
    assert_eq!(b_seat["mode"], "human");
    assert_eq!(b.close("b").await.unwrap()["closed"], true);
    assert!(b
        .manual("b", other, json!({"action":"status"}))
        .await
        .is_err());
    b.shutdown().await;
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    println!("TAKEOVER_ENGINE_EVIDENCE: exact-target JPEG, queued action fenced, real trusted Unicode input, unknown write surfaced, stale replay denied, fresh observation required for writes, B unchanged, Agent resumed same page, lifecycle cleanup while human");
}
