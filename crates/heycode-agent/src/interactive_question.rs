//! Cancellable model-to-human questions shared by native and delegated turns.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const MAX_PENDING_QUESTIONS: usize = 64;
const MAX_PROMPT_BYTES: usize = 4 * 1024;
const MAX_HEADER_BYTES: usize = 64;
const MAX_CHOICE_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 1024;
const MAX_ANSWER_BYTES: usize = 16 * 1024;

/// One labelled answer proposed by the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionChoice {
    /// Exact value returned when selected.
    pub label: String,
    /// Optional explanation shown below the label.
    #[serde(default)]
    pub description: Option<String>,
}

pub use heycode_core::QuestionMode;

/// Validated question shared by blocking and durable optional tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionSpec {
    /// Stable model-supplied id, generated within a batch when omitted.
    #[serde(default)]
    pub id: String,
    /// Human-visible prompt, unique within a batch.
    pub question: String,
    /// Short category label.
    #[serde(default)]
    pub header: Option<String>,
    /// Required answer shape.
    pub mode: QuestionMode,
    /// Suggested answers with optional explanations.
    #[serde(default)]
    pub options: Vec<QuestionChoice>,
}

pub(crate) fn parse_questions(
    value: serde_json::Value,
) -> Result<(Vec<QuestionSpec>, bool), heycode_tools::ToolError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Batch {
        questions: Vec<QuestionSpec>,
    }
    let legacy = value.get("questions").is_none();
    let mut questions = if legacy {
        let args: AskUserQuestionArgs = serde_json::from_value(value)
            .map_err(|_| heycode_tools::ToolError::new("question arguments are invalid"))?;
        vec![QuestionSpec {
            id: String::new(),
            question: args.question,
            header: args.header,
            mode: if args.options.is_empty() {
                QuestionMode::FreeText
            } else {
                QuestionMode::SingleChoice
            },
            options: args
                .options
                .into_iter()
                .map(|o| QuestionChoice {
                    label: o.label,
                    description: o.description,
                })
                .collect(),
        }]
    } else {
        serde_json::from_value::<Batch>(value)
            .map_err(|_| heycode_tools::ToolError::new("question arguments are invalid"))?
            .questions
    };
    if !(1..=4).contains(&questions.len()) {
        return Err(heycode_tools::ToolError::new(
            "supply one to four questions",
        ));
    }
    let mut prompts = std::collections::HashSet::new();
    for (index, question) in questions.iter_mut().enumerate() {
        if question.id.is_empty() {
            question.id = format!("q{}", index + 1);
        }
        if !valid_line(&question.id, 64) {
            return Err(heycode_tools::ToolError::new("question id is invalid"));
        }
        validate_question_spec(question)?;
        if !prompts.insert(question.id.clone()) {
            return Err(heycode_tools::ToolError::new(
                "question ids must be unique within a batch",
            ));
        }
    }
    Ok((questions, legacy))
}

pub(crate) fn question_parameters() -> serde_json::Value {
    serde_json::json!({"type":"object","additionalProperties":false,
        "properties":{"questions":{"type":"array","minItems":1,"maxItems":4,"items":{
            "type":"object","additionalProperties":false,
            "properties":{
                "id":{"type":"string","minLength":1,"maxLength":64},
                "question":{"type":"string","minLength":1,"maxLength":4096},
                "header":{"type":"string","minLength":1,"maxLength":64},
                "mode":{"type":"string","enum":["single_choice","multiple_choice","free_text"]},
                "options":{"type":"array","maxItems":4,"items":{"type":"object","additionalProperties":false,
                    "properties":{"label":{"type":"string","minLength":1,"maxLength":256},"description":{"type":"string","minLength":1,"maxLength":1024}},"required":["label"]}}
            },"required":["question","mode"]
        }}},"required":["questions"]})
}

