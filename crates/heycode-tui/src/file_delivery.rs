//! Local delivery cards are derived only from complete admitted rich results.
use heycode_core::{
    AttachmentMetadata, DurableToolResult, DurableToolResultBlock, ToolResultAudience,
};
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use serde_json::Value;

#[derive(Clone)]
pub(crate) struct DeliveredFile {
    pub(crate) path: String,
    pub(crate) metadata: AttachmentMetadata,
}

pub(crate) struct DeliveryReceipt {
    pub(crate) files: Vec<DeliveredFile>,
    caption: Option<String>,
    display: String,
}

/// A JSON-shaped receipt alone is never evidence that its bytes were admitted.
pub(crate) fn receipt(value: &Value) -> Option<DeliveryReceipt> {
    let result: DurableToolResult = serde_json::from_value(value.clone()).ok()?;
    result.validate().ok()?;
    let value = result.structured_content().value()?;
    if value.get("schema_version")?.as_u64()? != 1
        || value.get("delivery")?.as_str()? != "local_conversation"
        || !value.get("local_only")?.as_bool()?
        || value.get("remote_sent")?.as_bool()?
        || !matches!(value.get("status")?.as_str()?, "normal" | "proactive")
    {
        return None;
    }
    let display = value.get("display")?.as_str()?;
    if !matches!(display, "attach" | "render") {
        return None;
    }
    let rows = value.get("files")?.as_array()?;
    if rows.is_empty() || rows.len() > 8 {
        return None;
    }
    let blobs = result
        .blocks()
        .iter()
        .filter_map(|block| match block {
            DurableToolResultBlock::EmbeddedBlob {
                uri,
                media,
                resource_extensions,
                metadata,
            } => Some((uri, media, resource_extensions, metadata)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if blobs.len() != rows.len() {
        return None;
    }
    let mut files = Vec::with_capacity(rows.len());
    let mut total = 0_u64;
    for (position, (row, (uri, media, extensions, metadata))) in rows.iter().zip(blobs).enumerate()
    {
        let index = position as u64 + 1;
        let path = row.get("path")?.as_str()?;
        if path.is_empty()
            || path.len() > 4096
            || path.chars().any(char::is_control)
            || std::path::Path::new(path).is_absolute()
            || std::path::Path::new(path)
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            || row.get("index")?.as_u64()? != index
            || extensions.get("heycode_user_file_index")?.as_u64()? != index
            || row.get("resource_uri")?.as_str()? != uri
            || row.get("content_id")?.as_str()? != media.attachment.content_id().as_str()
            || format!("sha256-{}", row.get("revision")?.as_str()?)
                != media.attachment.content_id().as_str()
            || row.get("byte_len")?.as_u64()? != media.attachment.byte_len()
            || media.attachment.byte_len() == 0
            || media.attachment.byte_len() > 8 * 1024 * 1024
            || metadata.annotations.audience() != [ToolResultAudience::User]
        {
            return None;
        }
        total = total.checked_add(media.attachment.byte_len())?;
        if total > 16 * 1024 * 1024 {
            return None;
        }
        files.push(DeliveredFile {
            path: path.to_owned(),
            metadata: media.attachment.clone(),
        });
    }
    let caption = match value.get("caption") {
        None => None,
        Some(value) => {
            let text = value.as_str()?;
            if text.len() > 512 || text.chars().any(char::is_control) {
                return None;
            }
            Some(text.to_owned())
        }
    };
    Some(DeliveryReceipt {
        files,
        caption,
        display: display.to_owned(),
    })
}

pub(crate) fn plain_lines(
    result: Option<&(bool, Value)>,
    expanded: bool,
    status: &str,
) -> Vec<String> {
    let Some((ok, value)) = result else {
        return vec![
            format!("Local file delivery · {status}"),
            "Preparing files for this conversation.".to_owned(),
        ];
    };
    if !ok {
        let error = value
            .get("message")
            .or_else(|| value.get("error"))
            .and_then(Value::as_str)
            .or_else(|| value.as_str())
            .unwrap_or("File preparation failed; no delivery receipt was committed.");
        return vec![
            format!("Local file delivery · {status}"),
            crate::markdown::terminal_safe_span(&error.chars().take(512).collect::<String>())
                .into_owned(),
        ];
    }
    let Some(receipt) = receipt(value) else {
        return vec![
            "Local file delivery · receipt unavailable".to_owned(),
            "No complete admitted attachment receipt is available.".to_owned(),
        ];
    };
    let mut lines = vec![
        format!(
            "Local files ({} {}) · stored",
            receipt.files.len(),
            if receipt.files.len() == 1 {
                "file"
            } else {
                "files"
            }
        ),
        "Stored in this conversation. No remote delivery.".to_owned(),
    ];
    if let Some(caption) = receipt.caption {
        lines.push(caption);
    }
    for file in receipt.files {
        lines.push(format!(
            "  {} · {} bytes · {}",
            file.path,
            file.metadata.byte_len(),
            file.metadata.media_type().as_str()
        ));
        if expanded {
            lines.push(format!(
                "    attachment: {}",
                file.metadata.content_id().as_str()
            ));
        }
    }
    if expanded {
        lines.push(format!("Display requested: {}. Source paths are provenance; saving uses verified stored bytes.", receipt.display));
    }
    lines.push("Enter: details · w: save local files · Esc: compose".to_owned());
    lines
}

pub(crate) fn draw_lines(
    result: Option<&(bool, Value)>,
    expanded: bool,
    focused: bool,
    status: &str,
    width: usize,
    styles: crate::terminal::Styles,
) -> Vec<Line<'static>> {
    let mut output = Vec::new();
    for (index, line) in plain_lines(result, expanded, status)
        .into_iter()
        .enumerate()
    {
        let line = if index == 0 {
            format!("{} {line}", if expanded { "▾" } else { "▸" })
        } else {
            format!("  {line}")
        };
        let style = if index == 0 {
            Style::default()
                .fg(if focused {
                    styles.accent()
                } else {
                    styles.text()
                })
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            Style::default().fg(styles.dim())
        };
        output.extend(crate::markdown::wrap_styled(
            &[Span::styled(line, style)],
            width.max(1),
            2,
        ));
    }
    output
}

/// Read verified content-addressed bytes; never read a retained source pathname.
pub(crate) fn save_files(
    store: &heycode_attachments::AttachmentStore,
    files: &[DeliveredFile],
    cancellation: tokio_util::sync::CancellationToken,
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    use std::io::Write;
    anyhow::ensure!(
        !files.is_empty() && files.len() <= 8,
        "Invalid delivery file count"
    );
    let directory = tempfile::Builder::new()
        .prefix("heycode-files-")
        .tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    let mut names = Vec::new();
    let mut total = 0_usize;
    for (index, file) in files.iter().enumerate() {
        anyhow::ensure!(!cancellation.is_cancelled(), "File save cancelled");
        let bytes = store.read(&file.metadata, cancellation.child_token())?;
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| anyhow::anyhow!("File save size overflow"))?;
        anyhow::ensure!(
            bytes.len() <= 8 * 1024 * 1024 && total <= 16 * 1024 * 1024,
            "Files exceed the local delivery limit"
        );
        let basename = std::path::Path::new(&file.path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file");
        let safe = basename
            .chars()
            .take(100)
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let name = format!(
            "{}-{}",
            index + 1,
            if safe.is_empty() { "file" } else { &safe }
        );
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(directory.path().join(&name))?;
        output.write_all(&bytes)?;
        output.sync_all()?;
        names.push(name);
    }
    anyhow::ensure!(!cancellation.is_cancelled(), "File save cancelled");
    let root = directory.keep();
    Ok(names.into_iter().map(|name| root.join(name)).collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use heycode_attachments::{
        AttachmentInput, AttachmentStore, AttachmentStoreConfig, local_attachment_plugin,
    };
    use heycode_core::{
        ToolResultAnnotations, ToolResultBlockMetadata, ToolResultMediaReference,
        ToolResultSchemaCheck, ToolStructuredContent,
    };
    use tokio_util::sync::CancellationToken;

    fn fixture() -> (
        tempfile::TempDir,
        heycode_core::Context,
        std::sync::Arc<AttachmentStore>,
        Value,
    ) {
        let root = tempfile::tempdir().unwrap();
        let context = heycode_core::compose(&[
            heycode_session::session_plugin(root.path().join("sessions")),
            local_attachment_plugin(
                AttachmentStoreConfig::new(root.path().join("objects"), 1024 * 1024).unwrap(),
            ),
        ])
        .unwrap();
        let store = context
            .get::<AttachmentStore>(heycode_attachments::SERVICE_ATTACHMENTS)
            .unwrap();
        let metadata = store
            .admit(
                AttachmentInput::new(b"retained exact bytes".to_vec(), None, None).unwrap(),
                CancellationToken::new(),
            )
            .unwrap()
            .metadata()
            .clone();
        let id = metadata.content_id().as_str().to_owned();
        let receipt = serde_json::json!({"schema_version":1,"delivery":"local_conversation","local_only":true,"remote_sent":false,"status":"normal","display":"attach","caption":"Prepared document","files":[{"index":1,"path":"deleted/document.txt","revision":id.strip_prefix("sha256-").unwrap(),"content_id":id,"byte_len":metadata.byte_len(),"resource_uri":"heycode://local-user-file/1"}]});
        let rich = DurableToolResult::new(
            vec![DurableToolResultBlock::EmbeddedBlob {
                uri: "heycode://local-user-file/1".to_owned(),
                media: ToolResultMediaReference {
                    attachment: metadata,
                    declared_media_type: None,
                },
                resource_extensions: serde_json::json!({"heycode_user_file_index":1})
                    .as_object()
                    .unwrap()
                    .clone(),
                metadata: ToolResultBlockMetadata::new(
                    ToolResultAnnotations::new(
                        vec![ToolResultAudience::User],
                        None,
                        None,
                        Default::default(),
                    )
                    .unwrap(),
                    Default::default(),
                )
                .unwrap(),
            }],
            ToolStructuredContent::Present(receipt),
            ToolResultSchemaCheck::NoSchema,
            Default::default(),
        )
        .unwrap();
        (root, context, store, serde_json::to_value(rich).unwrap())
    }

    #[test]
    fn delivery_card_requires_matching_admitted_refs_and_preserves_local_scope() {
        let (_root, mut context, _store, value) = fixture();
        assert_eq!(receipt(&value).unwrap().files.len(), 1);
        let lines = plain_lines(Some(&(true, value.clone())), true, "completed").join("\n");
        assert!(lines.contains("Stored in this conversation. No remote delivery."));
        assert!(lines.contains("deleted/document.txt"));
        assert!(lines.contains("text/plain"));
        assert!(receipt(&value["structuredContent"]["value"]).is_none());
        for (field, replacement) in [
            (
                "content_id",
                Value::String(format!("sha256-{}", "a".repeat(64))),
            ),
            ("path", Value::String("../ambient.txt".to_owned())),
            ("byte_len", Value::from(1)),
        ] {
            let mut changed = value.clone();
            changed["structuredContent"]["value"]["files"][0][field] = replacement;
            assert!(receipt(&changed).is_none());
        }
        let failed = plain_lines(Some(&(false, Value::Null)), false, "failed").join("\n");
        assert!(!failed.contains("· stored"));
        context.shutdown();
    }

    #[test]
    fn saving_delivered_files_uses_retained_bytes_without_a_source_path() {
        let (root, mut context, store, value) = fixture();
        assert!(!root.path().join("deleted/document.txt").exists());
        let receipt = receipt(&value).unwrap();
        let paths = save_files(&store, &receipt.files, CancellationToken::new()).unwrap();
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"retained exact bytes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(paths[0].parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&paths[0]).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(paths[0].parent().unwrap()).unwrap();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(save_files(&store, &receipt.files, cancelled).is_err());
        context.shutdown();
        assert!(save_files(&store, &receipt.files, CancellationToken::new()).is_err());
    }
}
