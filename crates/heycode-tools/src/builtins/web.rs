//! Model-facing public web Consumers over the replaceable `web` service.

use std::sync::Arc;

use async_trait::async_trait;

use crate::registry::ToolRegistry;
use crate::tool::{Tool, ToolCtx, ToolError};
use heycode_core::ToolSpec;

const FETCH_MAX_BYTES: u32 = 64 * 1024;
const FETCH_MAX_SOURCE_BYTES: u32 = 4 * 1024 * 1024;
const SEARCH_MAX_CHARS: usize = 4_096;

/// Fetch a public page through the composed web provider.
pub struct WebFetch {
    web: Arc<heycode_web::WebRegistry>,
}

impl WebFetch {
    /// Bind the Consumer to the composed web registry.
    #[must_use]
    pub fn new(web: Arc<heycode_web::WebRegistry>) -> Self {
        Self { web }
    }
}

#[async_trait]
impl Tool for WebFetch {
    fn effect(&self) -> crate::ToolEffect {
        crate::ToolEffect::ReadOnly
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_fetch".to_owned(),
            description: "Fetch a public HTTP(S) page and return its readable text, capped. Use for docs, pages, and raw files the user cites by URL."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["url"],
                "properties": {
                    "url": {"type": "string", "description": "Absolute public HTTP(S) URL"}
                }
            }),
        }
    }

    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        Some(heycode_core::UntrustedContentBoundary::web())
    }

    async fn run(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        let url = args
            .get("url")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("`url` must be a string"))?;
        let request =
            heycode_web::WebFetchRequest::with_limits(url, FETCH_MAX_SOURCE_BYTES, FETCH_MAX_BYTES)
                .map_err(|error| web_error("fetch", error))?;
        let result = self
            .web
            .fetch(request, cx.cancellation.clone())
            .await
            .map_err(|error| web_error("fetch", error))?;
        let source = result.source();
        let source_label = escape_markdown_label(source.title().unwrap_or(source.url()));
        let mut content = format!(
            "Source: [{source_label}](<{}>)\n\n{}",
            source.url(),
            result.content()
        );
        if let Some(pages) = source.page_count() {
            content.push_str(&format!("\n(PDF pages: {pages})"));
        }
        if source.source_truncated() {
            content.push_str(&format!(
                "\n(raw source truncated at {FETCH_MAX_SOURCE_BYTES} bytes)"
            ));
        } else if result.truncated() {
            content.push_str(&format!("\n(truncated at {FETCH_MAX_BYTES} bytes)"));
        }
        Ok(serde_json::Value::String(content))
    }
}

fn escape_markdown_label(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '[' | ']') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Search the public web through the composed web provider.
pub struct WebSearch {
    web: Arc<heycode_web::WebRegistry>,
}

impl WebSearch {
    /// Bind the Consumer to the composed web registry.
    #[must_use]
    pub fn new(web: Arc<heycode_web::WebRegistry>) -> Self {
        Self { web }
    }
}

#[async_trait]
impl Tool for WebSearch {
    fn effect(&self) -> crate::ToolEffect {
        crate::ToolEffect::ReadOnly
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".to_owned(),
            description: "Search the public web and return ranked result titles, URLs, and snippets. Use when you need current information beyond your training data."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": {"type": "string", "description": "The web search query"}
                }
            }),
        }
    }

    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        Some(heycode_core::UntrustedContentBoundary::web())
    }

    async fn run(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        let query = args
            .get("query")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("`query` must be a string"))?;
        let request = heycode_web::WebSearchRequest::new(query, 8)
            .map_err(|error| web_error("search", error))?;
        let results = self
            .web
            .search(request, cx.cancellation.clone())
            .await
            .map_err(|error| web_error("search", error))?;
        let mut output = String::new();
        for (index, result) in results.iter().enumerate() {
            output.push_str(&format!(
                "{}. {}\n   {}\n   {}\n",
                index + 1,
                result.title(),
                result.url(),
                result.snippet()
            ));
            if output.chars().count() > SEARCH_MAX_CHARS {
                break;
            }
        }
        if output.is_empty() {
            return Ok(serde_json::Value::String("no results found".to_owned()));
        }
        if output.chars().count() > SEARCH_MAX_CHARS {
            let bounded = output.chars().take(SEARCH_MAX_CHARS).collect::<String>();
            output = format!("{bounded}\n(truncated)");
        }
        Ok(serde_json::Value::String(output))
    }
}

/// Register both web Consumers onto a live tool registry.
///
/// # Errors
/// Registry duplicate-name failures surface verbatim.
pub fn register_web_tools(
    registry: &ToolRegistry,
    web: Arc<heycode_web::WebRegistry>,
) -> Result<(), crate::registry::RegisterError> {
    registry.register_shared(Arc::new(WebFetch::new(web.clone())))?;
    registry.register_shared(Arc::new(WebSearch::new(web)))?;
    Ok(())
}

fn web_error(operation: &str, error: heycode_web::WebError) -> ToolError {
    let message = match error.class() {
        heycode_web::WebErrorClass::InvalidRequest => {
            format!("invalid public web {operation} request")
        }
        heycode_web::WebErrorClass::Unsupported => {
            format!("no configured web provider supports {operation}")
        }
        heycode_web::WebErrorClass::Ambiguous => {
            format!("multiple web providers support {operation}; select one in web settings")
        }
        heycode_web::WebErrorClass::PolicyDenied => {
            format!("web {operation} was denied by the configured domain policy")
        }
        heycode_web::WebErrorClass::Cancelled => format!("web {operation} was cancelled"),
        heycode_web::WebErrorClass::Timeout => format!("web {operation} timed out"),
        heycode_web::WebErrorClass::Stopped => "web service is unavailable".to_owned(),
        heycode_web::WebErrorClass::Unavailable
        | heycode_web::WebErrorClass::Duplicate
        | heycode_web::WebErrorClass::Network
        | heycode_web::WebErrorClass::Http
        | heycode_web::WebErrorClass::InvalidResponse => {
            format!("web {operation} failed through the active provider")
        }
    };
    ToolError::new(message)
}

/// Compatibility test helper now owned by `heycode-web`.
#[doc(hidden)]
pub fn ip_is_private_for_tests(address: &std::net::IpAddr) -> bool {
    heycode_web::ip_is_private_for_tests(address)
}

/// Compatibility parser helper now owned by `heycode-web`.
#[doc(hidden)]
#[must_use]
pub fn parse_ddg_lite(html: &str) -> Vec<(String, String, String)> {
    heycode_web::parse_ddg_lite(html)
}

/// Compatibility HTML helper now owned by `heycode-web`.
#[doc(hidden)]
#[must_use]
pub fn strip_for_tests(html: &str) -> String {
    heycode_web::strip_html_for_tests(html)
}