pub(crate) fn validate_question_spec(
    question: &QuestionSpec,
) -> Result<(), heycode_tools::ToolError> {
    let count_valid = match question.mode {
        QuestionMode::FreeText => question.options.is_empty(),
        QuestionMode::SingleChoice | QuestionMode::MultipleChoice => {
            (2..=4).contains(&question.options.len())
        }
    };
    if !count_valid {
        return Err(heycode_tools::ToolError::new(
            "free_text requires no options; choice questions require two to four options",
        ));
    }
    validate_args(&AskUserQuestionArgs {
        question: question.question.clone(),
        header: question.header.clone(),
        options: question
            .options
            .iter()
            .map(|o| AskUserQuestionOption {
                label: o.label.clone(),
                description: o.description.clone(),
            })
            .collect(),
    })
}

/// One question delivered to an active front end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionNotification {
    /// Correlation id used only by this service.
    pub id: u64,
    /// Requested answer mode.
    pub mode: QuestionMode,
    /// One-based position and total in the current batch.
    pub progress: (usize, usize),
    /// Optional short category label.
    pub header: Option<String>,
    /// Human-facing question.
    pub prompt: String,
    /// Ordered suggested answers. The front end may also accept custom text.
    pub choices: Vec<QuestionChoice>,
}

/// How the human settled one question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionAnswer {
    /// A selected label or custom answer.
    Answer(String),
    /// Explicit selected labels; preserved as an array in tool results.
    Selected(Vec<String>),
    /// The human explicitly cancelled the dialog.
    Cancelled,
}

/// Failure to deliver or settle an interactive question.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveQuestionError {
    /// No active surface can receive the question.
    #[error("no interactive question surface is available")]
    Unavailable,
    /// The owning turn or surface cancelled the question.
    #[error("the question was cancelled")]
    Cancelled,
    /// Internal bounded state could not admit the question.
    #[error("the interactive question service is unavailable")]
    Internal,
}

/// Independent transport-owned notification stream.
pub struct QuestionSubscription {
    id: u64,
    receiver: tokio::sync::mpsc::UnboundedReceiver<QuestionNotification>,
    subscribers: Arc<Mutex<HashMap<u64, tokio::sync::mpsc::UnboundedSender<QuestionNotification>>>>,
    waiters: Arc<Mutex<HashMap<u64, PendingQuestion>>>,
}

impl QuestionSubscription {
    /// Opaque owner id used to bind responses to this transport.
    #[must_use]
    pub const fn owner_id(&self) -> u64 {
        self.id
    }

    /// Wait for the next question.
    pub async fn recv(&mut self) -> Option<QuestionNotification> {
        self.receiver.recv().await
    }

    /// Receive one already-buffered question.
    pub fn try_recv(
        &mut self,
    ) -> Result<QuestionNotification, tokio::sync::mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for QuestionSubscription {
    fn drop(&mut self) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.remove(&self.id);
        }
        let owned = self
            .waiters
            .lock()
            .map(|mut waiters| {
                let ids = waiters
                    .iter()
                    .filter_map(|(id, pending)| (pending.owner == self.id).then_some(*id))
                    .collect::<Vec<_>>();
                ids.into_iter()
                    .filter_map(|id| waiters.remove(&id).map(|pending| pending.sender))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for sender in owned {
            let _sent = sender.send(QuestionAnswer::Cancelled);
        }
    }
}

struct PendingQuestion {
    owner: u64,
    mode: QuestionMode,
    labels: Vec<String>,
    sender: oneshot::Sender<QuestionAnswer>,
}

/// Shared model-question broker. It never chooses an answer on the user's behalf.
#[derive(Clone, Default)]
pub struct InteractiveQuestion {
    gate: Arc<tokio::sync::Mutex<()>>,
    counter: Arc<AtomicU64>,
    waiters: Arc<Mutex<HashMap<u64, PendingQuestion>>>,
    subscriber_counter: Arc<AtomicU64>,
    subscribers: Arc<Mutex<HashMap<u64, tokio::sync::mpsc::UnboundedSender<QuestionNotification>>>>,
}

impl InteractiveQuestion {
    /// Create an empty broker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a transport-owned subscription.
    pub fn take_subscription(&self) -> Option<QuestionSubscription> {
        let id = self
            .subscriber_counter
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .ok()?;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut subscribers = self.subscribers.lock().ok()?;
        if !subscribers.is_empty() {
            return None;
        }
        subscribers.insert(id, sender);
        drop(subscribers);
        Some(QuestionSubscription {
            id,
            receiver,
            subscribers: self.subscribers.clone(),
            waiters: self.waiters.clone(),
        })
    }

