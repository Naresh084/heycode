//! Shared cross-protocol request replay oracle for Q04 fixtures.

use heycode_core::RequestId;
use heycode_llm::{InferenceAdapter, ResolvedCall};
use heycode_session::{Session, SessionEventKind};

/// Failure of the persisted-session replay oracle.
///
/// Diagnostics retain only stable stage/field information; prompt, schema,
/// provider state and credential values are never copied into this type.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReplayOracleError {
    /// The live call could not become a valid safe snapshot or did not match
    /// the reopened projection.
    #[error(transparent)]
    Desync(#[from] crate::RequestDesyncError),
    /// Header/context could not commit to the disposable fixture session.
    #[error("replay oracle durable commit failed")]
    DurableCommit,
    /// The committed bytes could not be flushed before the independent read.
    #[error("replay oracle durable flush failed")]
    DurableFlush,
    /// The JSONL session could not be reopened from disk.
    #[error("replay oracle could not reopen the session")]
    Reopen,
    /// The reopened session could not reconstruct requests.
    #[error("replay oracle request projection failed")]
    Projection,
    /// The exact committed request id was absent after reopen.
    #[error("replay oracle committed request is missing")]
    MissingRequest,
}

/// Commit one resolved call's C02 snapshots, reopen the JSONL from disk,
/// reconstruct the exact request and run C05 verification.
///
/// The caller seeds the session's prior model-visible events before invoking
/// this function. Success returns the same dispatch capability production C05
/// uses, proving the fixture's live call is reconstructable from persisted
/// session truth rather than from an in-memory copy.
///
/// # Errors
/// Durable append/flush/reopen/projection failure, a missing request id, or the
/// first field mismatch between the reopened projection and live call.
pub fn verify_persisted_replay<'a>(
    session: &mut Session,
    turn: u64,
    step: u32,
    request_id: RequestId,
    call: ResolvedCall,
    adapter: &'a dyn InferenceAdapter,
    attachments: Option<&heycode_attachments::AttachmentStore>,
) -> Result<crate::VerifiedResolvedCall<'a>, ReplayOracleError> {
    let (header, context) = crate::snapshots_from_resolved_call(&call)?;
    session
        .append(SessionEventKind::RequestHeader {
            turn,
            step,
            request_id: request_id.clone(),
            header: Box::new(header),
        })
        .map_err(|_| ReplayOracleError::DurableCommit)?;
    session
        .append(SessionEventKind::RequestContext {
            request_id: request_id.clone(),
            context,
        })
        .map_err(|_| ReplayOracleError::DurableCommit)?;
    session
        .flush()
        .map_err(|_| ReplayOracleError::DurableFlush)?;
    let directory = session.path().parent().ok_or(ReplayOracleError::Reopen)?;
    let reopened = Session::open(directory).map_err(|_| ReplayOracleError::Reopen)?;
    let projected = heycode_session::project_requests(reopened.events())
        .map_err(|_| ReplayOracleError::Projection)?
        .into_iter()
        .find(|request| request.request_id == request_id)
        .ok_or(ReplayOracleError::MissingRequest)?;
    crate::verify_resolved_call(&projected, call, adapter, attachments).map_err(Into::into)
}
