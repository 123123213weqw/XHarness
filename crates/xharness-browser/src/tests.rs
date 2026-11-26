use super::*;

#[tokio::test]
async fn absent_chat_and_invalid_urls_do_not_start_a_browser() {
    let browsers = Browsers::default();
    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://user:password@example.com/",
    ] {
        assert!(browsers
            .dispatch("test", "control", json!({"action":"open","url":url}))
            .await
            .is_err());
    }
    assert!(browsers.sessions.lock().await.is_empty());
    assert!(browsers
        .dispatch("other", "observe", json!({}))
        .await
        .is_err());
    assert!(browsers.preview("other", "headless:wrong").await.is_err());
}
#[tokio::test]
async fn capacity_is_bounded_and_failed_admission_slots_are_collectable() {
    let browsers = Browsers::default();
    let mut slots = Vec::new();
    for index in 0..4 {
        slots.push(
            browsers
                .slot(&format!("owner-{index}"), true)
                .await
                .unwrap(),
        );
    }
    assert!(browsers.slot("fifth", true).await.is_err());
    drop(OpenLease(slots.remove(0)));
    assert!(browsers.slot("fifth", true).await.is_ok());
    assert_eq!(browsers.sessions.lock().await.len(), 4);
}
#[tokio::test]
async fn stale_ui_closure_cannot_close_another_chat_or_successor() {
    let browsers = Browsers::default();
    let slot = browsers.slot("owner", true).await.unwrap();
    *slot.id.lock().unwrap() = Some("headless:new".into());
    assert!(browsers
        .close_tab("other", Some("headless:new"))
        .await
        .is_err());
    assert!(browsers
        .close_tab("owner", Some("headless:old"))
        .await
        .is_err());
    assert!(!slot.retired.load(Ordering::SeqCst));
    let result = browsers
        .close_tab("owner", Some("headless:new"))
        .await
        .unwrap();
    assert_eq!(result["tab_id"], "headless:new");
    assert!(slot.retired.load(Ordering::SeqCst));
    assert!(browsers.sessions.lock().await.is_empty());
}
#[test]
fn discovered_candidates_are_files_and_canonical_aliases_are_unique() {
    let candidates = process::candidates();
    let mut seen = std::collections::HashSet::new();
    for file in candidates {
        assert!(file.is_absolute() && file.is_file());
        assert!(seen.insert(file.canonicalize().unwrap()));
    }
}

#[test]
fn browser_discovery_pins_numeric_loopback_and_rejects_foreign_paths() {
    use super::process::parse_port_file;
    assert_eq!(
        parse_port_file("1234\n/devtools/browser/abc-123\n"),
        Some((1234, "abc-123"))
    );
    for invalid in [
        "1234\nws://example.com/devtools/browser/id",
        "1234\n/devtools/browser/../page/id",
        "1234\n/devtools/browser/",
        "0\n/devtools/browser/id",
        "65536\n/devtools/browser/id",
    ] {
        assert!(parse_port_file(invalid).is_none(), "{invalid}");
    }
}

#[tokio::test]
async fn standalone_has_no_trusted_desktop_surface() {
    use super::Browsers;
    use serde_json::json;
    let standalone = Browsers::standalone();
    let owner = "fixture";
    assert_eq!(
        standalone.status(owner).await["capabilities"]["human_takeover"],
        false
    );
    assert_eq!(
        Browsers::default().status(owner).await["capabilities"]["human_takeover"],
        true
    );
    assert!(standalone
        .open_from_shell(owner, "https://example.invalid".into())
        .await
        .is_err());
    assert!(standalone.preview(owner, "stale-tab").await.is_err());
    assert!(standalone
        .manual(owner, "stale-tab", json!({"action":"status"}))
        .await
        .is_err());
    assert!(standalone.sessions.lock().await.is_empty());
}

#[tokio::test]
async fn debug_requires_standalone_and_never_creates_a_chat_browser() {
    let args = serde_json::json!({"action":"start"});
    let desktop = super::Browsers::default();
    assert_eq!(
        desktop.debug("fixture", args.clone()).await.unwrap_err(),
        "browser_debug is available only in a standalone Host"
    );
    let standalone = super::Browsers::standalone();
    assert!(standalone.debug("fixture", args).await.is_err());
    assert!(desktop.sessions.lock().await.is_empty());
    assert!(standalone.sessions.lock().await.is_empty());
}