    /// Whether an exact request still awaits the human.
    #[must_use]
    pub fn is_pending(&self, id: u64) -> bool {
        self.waiters
            .lock()
            .is_ok_and(|waiters| waiters.contains_key(&id))
    }

    /// Resolve one request owned by an exact transport subscription.
    pub fn answer_owned(&self, owner: u64, id: u64, answer: QuestionAnswer) -> bool {
        if let QuestionAnswer::Answer(answer) = &answer
            && !valid_block(answer, MAX_ANSWER_BYTES)
        {
            return false;
        }
        let sender = self.waiters.lock().ok().and_then(|mut waiters| {
            if waiters.get(&id).is_some_and(|pending| {
                pending.owner == owner
                    && match &answer {
                        QuestionAnswer::Selected(labels) => {
                            pending.mode == QuestionMode::MultipleChoice
                                && !labels.is_empty()
                                && labels.iter().all(|label| pending.labels.contains(label))
                                && labels
                                    .iter()
                                    .collect::<std::collections::HashSet<_>>()
                                    .len()
                                    == labels.len()
                        }
                        _ => true,
                    }
            }) {
                waiters.remove(&id).map(|pending| pending.sender)
            } else {
                None
            }
        });
        sender.is_some_and(|sender| sender.send(answer).is_ok())
    }

    #[cfg(test)]
    async fn ask(
        &self,
        header: Option<String>,
        prompt: String,
        choices: Vec<QuestionChoice>,
        cancellation: CancellationToken,
    ) -> Result<QuestionAnswer, InteractiveQuestionError> {
        let mode = if choices.is_empty() {
            QuestionMode::FreeText
        } else {
            QuestionMode::SingleChoice
        };
        self.ask_spec(
            QuestionSpec {
                id: "q1".into(),
                header,
                question: prompt,
                options: choices,
                mode,
            },
            (1, 1),
            cancellation,
        )
        .await
    }

    async fn ask_spec(
        &self,
        question: QuestionSpec,
        progress: (usize, usize),
        cancellation: CancellationToken,
    ) -> Result<QuestionAnswer, InteractiveQuestionError> {
        if cancellation.is_cancelled() {
            return Err(InteractiveQuestionError::Cancelled);
        }
        // Every surface has one foreground question card. Serialize here,
        // across native and delegated tool executors, so parallel host-tool
        // calls cannot overwrite a card and strand its first waiter.
        let _gate = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(InteractiveQuestionError::Cancelled);
            }
            gate = self.gate.lock() => gate,
        };
        let id = self
            .counter
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .map_err(|_| InteractiveQuestionError::Internal)?;
        let (owner, subscriber) = {
            let subscribers = self
                .subscribers
                .lock()
                .map_err(|_| InteractiveQuestionError::Internal)?;
            let mut active = subscribers.iter();
            let Some((&owner, subscriber)) = active.next() else {
                return Err(InteractiveQuestionError::Unavailable);
            };
            if active.next().is_some() {
                return Err(InteractiveQuestionError::Unavailable);
            }
            (owner, subscriber.clone())
        };
        let (sender, receiver) = oneshot::channel();
        {
            let mut waiters = self
                .waiters
                .lock()
                .map_err(|_| InteractiveQuestionError::Internal)?;
            if waiters.len() >= MAX_PENDING_QUESTIONS {
                return Err(InteractiveQuestionError::Unavailable);
            }
            waiters.insert(
                id,
                PendingQuestion {
                    owner,
                    mode: question.mode,
                    labels: question.options.iter().map(|o| o.label.clone()).collect(),
                    sender,
                },
            );
        }
        let notification = QuestionNotification {
            id,
            mode: question.mode,
            progress,
            header: question.header,
            prompt: question.question,
            choices: question.options,
        };
        let delivered = subscriber.send(notification).is_ok();
        if !delivered {
            if let Ok(mut waiters) = self.waiters.lock() {
                waiters.remove(&id);
            }
            return Err(InteractiveQuestionError::Unavailable);
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                if let Ok(mut waiters) = self.waiters.lock() {
                    waiters.remove(&id);
                }
                Err(InteractiveQuestionError::Cancelled)
            }
            answer = receiver => answer.map_err(|_| InteractiveQuestionError::Cancelled),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskUserQuestionArgs {
    question: String,
    #[serde(default)]
    header: Option<String>,
    #[serde(default)]
    options: Vec<AskUserQuestionOption>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskUserQuestionOption {
    label: String,
    #[serde(default)]
    description: Option<String>,
}

pub(crate) struct AskUserQuestionTool {
    questions: InteractiveQuestion,
}

impl AskUserQuestionTool {
    pub(crate) fn new(questions: InteractiveQuestion) -> Self {
        Self { questions }
    }
}

#[async_trait::async_trait]
impl heycode_tools::Tool for AskUserQuestionTool {
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: "ask_user_question".to_owned(),
            description: "Ask one to four necessary non-secret questions and wait for explicit answers. Use single_choice, multiple_choice, or free_text; choice questions need 2–4 labelled options with helpful descriptions. Custom text is always allowed. Never infer an answer from silence.".to_owned(),
            parameters: question_parameters(),
        }
    }

