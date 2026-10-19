use std::time::Duration;

use anyhow::Context;
use chromiumoxide::cdp::js_protocol::debugger::{self, EventScriptParsed};
use chromiumoxide::cdp::js_protocol::runtime::{
    CallFunctionOnParams, EvaluateParams, ExecutionContextId, GetPropertiesParams, RemoteObjectId,
};
use chromiumoxide::types::MethodId;
use chromiumoxide::{Command, Method, Page};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::info;

const INSTALL_SCRIPT: &str = include_str!("menu.js");

// V8 scope inspection includes internal#location types absent from Chromium's public object enums.
#[derive(Serialize)]
#[serde(transparent)]
struct ScopeProperties(GetPropertiesParams);

impl Method for ScopeProperties {
    fn identifier(&self) -> MethodId {
        self.0.identifier()
    }
}

impl Command for ScopeProperties {
    type Response = ScopePropertiesResponse;
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScopePropertiesResponse {
    result: Vec<ScopeProperty>,
    internal_properties: Option<Vec<ScopeProperty>>,
}

#[derive(Debug, Deserialize)]
struct ScopeProperty {
    name: String,
    value: Option<ScopeValue>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScopeValue {
    object_id: Option<RemoteObjectId>,
    description: Option<String>,
}

pub(super) struct MenuProxy {
    context: ExecutionContextId,
}

impl MenuProxy {
    pub(super) async fn install(page: &Page) -> anyhow::Result<Self> {
        let mut scripts = page.event_listener::<EventScriptParsed>().await?;

        // Enumerate existing preload contexts once; no breakpoints or page pauses are used.
        page.execute(debugger::EnableParams::default()).await?;

        let context = tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(script) = scripts.next().await {
                if script.url == "node:electron/js2c/sandbox_bundle" || script.url.ends_with("/preload.js") {
                    return Some(script.execution_context_id);
                }
            }

            None
        })
        .await;

        page.execute(debugger::DisableParams::default()).await?;

        let context = context?.context("Electron preload context not found")?;
        let function = page
            .execute(EvaluateParams::builder()
                .expression("Object.values(globalThis.__SENTRY_IPC__ ?? {}).flatMap(Object.values).find(value => typeof value === 'function')")
                .context_id(context)
                .build().map_err(anyhow::Error::msg)?)
            .await?;
        let function = function.result.result.object_id.context("Preload function not found")?;
        let properties = page.execute(ScopeProperties(GetPropertiesParams::new(function))).await?;
        let scopes = properties
            .result
            .internal_properties
            .unwrap_or_default()
            .into_iter()
            .find(|property| property.name == "[[Scopes]]")
            .and_then(|property| property.value?.object_id)
            .context("Preload function scopes unavailable")?;
        let scopes = page.execute(ScopeProperties(GetPropertiesParams::new(scopes))).await?;

        for scope in scopes.result.result {
            let Some(value) = scope.value else { continue };

            if value.description.as_deref() == Some("Global") {
                continue;
            }

            let Some(object) = value.object_id else { continue };
            let properties = page.execute(ScopeProperties(GetPropertiesParams::new(object))).await?;

            for property in properties.result.result {
                let Some(object) = property.value.and_then(|value| value.object_id) else {
                    continue;
                };

                if Self::try_install(page, object).await? {
                    info!("Native menu proxy installed");

                    return Ok(Self { context });
                }
            }
        }

        anyhow::bail!("Writable Electron IPC object not found")
    }

    async fn try_install(page: &Page, object: RemoteObjectId) -> anyhow::Result<bool> {
        let result = page.execute(CallFunctionOnParams::builder()
            .object_id(object)
            .function_declaration(format!("function() {{ if (!this?.ipcRenderer || typeof this.ipcRenderer.invoke !== 'function' || !Object.isExtensible(this.ipcRenderer)) return false; return ({INSTALL_SCRIPT}).call(this); }}"))
            .return_by_value(true)
            .build().map_err(anyhow::Error::msg)?)
            .await?;

        Ok(result.result.result.value == Some(serde_json::Value::Bool(true)))
    }

    pub(super) async fn execute(&self, page: &Page, expression: &str) -> anyhow::Result<()> {
        let result = page
            .execute(
                EvaluateParams::builder()
                    .context_id(self.context)
                    .expression(format!("globalThis.__codexApp2WebMenu?.{expression}"))
                    .build()
                    .map_err(anyhow::Error::msg)?,
            )
            .await?;

        anyhow::ensure!(result.result.exception_details.is_none(), "Menu proxy script failed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn scope_properties_accept_v8_internal_types() {
        let properties: super::ScopePropertiesResponse = serde_json::from_value(serde_json::json!({
            "result": [], "internalProperties": [
                { "name": "[[FunctionLocation]]", "value": { "type": "internal#location", "value": { "scriptId": "1", "lineNumber": 0 } } },
                { "name": "[[Scopes]]", "value": { "type": "object", "subtype": "internal#scopeList", "objectId": "scopes" } }
            ]
        })).unwrap();
        let scopes = properties.internal_properties.unwrap();
        assert_eq!(
            scopes[1].value.as_ref().unwrap().object_id.as_ref().unwrap().as_ref(),
            "scopes"
        );
    }

    #[test]
    fn menu_proxy_preserves_native_calls_and_validates_remote_responses() {
        let output = std::process::Command::new("node")
            .args(["--test", concat!(env!("CARGO_MANIFEST_DIR"), "/src/cdp/menu.test.js")])
            .output()
            .expect("Node is required for menu proxy tests");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
