//! File tools must be Consumers of the replaceable filesystem Provider.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_exec::{
    EditFileOutput, EditFileSpec, FileMetadata, FileSystemBackend, FileSystemError,
    FileSystemPolicy, FileSystemRoot, FileSystemRootAccess, FileSystemService, GlobOutput,
    GlobSpec, GrepOutput, GrepSpec, ObservationLog, PathRequest, ReadFileOutput, ReadFileSpec,
    ResolvedPath, WriteFileSpec,
};
use heycode_tools::{ToolCtx, ToolsConfig, builtin_tools};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct RecordingBackend {
    local: FileSystemService,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

impl RecordingBackend {
    fn record(&self, operation: &'static str) {
        self.calls.lock().expect("call log").push(operation);
    }
}

#[async_trait::async_trait]
impl FileSystemBackend for RecordingBackend {
    fn resolve(&self, request: PathRequest) -> Result<ResolvedPath, FileSystemError> {
        self.record("resolve");
        self.local.resolve(request)
    }

    fn observations(&self) -> ObservationLog {
        self.local.observations()
    }

    fn policy(&self) -> FileSystemPolicy {
        self.local.policy()
    }

    async fn metadata(
        &self,
        path: ResolvedPath,
        cancellation: CancellationToken,
    ) -> Result<FileMetadata, FileSystemError> {
        self.record("metadata");
        self.local.metadata(path, cancellation).await
    }

    async fn create_dir_all(
        &self,
        path: ResolvedPath,
        cancellation: CancellationToken,
    ) -> Result<(), FileSystemError> {
        self.record("create_dir_all");
        self.local.create_dir_all(path, cancellation).await
    }

    async fn read(
        &self,
        spec: ReadFileSpec,
        cancellation: CancellationToken,
    ) -> Result<ReadFileOutput, FileSystemError> {
        self.record("read");
        self.local.read(spec, cancellation).await
    }

    async fn write(
        &self,
        spec: WriteFileSpec,
        cancellation: CancellationToken,
    ) -> Result<(), FileSystemError> {
        self.record("write");
        self.local.write(spec, cancellation).await
    }

    async fn edit(
        &self,
        spec: EditFileSpec,
        cancellation: CancellationToken,
    ) -> Result<EditFileOutput, FileSystemError> {
        self.record("edit");
        self.local.edit(spec, cancellation).await
    }

    async fn write_checked(
        &self,
        spec: heycode_exec::CheckedWriteSpec,
        cancellation: CancellationToken,
    ) -> Result<heycode_exec::CheckedWriteOutput, FileSystemError> {
        self.record("write_checked");
        self.local.write_checked(spec, cancellation).await
    }

    async fn edit_many(
        &self,
        spec: heycode_exec::MultiEditSpec,
        cancellation: CancellationToken,
    ) -> Result<heycode_exec::MultiEditOutput, FileSystemError> {
        self.record("edit_many");
        self.local.edit_many(spec, cancellation).await
    }

    async fn glob(
        &self,
        spec: GlobSpec,
        cancellation: CancellationToken,
    ) -> Result<GlobOutput, FileSystemError> {
        self.record("glob");
        self.local.glob(spec, cancellation).await
    }

    async fn grep(
        &self,
        spec: GrepSpec,
        cancellation: CancellationToken,
    ) -> Result<GrepOutput, FileSystemError> {
        self.record("grep");
        self.local.grep(spec, cancellation).await
    }
}

#[tokio::test]
async fn every_file_tool_dispatches_through_the_injected_provider() {
    let dir = tempfile::tempdir().expect("temporary directory");
    std::fs::write(dir.path().join("file.txt"), "hello world\n").expect("seed file");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let policy =
        FileSystemPolicy::new([
            FileSystemRoot::new(dir.path(), FileSystemRootAccess::ReadWrite).expect("root"),
        ])
        .expect("policy");
    let filesystem = FileSystemService::new(Arc::new(RecordingBackend {
        local: FileSystemService::local(policy).expect("local filesystem"),
        calls: Arc::clone(&calls),
    }));
    let shell = heycode_exec::ShellService::local(
        heycode_exec::LocalShellConfig::platform(
            dir.path().to_path_buf(),
            std::time::Duration::from_secs(1),
        )
        .expect("shell config"),
    );
    let (tools, _) = builtin_tools(
        &ToolsConfig {
            web_enabled: false,
            ..ToolsConfig::default()
        },
        filesystem,
        shell,
    );
    let context = ToolCtx {
        cwd: dir.path().to_path_buf(),
        ..Default::default()
    };

    tools
        .get("read")
        .expect("read")
        .run(json!({"path": "file.txt"}), &context)
        .await
        .expect("read result");
    tools
        .get("write")
        .expect("write")
        .run(json!({"path": "new.txt", "content": "new\n"}), &context)
        .await
        .expect("write result");
    tools
        .get("edit")
        .expect("edit")
        .run(
            json!({"path": "file.txt", "old_string": "world", "new_string": "rust"}),
            &context,
        )
        .await
        .expect("edit result");
    tools
        .get("glob")
        .expect("glob")
        .run(json!({"pattern": "*.txt"}), &context)
        .await
        .expect("glob result");
    tools
        .get("grep")
        .expect("grep")
        .run(json!({"pattern": "rust"}), &context)
        .await
        .expect("grep result");

    assert_eq!(
        *calls.lock().expect("call log"),
        [
            "resolve",
            "read",
            "resolve",
            "write_checked",
            "resolve",
            "edit_many",
            "resolve",
            "glob",
            "resolve",
            "grep"
        ]
    );
}
