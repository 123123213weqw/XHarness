//! Fixed native tools carry their argument schemas in every model request.
//! Dynamic foreign MCP tools remain lazy and keep their separate namespace.
#![forbid(unsafe_code)]

use crate::native_browser::{self, NativeBrowser};
use serde_json::{Map, Value};
use std::sync::Arc;
use xharness_tools::{ToolDefinition, ToolHandlerError, ToolOutput, ToolSpec};

pub(super) fn is_direct(name: &str) -> bool {
    matches!(
        name,
        "browser_control"
            | "browser_observe"
            | "browser_perform"
            | "browser_evaluate"
            | "browser_debug"
            | "browser_input"
            | "browser_screenshot"
            | "browser_request"
            | "browser_read_response"
    )
}

pub(super) async fn invoke(
    native: &NativeBrowser,
    headless: bool,
    owner: &str,
    tool: &str,
    parameters: Map<String, Value>,
    media: Option<&Arc<dyn xharness_computer::ComputerMediaSink>>,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<ToolOutput, ToolHandlerError> {
    super::contract::validate_native(headless, tool, &Value::Object(parameters.clone()))?;
    if tool == "screenshot" {
        let media = media.ok_or_else(|| super::error("browser attachment service unavailable"))?;
        return native
            .screenshot(owner, parameters, media.as_ref(), cancellation)
            .await;
    }
    super::native_output::receipt(
        native
            .call(owner, tool, parameters, cancellation)
            .await
            .map_err(super::error)?,
    )
}

pub(super) fn specs(
    native: Arc<NativeBrowser>,
    owner: String,
    media: Option<Arc<dyn xharness_computer::ComputerMediaSink>>,
) -> Vec<ToolSpec> {
    let webview = native.supports_webview();
    let definitions = if webview {
        native_browser::tools()
    } else {
        native_browser::headless_tools()
    };
    let mut specs: Vec<_> = definitions
        .into_iter()
        .map(|d| {
            let tool = d["name"].as_str().unwrap_or_default().to_owned();
            let webview_only = matches!(tool.as_str(), "request" | "read_response");
            let backends: &[&str] = if webview_only {
                &[native_browser::SERVER]
            } else if webview {
                &[native_browser::HEADLESS_SERVER, native_browser::SERVER]
            } else {
                &[native_browser::HEADLESS_SERVER]
            };
            let description = if webview_only || !webview {
                d["description"].as_str().unwrap_or_default().to_owned()
            } else {
                native_browser::headless_tools()
                    .iter()
                    .find(|t| t["name"] == tool)
                    .and_then(|t| t["description"].as_str())
                    .unwrap_or_default()
                    .to_owned()
            };
            let definition = ToolDefinition::new(
                format!("browser_{tool}"),
                format!("Typed native browser tool; no discovery required. {description}"),
                super::contract::direct_schema(&d["inputSchema"], backends),
            );
            let native = native.clone();
            let owner = owner.clone();
            let media = media.clone();
            ToolSpec::new(definition, move |context| {
                let native = native.clone();
                let owner = owner.clone();
                let media = media.clone();
                let tool = tool.clone();
                async move {
                    let result = async {
                        let mut parameters =
                            context.arguments.as_object().cloned().ok_or_else(|| {
                                super::error("browser arguments must be an object")
                            })?;
                        let backend = parameters
                            .remove("backend")
                            .ok_or_else(|| super::error("browser backend required"))?;
                        let headless =
                            match backend.as_str() {
                                Some(native_browser::HEADLESS_SERVER) if !webview_only => true,
                                Some(native_browser::SERVER) if webview => false,
                                _ => return Err(super::error(
                                    "invalid browser backend; no backend was selected or changed",
                                )),
                            };
                        let background = native.headless();
                        let selected = if headless {
                            &background
                        } else {
                            native.as_ref()
                        };
                        invoke(
                            selected,
                            headless,
                            &owner,
                            &tool,
                            parameters,
                            media.as_ref(),
                            &context.cancellation,
                        )
                        .await
                    }
                    .await;
                    super::native_output::finish(result)
                }
            })
            .with_timeout(std::time::Duration::from_secs(45))
        })
        .collect();
    if !webview {
        specs.push(debug_spec(native, owner));
    }
    specs
}

fn debug_spec(native: Arc<NativeBrowser>, owner: String) -> ToolSpec {
    let schema = super::contract::direct_schema(
        &xharness_browser::debug_schema(),
        &[native_browser::HEADLESS_SERVER],
    );
    ToolSpec::new(
        ToolDefinition::new(
            "browser_debug",
            "Standalone Chrome/Edge only. On the current open page, start recording Console and Network in the background, even between turns. Read untrusted events using capture_id and next_cursor; optional kind=console or network. Stop discards the capture. Start before the navigation you want to inspect. Does not capture headers or response bodies.",
            schema,
        ),
        move |context| {
            let native = native.clone();
            let owner = owner.clone();
            async move {
                let result = async {
                    let mut arguments = context.arguments.as_object().cloned()
                        .ok_or_else(|| super::error("browser debug arguments must be an object"))?;
                    if arguments.remove("backend").as_ref().and_then(Value::as_str)
                        != Some(native_browser::HEADLESS_SERVER)
                    {
                        return Err(super::error("browser_debug requires backend=browser"));
                    }
                    super::native_output::receipt(native.debug(
                        &owner,
                        Value::Object(arguments),
                        &context.cancellation,
                    ).await.map_err(super::error)?)
                }.await;
                super::native_output::finish(result)
            }
        },
    )
    .with_timeout(std::time::Duration::from_secs(12))
}
