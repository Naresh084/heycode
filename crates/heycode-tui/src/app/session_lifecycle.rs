//! Session command continuations kept separate from the shared shell event loop.

use super::{AppState, Item, SessionRecompose, TuiRunOutcome};
use heycode_core::SessionId;
use heycode_session::{ForkBoundary, SessionQueryError, SessionTitle};

impl AppState {
    pub(super) fn search_sessions(&mut self, search: &str) {
        self.open_session_browser();
        if let Some(browser) = self.session_browser.as_mut() {
            match browser.set_exact_title(search) {
                Err(error) => self.session_error(error),
                Ok(()) => {
                    if let Some(error) = browser.error() {
                        self.session_error(error);
                    } else if browser.total_matches() == 0 {
                        self.session_browser = None;
                        self.items
                            .push(Item::Info(format!("Session {search} was not found.")));
                    } else if browser.total_matches() == 1 {
                        if let Some(row) = browser.rows().first() {
                            let id = row.summary().id().clone();
                            self.select_session(id);
                        }
                    } else {
                        browser.set_notice(format!(
                            "Multiple sessions are named {search}. Choose a session."
                        ));
                    }
                }
            }
        }
    }

    pub(super) fn branch_session(
        &mut self,
        source: Option<SessionId>,
        title: Option<SessionTitle>,
    ) {
        let mut command = source.as_ref().map_or_else(
            || "/branch".to_owned(),
            |id| format!("/branch --session {id}"),
        );
        if let Some(title) = title.as_ref() {
            command.push(' ');
            command.push_str(title.as_str());
        }
        let Some(parent_session_id) = source.or_else(|| self.current_session_id()) else {
            self.session_error(SessionQueryError::SessionNotFound);
            return;
        };
        let Some(query) = self.session_query.as_ref() else {
            self.items
                .push(Item::Error("session lifecycle is unavailable".into()));
            return;
        };
        let parent_title = query.resume(&parent_session_id).ok().and_then(|source| {
            source
                .events()
                .iter()
                .rev()
                .find_map(|event| match &event.kind {
                    heycode_session::SessionEventKind::SessionTitle { title } => {
                        Some(title.clone())
                    }
                    _ => None,
                })
        });
        match query.fork(&parent_session_id, ForkBoundary::Latest) {
            Ok(mut child) => {
                let session_id = child.id().clone();
                if let Some(title) = title
                    && let Err(error) = query.rename_open(&mut child, &title)
                {
                    drop(child);
                    self.items.push(Item::Error(format!(
                        "branch title could not be saved: {error}; branch {session_id} remains available in /resume"
                    )));
                    return;
                }
                let title = child
                    .events()
                    .iter()
                    .rev()
                    .find_map(|event| match &event.kind {
                        heycode_session::SessionEventKind::SessionTitle { title } => {
                            Some(title.clone())
                        }
                        _ => None,
                    });
                drop(child);
                self.run_outcome =
                    Some(TuiRunOutcome::RecomposeSession(SessionRecompose::Forked {
                        parent_session_id,
                        parent_title,
                        session_id,
                        title,
                        command,
                    }));
            }
            Err(error) => self.session_error(error),
        }
    }
}
