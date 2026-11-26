//! Exact-page human control; never called from the model tool bridge.
#![forbid(unsafe_code)]
use super::{rejected, seat::Manual, Browsers, Driver, Slot, Ticket};
use serde_json::{json, Value};
use std::{sync::atomic::Ordering, time::Duration};

impl Browsers {
    /// Trusted shell only. The same owner, exact page, driver generation and
    /// current preview are all required; nothing here is a model RPC.
    pub async fn manual(&self, owner: &str, tab: &str, arguments: Value) -> Result<Value, String> {
        if !self.desktop {
            return Err("human takeover is not available on this Host".into());
        }
        let action: Manual =
            serde_json::from_value(arguments).map_err(|_| "invalid browser takeover arguments")?;
        let slot = self.slot(owner, false).await?;
        let exact = || {
            if slot.retired.load(Ordering::SeqCst)
                || slot
                    .id
                    .lock()
                    .map_err(|_| "browser identity unavailable")?
                    .as_deref()
                    != Some(tab)
            {
                return Err("browser control target closed or replaced".to_owned());
            }
            Ok(())
        };
        exact()?;
        if matches!(action, Manual::Status {}) {
            return slot.seat.receipt();
        }
        let generation = |value: &str| {
            value
                .parse::<u64>()
                .map_err(|_| "invalid browser control generation".to_owned())
        };
        match action {
            Manual::Takeover {
                generation: expected,
            } => {
                self.change_driver(&slot, exact, generation(&expected)?, Driver::Human)
                    .await
            }
            Manual::Return {
                generation: expected,
            } => {
                self.change_driver(&slot, exact, generation(&expected)?, Driver::Agent)
                    .await
            }
            other => {
                let expected = match &other {
                    Manual::Click { generation, .. }
                    | Manual::Text { generation, .. }
                    | Manual::Key { generation, .. }
                    | Manual::Navigate { generation, .. } => generation,
                    _ => return Err("invalid browser input".into()),
                };
                let ticket = Ticket {
                    generation: generation(expected)?,
                    driver: Driver::Human,
                };
                slot.seat.require(ticket, Driver::Human)?;
                let mut guard = slot.instance.lock().await;
                exact()?;
                slot.seat.require(ticket, Driver::Human)?;
                let instance = guard.as_mut().ok_or("browser is not ready")?;
                instance.release_pending().await?;
                let admit = || slot.admit(ticket);
                let target_frame = match &other {
                    Manual::Click { frame_id, .. }
                    | Manual::Text { frame_id, .. }
                    | Manual::Key { frame_id, .. } => Some(frame_id),
                    _ => None,
                };
                if let Some(id) = target_frame {
                    let frame = {
                        let mut frame = slot
                            .manual_frame
                            .lock()
                            .map_err(|_| "preview frame unavailable")?;
                        if frame.as_ref().is_none_or(|f| f.id != *id) {
                            return Ok(rejected("preview changed; wait for a fresh image"));
                        }
                        frame.take().ok_or("preview frame unavailable")?
                    };
                    if frame.created.elapsed() > Duration::from_secs(5)
                        || instance.stamp().await? != frame.stamp
                    {
                        return Ok(rejected("preview geometry changed; wait for a fresh image"));
                    }
                }
                use crate::InputRequest;
                match other {
                    Manual::Click { frame_id, x, y, .. } => {
                        let request = InputRequest::Click { frame_id, x, y };
                        request.validate()?;
                        instance.send_input(request, admit).await
                    }
                    Manual::Key { frame_id, key, .. } => {
                        // No global shortcuts/clipboard. These keys target only
                        // the engine element currently focused by the user.
                        if !matches!(
                            key.as_str(),
                            "Enter"
                                | "Tab"
                                | "Escape"
                                | "Backspace"
                                | "ArrowUp"
                                | "ArrowDown"
                                | "PageUp"
                                | "PageDown"
                        ) {
                            return Err("unsupported manual browser key".into());
                        }
                        let request = InputRequest::Key {
                            frame_id,
                            key,
                            modifiers: vec![],
                        };
                        request.validate()?;
                        instance.send_input(request, admit).await
                    }
                    Manual::Text { text, .. } => {
                        if text.is_empty() || text.len() > 4096 {
                            return Err("invalid manual text size".into());
                        }
                        let effect = admit()?;
                        instance
                            .process
                            .cdp
                            .call("Input.insertText", json!({"text":text}))
                            .await?;
                        effect.resolve(true);
                        Ok(json!({"ok":true,"effect":"applied"}))
                    }
                    Manual::Navigate { url, .. } => {
                        let url = crate::web_url(&url)?.to_string();
                        slot.manual_frame
                            .lock()
                            .map_err(|_| "preview frame unavailable")?
                            .take();
                        let effect = admit()?;
                        let result = instance.navigate(url).await;
                        effect.resolve(result.is_ok());
                        result
                    }
                    _ => Err("invalid browser input".into()),
                }
            }
        }
    }

    async fn change_driver(
        &self,
        slot: &Slot,
        exact: impl Fn() -> Result<(), String>,
        expected: u64,
        to: Driver,
    ) -> Result<Value, String> {
        // Announce the transition before waiting for already-admitted work.
        // Queued work carries an old ticket even if the user returns rapidly.
        let transition = slot.seat.begin(expected, to)?;
        let mut guard = slot.instance.lock().await;
        exact()?;
        let instance = guard.as_mut().ok_or("browser is not ready")?;
        instance.release_pending().await?;
        instance.frame = None;
        slot.manual_frame
            .lock()
            .map_err(|_| "preview frame unavailable")?
            .take();
        transition.commit()?;
        slot.seat.receipt()
    }
}