    fn effect(&self) -> heycode_tools::ToolEffect {
        // A question is a scheduling barrier: no later tool should run before
        // the model receives the answer it said was required.
        heycode_tools::ToolEffect::Mutates
    }

    async fn run(
        &self,
        args: serde_json::Value,
        cx: &heycode_tools::ToolCtx,
    ) -> Result<serde_json::Value, heycode_tools::ToolError> {
        let (questions, legacy) = parse_questions(args)?;
        let total = questions.len();
        let mut answers = Vec::new();
        for (index, question) in questions.into_iter().enumerate() {
            let prompt = question.question.clone();
            let question_id = question.id.clone();
            let answer = match self
                .questions
                .ask_spec(question, (index + 1, total), cx.cancellation.clone())
                .await
            {
                Ok(QuestionAnswer::Answer(answer)) => serde_json::json!(answer),
                Ok(QuestionAnswer::Selected(labels)) => serde_json::json!(labels),
                Ok(QuestionAnswer::Cancelled) => {
                    return Err(heycode_tools::ToolError::new(
                        "The user cancelled the question.",
                    ));
                }
                Err(error) => return Err(heycode_tools::ToolError::new(error.to_string())),
            };
            answers.push(serde_json::json!({"id":question_id,"question":prompt,"answer":answer}));
        }
        if legacy {
            Ok(serde_json::json!({"answer":answers.first().and_then(|entry|entry.get("answer"))}))
        } else {
            Ok(serde_json::json!({"answers":answers}))
        }
    }
}

fn validate_args(args: &AskUserQuestionArgs) -> Result<(), heycode_tools::ToolError> {
    if !valid_block(&args.question, MAX_PROMPT_BYTES)
        || args
            .header
            .as_deref()
            .is_some_and(|header| !valid_line(header, MAX_HEADER_BYTES))
        || (!args.options.is_empty() && !(2..=4).contains(&args.options.len()))
    {
        return Err(heycode_tools::ToolError::new(
            "question arguments are invalid",
        ));
    }
    let mut labels = std::collections::HashSet::with_capacity(args.options.len());
    for option in &args.options {
        if !valid_line(&option.label, MAX_CHOICE_BYTES)
            || option
                .description
                .as_deref()
                .is_some_and(|description| !valid_block(description, MAX_DESCRIPTION_BYTES))
            || !labels.insert(option.label.as_str())
        {
            return Err(heycode_tools::ToolError::new(
                "question options are invalid",
            ));
        }
    }
    Ok(())
}

fn valid_line(value: &str, maximum: usize) -> bool {
    valid_block(value, maximum)
        && !value
            .chars()
            .any(|character| matches!(character, '\n' | '\r'))
}

