//! Shared durable work tools. One session lock owns revision admission and append.

use heycode_core::{Context, CoreError, CoreResult, Plugin, ToolSpec};
use heycode_session::{
    Session, SessionEventKind, WorkChange, WorkError, WorkItem, WorkItemFields, WorkItemId,
    WorkProjection, WorkScope, WorkStatus, project_work_items,
};
use heycode_tools::{Tool, ToolCtx, ToolError, ToolRegistry};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

/// Session-scoped durable work owner.
pub const SERVICE_WORK: heycode_core::ServiceKey = heycode_core::ServiceKey::new("work");

/// Work admission or persistence failure.
#[derive(Debug, thiserror::Error)]
pub enum WorkServiceError {
    /// Invalid or conflicting work operation.
    #[error(transparent)]
    Domain(#[from] WorkError),
    /// Team authority or lifecycle refused the shared-work operation.
    #[error(transparent)]
    Team(#[from] crate::TeamError),
    /// Service lifecycle or session lock is unavailable.
    #[error("work service is unavailable")]
    Unavailable,
    /// An append/flush failed; committed state must be inspected before another mutation.
    #[error("work persistence outcome is uncertain; read the item before retrying")]
    Persistence,
}

/// A board shared by the parent and its native children.
pub struct WorkService {
    session: Arc<Mutex<Session>>,
    disposed: AtomicBool,
    teams: Option<Weak<crate::TeamService>>,
}
impl WorkService {
    /// Bind an existing durable session. Creating a service does not create work.
    #[must_use]
    pub fn new(session: Arc<Mutex<Session>>) -> Self {
        Self {
            session,
            disposed: AtomicBool::new(false),
            teams: None,
        }
    }
    /// Attach the existing team owner; no copied work state or second runtime is created.
    #[must_use]
    pub fn with_teams(mut self, teams: &Arc<crate::TeamService>) -> Self {
        self.teams = Some(Arc::downgrade(teams));
        self
    }
    fn team_service(&self) -> Result<Arc<crate::TeamService>, WorkServiceError> {
        self.teams
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or(WorkServiceError::Unavailable)
    }
    fn visible(&self, item: &WorkItem) -> Result<bool, WorkServiceError> {
        match item.scope() {
            WorkScope::Session => Ok(true),
            WorkScope::Team(team_id) => {
                if self.teams.is_none() {
                    return Ok(false);
                }
                match self.team_service()?.work_view(team_id) {
                    Ok(_) => Ok(true),
                    Err(error)
                        if matches!(
                            error.code(),
                            crate::TeamErrorCode::Refused | crate::TeamErrorCode::Unknown
                        ) =>
                    {
                        Ok(false)
                    }
                    Err(error) => Err(error.into()),
                }
            }
        }
    }

    /// Read authoritative work, including tombstones.
    pub fn snapshot(&self) -> Result<WorkProjection, WorkServiceError> {
        if self.disposed.load(Ordering::Acquire) {
            return Err(WorkServiceError::Unavailable);
        }
        let session = self
            .session
            .lock()
            .map_err(|_| WorkServiceError::Unavailable)?;
        Ok(project_work_items(session.events())?)
    }
    /// Create once using a caller-owned key, returning the latest record on an identical retry.
    pub fn create(
        &self,
        scope: WorkScope,
        request_key: &str,
        fields: WorkItemFields,
    ) -> Result<WorkItem, WorkServiceError> {
        if self.disposed.load(Ordering::Acquire) {
            return Err(WorkServiceError::Unavailable);
        }
        if let WorkScope::Team(team_id) = &scope {
            return Ok(self
                .team_service()?
                .create_work(team_id, request_key, fields)?);
        }
        let change = WorkChange::create(scope, request_key, fields)?;
        self.commit(|_| Ok(change))
    }
    /// Replace an item only if the supplied item revision is still current.
    pub fn update(
        &self,
        id: &WorkItemId,
        revision: u64,
        fields: WorkItemFields,
    ) -> Result<WorkItem, WorkServiceError> {
        let board = self.snapshot()?;
        let item = board.get(id).ok_or(WorkError::Unknown)?;
        if let WorkScope::Team(team_id) = item.scope() {
            return Ok(self.team_service()?.update_work(
                team_id,
                item.team_task_id().ok_or(WorkError::Invalid)?,
                revision,
                fields,
            )?);
        }
        self.commit(|board| {
            let current = board.get(id).ok_or(WorkError::Unknown)?;
            if current.scope() != &WorkScope::Session {
                return Err(WorkError::Scope);
            }
            WorkChange::update(current, revision, fields)
        })
    }
    fn commit(
        &self,
        prepare: impl FnOnce(&WorkProjection) -> Result<WorkChange, WorkError>,
    ) -> Result<WorkItem, WorkServiceError> {
        if self.disposed.load(Ordering::Acquire) {
            return Err(WorkServiceError::Unavailable);
        }
        let mut session = self
            .session
            .lock()
            .map_err(|_| WorkServiceError::Unavailable)?;
        if self.disposed.load(Ordering::Acquire) {
            return Err(WorkServiceError::Unavailable);
        }
        let mut board = project_work_items(session.events())?;
        let change = prepare(&board)?;
        let inserted = board.apply(&change)?;
        let item = board
            .get(change.item().id())
            .ok_or(WorkError::Unknown)?
            .clone();
        if inserted {
            session
                .append(SessionEventKind::WorkChange {
                    change: Box::new(change),
                })
                .map_err(|_| WorkServiceError::Persistence)?;
            session.flush().map_err(|_| WorkServiceError::Persistence)?;
        }
        Ok(item)
    }
    /// Stop future admission when the owning context is disposed.
    pub fn dispose(&self) {
        self.disposed.store(true, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
enum Operation {
    Create,
    Get,
    List,
    Update,
}
impl Operation {
    const fn name(self) -> &'static str {
        match self {
            Self::Create => "task_create",
            Self::Get => "task_get",
            Self::List => "task_list",
            Self::Update => "task_update",
        }
    }
}
struct WorkTool {
    service: Arc<WorkService>,
    operation: Operation,
}

#[async_trait::async_trait]
impl Tool for WorkTool {
    fn effect(&self) -> heycode_tools::ToolEffect {
        match self.operation {
            Operation::Get | Operation::List => heycode_tools::ToolEffect::ReadOnly,
            _ => heycode_tools::ToolEffect::Mutates,
        }
    }
    fn spec(&self) -> ToolSpec {
        let fields = json!({
            "subject":{"type":"string","minLength":1,"maxLength":256},
            "description":{"type":"string","maxLength":16384},
            "status":{"type":"string","enum":["pending","in_progress","blocked","completed","failed","cancelled","deleted"]},
            "owner":{"type":["string","null"],"maxLength":128},
            "dependencies":{"type":"array","items":{"type":"string"},"maxItems":64,"uniqueItems":true},
            "metadata":{"type":"object","maxProperties":64}
        });
        let (description, properties, required) = match self.operation {
            Operation::Create => {
                let mut properties = fields.as_object().cloned().unwrap_or_default();
                if self.service.teams.is_some() {
                    properties.insert("team_id".into(), json!({"type":"string","description":"Optional team scope. Only its lead can create team work; it starts pending."}));
                }
                properties.insert("request_key".into(), json!({"type":"string","minLength":1,"maxLength":128,"description":"Stable idempotency key. Retry the same create with the same key and payload."}));
                (
                    "Create a durable work item. Work is separate from agents and shell jobs. Multiple items can be active. Dependencies must name existing work in this session; starting or completing requires completed prerequisites.",
                    Value::Object(properties),
                    vec!["request_key", "subject"],
                )
            }
            Operation::Get => (
                "Read a durable work item, its exact revision, deletion tombstone, blocked_by prerequisites and a bounded page of blocks dependents. Follow blocks_next_cursor with blocks_after for additional dependents.",
                json!({"id":{"type":"string"},"blocks_after":{"type":"string","description":"Continue dependent IDs after blocks_next_cursor."},"blocks_limit":{"type":"integer","minimum":1,"maximum":200,"default":64}}),
                vec!["id"],
            ),
            Operation::List => (
                "List bounded summaries of this session's structured work, excluding agents and processes. Use task_get for descriptions, metadata and dependencies. Follow next_cursor until null. Deleted entries are hidden unless requested.",
                json!({"include_deleted":{"type":"boolean"},"limit":{"type":"integer","minimum":1,"maximum":200},"after":{"type":"string","description":"Continue after the next_cursor from a previous page."}}),
                vec![],
            ),
            Operation::Update => {
                let mut properties = fields.as_object().cloned().unwrap_or_default();
                properties.insert("id".into(), json!({"type":"string"}));
                properties.insert("expected_revision".into(), json!({"type":"integer","minimum":1,"description":"Exact revision returned by task_get/list. Stale updates fail; reread before deciding how to retry."}));
                (
                    "Update provided work fields atomically against expected_revision. Omitted fields are preserved; owner:null unassigns, dependencies:[] clears prerequisites. Status deleted makes a durable tombstone; deletion refuses live dependents.",
                    Value::Object(properties),
                    vec!["id", "expected_revision"],
                )
            }
        };
        ToolSpec {
            name: self.operation.name().into(),
            description: description.into(),
            parameters: json!({"type":"object","additionalProperties":false,"properties":properties,"required":required}),
        }
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        if cx.cancellation.is_cancelled() {
            return Err(ToolError::new("work operation cancelled"));
        }
        let object = args
            .as_object()
            .ok_or_else(|| ToolError::new("work arguments must be an object"))?;
        let allowed = self.spec().parameters["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        if object.keys().any(|key| !allowed.contains_key(key)) {
            return Err(ToolError::new("unknown work argument"));
        }
        let result = match self.operation {
            Operation::Create => {
                let request_key = required_text(&args, "request_key")?;
                let mut fields = object.clone();
                fields.remove("request_key");
                let scope = match fields.remove("team_id") {
                    Some(Value::String(id)) => WorkScope::Team(
                        heycode_session::TeamId::new(id)
                            .map_err(|_| ToolError::new("invalid team_id"))?,
                    ),
                    Some(_) => return Err(ToolError::new("team_id must be a string")),
                    None => WorkScope::Session,
                };
                fields
                    .entry("status".to_owned())
                    .or_insert(json!("pending"));
                let fields: WorkItemFields = serde_json::from_value(Value::Object(fields))
                    .map_err(|_| ToolError::new("invalid work fields"))?;
                self.service
                    .create(scope, request_key, fields)
                    .map(|item| json!(item))
            }
            Operation::Get => {
                let id = WorkItemId::new(required_text(&args, "id")?).map_err(domain_error)?;
                let after = args
                    .get("blocks_after")
                    .map(|_| required_text(&args, "blocks_after"))
                    .transpose()?;
                if let Some(after) = after {
                    WorkItemId::new(after).map_err(domain_error)?;
                }
                let limit = match args.get("blocks_limit") {
                    Some(value) => value
                        .as_u64()
                        .filter(|n| (1..=200).contains(n))
                        .ok_or_else(|| ToolError::new("blocks_limit must be between 1 and 200"))?
                        as usize,
                    None => 64,
                };
                self.service.snapshot().and_then(|board| {
                    let item = board.get(&id).ok_or(WorkError::Unknown)?;
                    if !self.service.visible(item)? {
                        return Err(WorkError::Unknown.into());
                    }
                    let mut dependents = board.items().filter(|other| {
                        other.scope() == item.scope()
                            && other.fields().status != WorkStatus::Deleted
                            && other.fields().dependencies.contains(&id)
                            && after.is_none_or(|after| other.id().as_str() > after)
                    });
                    let blocks: Vec<_> = dependents
                        .by_ref()
                        .take(limit)
                        .map(|other| other.id())
                        .collect();
                    let next = if dependents.next().is_some() {
                        blocks.last().copied()
                    } else {
                        None
                    };
                    let mut receipt = json!(item);
                    receipt["blocked_by"] = json!(item.fields().dependencies);
                    receipt["blocks"] = json!(blocks);
                    receipt["blocks_next_cursor"] = json!(next);
                    Ok(receipt)
                })
            }
            Operation::List => {
                let include_deleted = match args.get("include_deleted") {
                    Some(value) => value
                        .as_bool()
                        .ok_or_else(|| ToolError::new("include_deleted must be boolean"))?,
                    None => false,
                };
                let limit = match args.get("limit") {
                    Some(value) => value
                        .as_u64()
                        .filter(|limit| (1..=200).contains(limit))
                        .ok_or_else(|| ToolError::new("limit must be between 1 and 200"))?
                        as usize,
                    None => 100,
                };
                let after = args
                    .get("after")
                    .map(|_| required_text(&args, "after"))
                    .transpose()?;
                self.service.snapshot().and_then(|board| {
                    let visible_teams = match &self.service.teams { Some(_) => self.service.team_service()?.visible_work_teams()?, None => Default::default() };
                    let mut items = Vec::new();
                    let mut bytes = 0;
                    let mut more = false;
                    for item in board.items().filter(|item| (include_deleted || item.fields().status != WorkStatus::Deleted)
                        && after.is_none_or(|after| item.id().as_str() > after)) {
                        if let WorkScope::Team(team_id) = item.scope() && !visible_teams.contains(team_id) { continue; }
                        let summary = json!({"id":item.id(),"scope":item.scope(),"revision":item.revision(),"subject":item.fields().subject,"status":item.fields().status,"owner":item.fields().owner,"dependency_count":item.fields().dependencies.len()});
                        let size = summary.to_string().len();
                        if items.len() >= limit || bytes + size > 24 * 1024 { more = true; break; }
                        bytes += size;
                        items.push(summary);
                    }
                    let cursor = if more { items.last().and_then(|item| item.get("id")).cloned() } else { None };
                    Ok(json!({"items":items,"next_cursor":cursor}))
                })
            }
            Operation::Update => {
                let id = WorkItemId::new(required_text(&args, "id")?).map_err(domain_error)?;
                let revision = args
                    .get("expected_revision")
                    .and_then(Value::as_u64)
                    .filter(|revision| *revision > 0)
                    .ok_or_else(|| {
                        ToolError::new("expected_revision must be a positive integer")
                    })?;
                let board = self.service.snapshot().map_err(service_error)?;
                let current = board
                    .get(&id)
                    .ok_or_else(|| domain_error(WorkError::Unknown))?;
                if !self.service.visible(current).map_err(service_error)? {
                    return Err(domain_error(WorkError::Unknown));
                }
                let mut fields = serde_json::to_value(current.fields())
                    .map_err(|_| ToolError::new("work fields unavailable"))?;
                let target = fields
                    .as_object_mut()
                    .ok_or_else(|| ToolError::new("work fields unavailable"))?;
                for (key, value) in object {
                    if !matches!(key.as_str(), "id" | "expected_revision") {
                        target.insert(key.clone(), value.clone());
                    }
                }
                let fields = serde_json::from_value(fields)
                    .map_err(|_| ToolError::new("invalid work fields"))?;
                self.service
                    .update(&id, revision, fields)
                    .map(|item| json!(item))
            }
        };
        result.map_err(service_error)
    }
}
fn required_text<'a>(args: &'a Value, field: &str) -> Result<&'a str, ToolError> {
    args.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new(format!("{field} must be a string")))
}
fn domain_error(error: WorkError) -> ToolError {
    ToolError::new(error.to_string())
}
fn service_error(error: WorkServiceError) -> ToolError {
    ToolError::new(error.to_string())
}

/// Install durable work and its four model tools using effect-owned registrations.
#[must_use]
pub fn work_plugin() -> Box<dyn Plugin> {
    build_work_plugin(false)
}

/// Install work with the existing team service as its shared-work adapter.
#[must_use]
pub fn work_plugin_with_teams() -> Box<dyn Plugin> {
    build_work_plugin(true)
}

fn build_work_plugin(teams: bool) -> Box<dyn Plugin> {
    struct WorkPlugin {
        teams: bool,
    }
    impl Plugin for WorkPlugin {
        fn name(&self) -> &'static str {
            "work"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Tool,
                ],
            )
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            [
                Operation::Create,
                Operation::Get,
                Operation::List,
                Operation::Update,
            ]
            .into_iter()
            .map(|op| {
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Tool,
                    op.name(),
                )
            })
            .collect()
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            if self.teams {
                &[
                    heycode_session::SERVICE_SESSION,
                    heycode_tools::SERVICE_TOOLS,
                    crate::SERVICE_TEAMS,
                ]
            } else {
                &[
                    heycode_session::SERVICE_SESSION,
                    heycode_tools::SERVICE_TOOLS,
                ]
            }
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_WORK]
        }
        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let session = context
                .get::<Mutex<Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| CoreError::other("work session missing"))?;
            let tools = context
                .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                .ok_or_else(|| CoreError::other("work tools missing"))?;
            let mut service = WorkService::new(session);
            if self.teams {
                service = service.with_teams(
                    &context
                        .get::<crate::TeamService>(crate::SERVICE_TEAMS)
                        .ok_or_else(|| CoreError::other("team work adapter missing"))?,
                );
            }
            context.provide(SERVICE_WORK, self.name(), service)?;
            let service = context
                .get::<WorkService>(SERVICE_WORK)
                .ok_or_else(|| CoreError::other("work service missing"))?;
            for operation in [
                Operation::Create,
                Operation::Get,
                Operation::List,
                Operation::Update,
            ] {
                let registration = tools
                    .register_owned(Arc::new(WorkTool {
                        service: service.clone(),
                        operation,
                    }))
                    .map_err(|error| CoreError::other(error.to_string()))?;
                context.effect(move || drop(registration));
            }
            context.effect(move || service.dispose());
            Ok(())
        }
    }
    Box::new(WorkPlugin { teams })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    fn fields(subject: &str) -> WorkItemFields {
        WorkItemFields {
            subject: subject.into(),
            description: String::new(),
            status: WorkStatus::Pending,
            owner: None,
            dependencies: Vec::new(),
            metadata: Default::default(),
        }
    }
    #[test]
    fn concurrent_work_updates_are_atomic_and_reopen_preserves_the_board() {
        let root = tempfile::tempdir().unwrap();
        let session = Session::create(root.path()).unwrap();
        let directory = root.path().join(session.id().as_str());
        let session = Arc::new(Mutex::new(session));
        let service = Arc::new(WorkService::new(session.clone()));
        let a = service
            .create(WorkScope::Session, "a", fields("First"))
            .unwrap();
        let b = service
            .create(WorkScope::Session, "b", fields("Second"))
            .unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let handles = [a.clone(), b.clone()]
            .into_iter()
            .map(|item| {
                let service = service.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut updated = item.fields().clone();
                    updated.status = WorkStatus::InProgress;
                    barrier.wait();
                    service.update(item.id(), 1, updated).unwrap()
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(
            service
                .snapshot()
                .unwrap()
                .items()
                .filter(|item| item.fields().status == WorkStatus::InProgress)
                .count(),
            2
        );
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let handles = ["winner one", "winner two"]
            .into_iter()
            .map(|subject| {
                let service = service.clone();
                let barrier = barrier.clone();
                let id = a.id().clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    service.update(&id, 2, fields(subject))
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(WorkServiceError::Domain(WorkError::Conflict))
                ))
                .count(),
            1
        );
        let before_count = session.lock().unwrap().events().len();
        let retry = service
            .create(WorkScope::Session, "a", fields("First"))
            .unwrap();
        assert_eq!(retry.revision(), 3);
        assert_eq!(session.lock().unwrap().events().len(), before_count);
        let expected = service.snapshot().unwrap();
        drop(service);
        drop(session);
        let reopened = Session::open(directory).unwrap();
        assert_eq!(project_work_items(reopened.events()).unwrap(), expected);
    }
    #[tokio::test]
    async fn tool_patch_preserves_omitted_fields_and_refuses_stale_or_invalid_input() {
        let root = tempfile::tempdir().unwrap();
        let service = Arc::new(WorkService::new(Arc::new(Mutex::new(
            Session::create(root.path()).unwrap(),
        ))));
        let cx = ToolCtx::default();
        let create = WorkTool {
            service: service.clone(),
            operation: Operation::Create,
        };
        let update = WorkTool {
            service: service.clone(),
            operation: Operation::Update,
        };
        let list = WorkTool {
            service: service.clone(),
            operation: Operation::List,
        };
        let item = create.run(json!({"request_key":"tool","subject":"Keep title","description":"Keep detail","owner":"agent-a"}), &cx).await.unwrap();
        let id = item["id"].as_str().unwrap();
        let changed = update
            .run(
                json!({"id":id,"expected_revision":1,"owner":null,"status":"in_progress"}),
                &cx,
            )
            .await
            .unwrap();
        assert_eq!(changed["fields"]["subject"], "Keep title");
        assert_eq!(changed["fields"]["description"], "Keep detail");
        assert!(changed["fields"]["owner"].is_null());
        assert!(
            update
                .run(
                    json!({"id":id,"expected_revision":1,"subject":"stale"}),
                    &cx
                )
                .await
                .is_err()
        );
        assert!(
            create
                .run(
                    json!({"request_key":"tool2","subject":"Bad","unknown":true}),
                    &cx
                )
                .await
                .is_err()
        );
        let deleted = update
            .run(
                json!({"id":id,"expected_revision":2,"status":"deleted"}),
                &cx,
            )
            .await
            .unwrap();
        assert_eq!(deleted["revision"], 3);
        assert_eq!(list.run(json!({}), &cx).await.unwrap()["items"], json!([]));
        assert_eq!(
            list.run(json!({"include_deleted":true}), &cx)
                .await
                .unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        service.dispose();
        assert!(list.run(json!({}), &cx).await.is_err());
    }
    #[tokio::test]
    async fn reverse_dependencies_are_paged_and_exclude_tombstones() {
        let root = tempfile::tempdir().unwrap();
        let service = Arc::new(WorkService::new(Arc::new(Mutex::new(
            Session::create(root.path()).unwrap(),
        ))));
        let a = service
            .create(WorkScope::Session, "parent", fields("Prerequisite"))
            .unwrap();
        let mut dependent = fields("Dependent");
        dependent.dependencies.push(a.id().clone());
        let b = service
            .create(WorkScope::Session, "child-b", dependent.clone())
            .unwrap();
        let c = service
            .create(WorkScope::Session, "child-c", dependent)
            .unwrap();
        let get = WorkTool {
            service: service.clone(),
            operation: Operation::Get,
        };
        let cx = ToolCtx::default();
        let first = get
            .run(json!({"id":a.id(),"blocks_limit":1}), &cx)
            .await
            .unwrap();
        assert_eq!(first["blocks"].as_array().unwrap().len(), 1);
        assert!(first["blocked_by"].as_array().unwrap().is_empty());
        let next = get
            .run(
                json!({"id":a.id(),"blocks_limit":1,"blocks_after":first["blocks_next_cursor"]}),
                &cx,
            )
            .await
            .unwrap();
        assert_eq!(next["blocks"].as_array().unwrap().len(), 1);
        assert_ne!(first["blocks"][0], next["blocks"][0]);
        assert!(next["blocks_next_cursor"].is_null());
        assert_eq!(
            get.run(json!({"id":b.id()}), &cx).await.unwrap()["blocked_by"],
            json!([a.id()])
        );
        let mut deleted = c.fields().clone();
        deleted.status = WorkStatus::Deleted;
        service.update(c.id(), c.revision(), deleted).unwrap();
        let remaining = get.run(json!({"id":a.id()}), &cx).await.unwrap();
        assert_eq!(remaining["blocks"], json!([b.id()]));
        assert_eq!(
            remaining["revision"], 1,
            "reading dependencies must not mutate work"
        );
        assert!(
            get.run(json!({"id":a.id(),"blocks_limit":0}), &cx)
                .await
                .is_err()
        );
        assert!(
            get.run(json!({"id":a.id(),"blocks_after":"bad\n"}), &cx)
                .await
                .is_err()
        );
    }
}