fn valid_block(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= maximum
        && !value.chars().any(|character| {
            character == '\0'
                || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use heycode_tools::Tool as _;

    fn choices() -> Vec<QuestionChoice> {
        vec![
            QuestionChoice {
                label: "Proceed".to_owned(),
                description: Some("Continue with the implementation".to_owned()),
            },
            QuestionChoice {
                label: "Stop".to_owned(),
                description: Some("Leave the workspace unchanged".to_owned()),
            },
        ]
    }

    #[tokio::test]
    async fn exact_subscription_owns_answer_and_resumes_waiter() {
        let questions = InteractiveQuestion::new();
        let mut subscription = questions.take_subscription().expect("subscription");
        assert!(
            questions.take_subscription().is_none(),
            "ownership is exclusive"
        );
        let owner = subscription.owner_id();
        let asking = {
            let questions = questions.clone();
            tokio::spawn(async move {
                questions
                    .ask(
                        Some("Intent".to_owned()),
                        "How should I continue?".to_owned(),
                        choices(),
                        CancellationToken::new(),
                    )
                    .await
            })
        };
        let notification = subscription.recv().await.expect("question");
        assert_eq!(notification.header.as_deref(), Some("Intent"));
        assert_eq!(notification.choices, choices());
        assert!(!questions.answer_owned(
            owner + 1,
            notification.id,
            QuestionAnswer::Answer("Proceed".to_owned())
        ));
        assert!(questions.is_pending(notification.id));
        assert!(questions.answer_owned(
            owner,
            notification.id,
            QuestionAnswer::Answer("Proceed".to_owned())
        ));
        assert_eq!(
            asking.await.unwrap().unwrap(),
            QuestionAnswer::Answer("Proceed".to_owned())
        );
    }

    #[tokio::test]
    async fn unavailable_surface_fails_immediately() {
        let questions = InteractiveQuestion::new();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            questions.ask(
                None,
                "Need input".to_owned(),
                Vec::new(),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("must not hang");
        assert_eq!(outcome, Err(InteractiveQuestionError::Unavailable));
    }

    #[test]
    fn question_tool_is_a_batch_barrier() {
        let tool = AskUserQuestionTool::new(InteractiveQuestion::new());
        assert_eq!(tool.spec().name, "ask_user_question");
        assert_eq!(tool.effect(), heycode_tools::ToolEffect::Mutates);
    }

    #[tokio::test]
    async fn dropping_owner_cancels_its_pending_question() {
        let questions = InteractiveQuestion::new();
        let mut subscription = questions.take_subscription().expect("subscription");
        let asking = {
            let questions = questions.clone();
            tokio::spawn(async move {
                questions
                    .ask(
                        None,
                        "Need input".to_owned(),
                        Vec::new(),
                        CancellationToken::new(),
                    )
                    .await
            })
        };
        let notification = subscription.recv().await.expect("question");
        assert!(questions.is_pending(notification.id));
        drop(subscription);
        assert_eq!(asking.await.unwrap().unwrap(), QuestionAnswer::Cancelled);
        assert!(!questions.is_pending(notification.id));
    }

    #[tokio::test]
    async fn parallel_calls_are_presented_and_settled_one_at_a_time() {
        let questions = InteractiveQuestion::new();
        let mut subscription = questions.take_subscription().expect("subscription");
        let owner = subscription.owner_id();
        let first_task = {
            let questions = questions.clone();
            tokio::spawn(async move {
                questions
                    .ask(
                        None,
                        "First or second A".to_owned(),
                        Vec::new(),
                        CancellationToken::new(),
                    )
                    .await
            })
        };
        let second_task = {
            let questions = questions.clone();
            tokio::spawn(async move {
                questions
                    .ask(
                        None,
                        "First or second B".to_owned(),
                        Vec::new(),
                        CancellationToken::new(),
                    )
                    .await
            })
        };
        let first = subscription.recv().await.expect("first question");
        assert!(matches!(
            subscription.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert!(questions.answer_owned(
            owner,
            first.id,
            QuestionAnswer::Answer("first answer".to_owned())
        ));
        let second = subscription.recv().await.expect("second question");
        assert_ne!(first.prompt, second.prompt);
        assert!(questions.answer_owned(owner, second.id, QuestionAnswer::Cancelled));
        let outcomes = [first_task.await.unwrap(), second_task.await.unwrap()];
        assert!(outcomes.contains(&Ok(QuestionAnswer::Answer("first answer".to_owned()))));
        assert!(outcomes.contains(&Ok(QuestionAnswer::Cancelled)));
    }
    #[test]
    fn structured_question_validation_retains_ids_and_rejects_ambiguous_modes() {
        let (questions,legacy) = parse_questions(serde_json::json!({"questions":[
            {"id":"scope","question":"Which scope?","mode":"multiple_choice","options":[{"label":"A","description":"First"},{"label":"B"}]},
            {"question":"Any detail?","mode":"free_text"}
        ]})).unwrap();
        assert!(!legacy);
        assert_eq!(questions[0].id, "scope");
        assert_eq!(questions[1].id, "q2");
        for invalid in [
            serde_json::json!({"questions":[]}),
            serde_json::json!({"questions":[{"question":"Pick","mode":"multiple_choice","options":[]}]}),
            serde_json::json!({"questions":[{"question":"Write","mode":"free_text","options":[{"label":"A"},{"label":"B"}]}]}),
            serde_json::json!({"questions":[{"id":"same","question":"A","mode":"free_text"},{"id":"same","question":"B","mode":"free_text"}]}),
        ] {
            assert!(parse_questions(invalid).is_err());
        }
    }

    #[tokio::test]
    async fn required_batch_preserves_typed_answers_progress_and_exact_owner() {
        let questions = InteractiveQuestion::new();
        let mut subscription = questions.take_subscription().unwrap();
        let owner = subscription.owner_id();
        let tool = AskUserQuestionTool::new(questions.clone());
        let task = tokio::spawn(async move {
            tool.run(serde_json::json!({"questions":[
            {"id":"scope","question":"Which scope?","mode":"multiple_choice","options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}]},
            {"id":"detail","question":"Any detail?","mode":"free_text"}
        ]}),&heycode_tools::ToolCtx::default()).await
        });
        let first = subscription.recv().await.unwrap();
        assert_eq!(first.mode, QuestionMode::MultipleChoice);
        assert_eq!(first.progress, (1, 2));
        assert_eq!(first.choices[0].description.as_deref(), Some("First"));
        assert!(!task.is_finished(), "silence cannot answer required input");
        assert!(!questions.answer_owned(
            owner + 1,
            first.id,
            QuestionAnswer::Selected(vec!["A".into()])
        ));
        assert!(!questions.answer_owned(owner, first.id, QuestionAnswer::Selected(vec![])));
        assert!(!questions.answer_owned(
            owner,
            first.id,
            QuestionAnswer::Selected(vec!["invented".into()])
        ));
        assert!(!questions.answer_owned(
            owner,
            first.id,
            QuestionAnswer::Selected(vec!["A".into(), "A".into()])
        ));
        assert!(questions.answer_owned(
            owner,
            first.id,
            QuestionAnswer::Selected(vec!["A".into(), "B".into()])
        ));
        let second = subscription.recv().await.unwrap();
        assert_eq!(second.progress, (2, 2));
        assert_eq!(second.mode, QuestionMode::FreeText);
        assert!(!questions.answer_owned(owner, second.id, QuestionAnswer::Answer(" ".into())));
        assert!(questions.answer_owned(
            owner,
            second.id,
            QuestionAnswer::Answer("Custom /answer text".into())
        ));
        let result = task.await.unwrap().unwrap();
        assert_eq!(
            result["answers"][0],
            serde_json::json!({"id":"scope","question":"Which scope?","answer":["A","B"]})
        );
        assert_eq!(result["answers"][1]["answer"], "Custom /answer text");
        assert!(!questions.answer_owned(
            owner,
            first.id,
            QuestionAnswer::Selected(vec!["A".into()])
        ));
    }

    #[tokio::test]
    async fn cancelling_a_required_batch_does_not_answer_or_present_later_questions() {
        let questions = InteractiveQuestion::new();
        let mut subscription = questions.take_subscription().unwrap();
        let owner = subscription.owner_id();
        let tool = AskUserQuestionTool::new(questions.clone());
        let task =
            tokio::spawn(async move {
                tool.run(serde_json::json!({"questions":[
            {"question":"First","mode":"free_text"},{"question":"Second","mode":"free_text"}
        ]}),&heycode_tools::ToolCtx::default()).await
            });
        let first = subscription.recv().await.unwrap();
        assert!(questions.answer_owned(owner, first.id, QuestionAnswer::Cancelled));
        assert!(task.await.unwrap().is_err());
        assert!(subscription.try_recv().is_err());
    }
}
