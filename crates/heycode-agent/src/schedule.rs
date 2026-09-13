//! Ordered-commit scheduling for one step's batch of model-issued tool calls.
//!
//! One assistant message can request several tools. Running them one at a time
//! is correct but slow; running them concurrently is fast but reorders
//! everything the model and the human see. This module does both: it overlaps
//! the calls it is allowed to overlap and moves every call through a single
//! commit cursor **in the model's order**, so the durable log and the UI bus
//! carry exactly the sequence a one-at-a-time batch would have produced —
//! whatever order the calls actually finish in.
//!
//! The rules, each of which a test in this module pins:
//!
//! - **Announce at the head, commit at the head.** A call is announced
//!   ([`BatchCalls::announce`]: `tool/call` + `UiEvent::ToolStarted`) when it
//!   reaches the front of the commit queue, and committed
//!   ([`BatchCalls::commit`]: `tool/result` + `UiEvent::ToolFinished`) once its
//!   outcome is known and every earlier call is committed. Announcement and
//!   commit therefore strictly alternate, exactly as they do sequentially, and
//!   at most one call is ever visibly open.
//! - **Admission is serialized in model order.** The approval decision for one
//!   call completes before the next call is admitted, so a human is asked about
//!   one call at a time and in the order the model asked for them. Execution of
//!   the calls already admitted continues while that decision is outstanding.
//! - **A call that is not known to be parallel-safe is a barrier.** It is not
//!   admitted until every earlier call is committed, and nothing after it is
//!   admitted until it is committed. [`is_parallel_safe`] is a deny-by-default
//!   allowlist of read-only tool names.
//! - **A tool failure is an outcome, not an abort.** It commits at its own
//!   index and the calls after it still run. The assistant message already
//!   declared these calls and every provider requires one result per declared
//!   call, so skipping a successor's result would make the next request
//!   unsendable.
//! - **Cancellation stops admission.** Calls already executing have their token
//!   cancelled and are still awaited — a tool owns its own unwinding — while
//!   calls that never started commit as [`CallOutcome::NotStarted`]. Every
//!   declared call still gets exactly one result.
//!
//! The batch is driven on the caller's own task: the call futures are held in
//! a [`FuturesUnordered`], never spawned. That is a deliberate trade. Spawning
//! would give thread-level parallelism but detach the tools from the turn that
//! owns them — a cancelled turn would leave tool work running behind it, and
//! `JoinSet`, the structured alternative, aborts its tasks on drop, which
//! contradicts this workspace's rule that a cancelled tool is cancelled through
//! its token and then AWAITED (GOTCHAS #455). So overlap here is concurrency
//! with a bounded lifetime: every model-callable tool in this workspace is IO
//! bound (`tokio` files, processes and HTTP), so their waiting genuinely
//! overlaps, but a tool that burned CPU inline would not be sped up and would
//! stall its neighbours. Such a tool must offload its own work.
//!
//! One deliberate consequence: `tool/call` records the batch in model order,
//! not in wall-clock dispatch order. A call that overlapped its predecessor is
//! announced when it reaches the head, which may be after it already ran. The
//! log is a record of the batch, not a trace of it.

use std::future::Future;
use std::pin::Pin;

use futures::stream::{FuturesUnordered, StreamExt};
use tokio_util::sync::CancellationToken;

/// Boxed future produced by one call of a batch.
type CallFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Ceiling on how many calls of one batch execute at the same time.
pub(crate) const MAX_PARALLEL_TOOL_CALLS: usize = 8;

/// Client tools this workspace knows to be read-only, and therefore safe to
/// overlap with any other call in the same batch.
///
/// This is an allowlist and the default is **serialize**: an unknown name — an
/// `mcp__<server>__<tool>`, a plugin-contributed tool, anything added later —
/// is treated as a barrier. The claim that two concurrent invocations cannot
/// observe or corrupt each other's effects belongs to the tool, which states
/// it through [`heycode_tools::Tool::effect`]; the scheduler only reads it. A
/// list of names here would have to be edited every time a tool is added and
/// could never speak for a tool this crate has not heard of.
/// Whether a call whose tool declared `effect` may overlap the rest of its
/// batch.
///
/// `None` is a tool the registry does not know — a name the model invented, or
/// one registered after this batch was built — and joins everything that
/// declares nothing on the barrier side. Deny by default: overlapping is a
/// claim, and only the tool can make it.
pub(crate) fn is_parallel_safe(effect: Option<heycode_tools::ToolEffect>) -> bool {
    matches!(
        effect,
        Some(heycode_tools::ToolEffect::ReadOnly | heycode_tools::ToolEffect::Orchestration)
    )
}

/// The cancellation sources one batch honours.
pub(crate) struct BatchCancellation<'a> {
    /// The turn's own token.
    pub(crate) turn: &'a CancellationToken,
    /// The caller's token, which is cancelled independently of the turn.
    pub(crate) caller: &'a CancellationToken,
}

impl BatchCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.turn.is_cancelled() || self.caller.is_cancelled()
    }

    async fn cancelled(&self) {
        tokio::select! {
            () = self.turn.cancelled() => {}
            () = self.caller.cancelled() => {}
        }
    }
}

/// What the ordered admission phase decided about one call.
pub(crate) enum Admission<T> {
    /// Execute the call.
    Approved,
    /// Refused before it ran; this outcome commits at the call's own index.
    Refused(T),
}

/// What the batch committed for one call.
pub(crate) enum CallOutcome<T> {
    /// The call ran to completion — including a tool that returned an error.
    Ran(T),
    /// The batch was cancelled before this call started executing.
    NotStarted,
}

/// The per-call work one batch drives.
///
/// Every method takes `&self`: a batch holds several call futures at once, so
/// mutable state belongs to the implementer (the agent commits through its
/// session mutex and its event bus).
pub(crate) trait BatchCalls: Sync {
    /// Per-call result carried from execution to commit.
    type Outcome: Send;

    /// How many calls this batch contains.
    fn len(&self) -> usize;

    /// Whether the call at `index` may overlap the other calls in the batch.
    fn parallel_safe(&self, index: usize) -> bool;

    /// Announce the call at `index`. Called exactly once per call, when it
    /// reaches the head of the commit queue and always before its commit.
    ///
    /// # Errors
    /// Whatever the implementer's durable append reports.
    fn announce(&self, index: usize) -> anyhow::Result<()>;

    /// Decide whether the call at `index` may execute. Serialized in model
    /// order: the batch never has two admissions outstanding.
    ///
    /// `cancellation` is the batch's withdrawal handle for THIS admission: a
    /// cancelled batch fires it so an interactive policy can take its dialog
    /// down and return instead of parking a stopped turn open forever. The
    /// verdict of a cancelled admission is discarded either way — the call
    /// commits as [`CallOutcome::NotStarted`].
    fn admit(
        &self,
        index: usize,
        cancellation: CancellationToken,
    ) -> CallFuture<'_, Admission<Self::Outcome>>;

    /// Execute the call at `index`. The returned future owns its own
    /// unwinding: a cancelled batch cancels `token` and still awaits it.
    fn run(&self, index: usize, token: CancellationToken) -> CallFuture<'_, Self::Outcome>;

    /// Commit the call at `index`. Called exactly once per call, in index
    /// order, after that call's [`BatchCalls::announce`].
    ///
    /// # Errors
    /// Whatever the implementer's durable append reports.
    fn commit(&self, index: usize, outcome: CallOutcome<Self::Outcome>) -> anyhow::Result<()>;
}

/// The single ordered cursor. Nothing reaches the log or the bus except
/// through this: `announced` and `committed` only ever advance by one, and
/// only ever at the head, which is what makes the committed sequence the
/// model's sequence regardless of completion order.
struct Cursor<T> {
    slots: Vec<Option<CallOutcome<T>>>,
    /// Calls whose admission has begun.
    started: usize,
    /// Calls announced so far; always `committed` or `committed + 1`.
    announced: usize,
    /// Calls committed so far.
    committed: usize,
}

impl<T> Cursor<T> {
    fn new(total: usize) -> Self {
        Self {
            slots: (0..total).map(|_| None).collect(),
            started: 0,
            announced: 0,
            committed: 0,
        }
    }

    /// Announce the call at the head once it has begun, so the surface shows
    /// the call the batch is currently waiting on and never more than one.
    fn announce_head(&mut self, calls: &impl BatchCalls<Outcome = T>) -> anyhow::Result<()> {
        if self.announced == self.committed && self.announced < self.started {
            calls.announce(self.announced)?;
            self.announced += 1;
        }
        Ok(())
    }

    /// Commit every call whose outcome is known and whose predecessors are all
    /// committed. A gap stops the drain: that is the ordering barrier.
    fn drain(&mut self, calls: &impl BatchCalls<Outcome = T>) -> anyhow::Result<()> {
        while self.committed < self.slots.len() {
            let index = self.committed;
            let Some(outcome) = self.slots[index].take() else {
                break;
            };
            if self.announced == index {
                calls.announce(index)?;
                self.announced = index + 1;
            }
            calls.commit(index, outcome)?;
            self.committed = index + 1;
            self.announce_head(calls)?;
        }
        Ok(())
    }
}

/// What one turn of the driver loop observed.
enum Progress<T> {
    Cancelled,
    Admitted(usize, Admission<T>),
    Completed(usize, T),
}

/// Await the admission in progress, parking forever when there is none so the
/// branch can sit in a `select!` without a ready-immediately busy loop.
async fn awaiting<T>(slot: &mut Option<CallFuture<'_, T>>) -> T {
    match slot.as_mut() {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

/// Run one batch: overlap what may overlap, commit everything in model order.
///
/// Returns once every call has been committed exactly once.
///
/// # Errors
/// The first error an [`BatchCalls::announce`] or [`BatchCalls::commit`]
/// returns. The batch stops admitting, cancels and awaits what is already
/// executing, and reports that error rather than committing anything further.
pub(crate) async fn run_batch<C: BatchCalls>(
    calls: &C,
    max_parallel: usize,
    cancellation: &BatchCancellation<'_>,
) -> anyhow::Result<()> {
    let total = calls.len();
    // A zero ceiling is a degenerate configuration, not a deadlock: one call at
    // a time is the honest reading of "no parallelism".
    let max_parallel = max_parallel.max(1);
    let mut cursor = Cursor::new(total);
    let mut executing: FuturesUnordered<CallFuture<'_, (usize, C::Outcome)>> =
        FuturesUnordered::new();
    let mut admitting: Option<CallFuture<'_, (usize, Admission<C::Outcome>)>> = None;
    // The withdrawal handle of the admission in flight. Held separately from
    // `tokens` (which owns execution tokens) because an admission that is
    // still parked has nothing executing to cancel.
    let mut admitting_token: Option<CancellationToken> = None;
    let mut tokens: Vec<(usize, CancellationToken)> = Vec::new();
    let mut barrier_executing = false;
    let mut cancelled = cancellation.is_cancelled();
    let mut failure: Option<anyhow::Error> = None;

    loop {
        if failure.is_none()
            && !cancelled
            && admitting.is_none()
            && cursor.started < total
            && !barrier_executing
            && executing.len() < max_parallel
            && (calls.parallel_safe(cursor.started) || cursor.committed == cursor.started)
        {
            let index = cursor.started;
            cursor.started = index + 1;
            if let Err(error) = cursor.announce_head(calls) {
                failure = Some(error);
                cancelled = true;
                withdraw(admitting_token.as_ref());
                cancel_all(&tokens);
                continue;
            }
            let admission_token = CancellationToken::new();
            let admission = calls.admit(index, admission_token.clone());
            admitting_token = Some(admission_token);
            admitting = Some(Box::pin(async move { (index, admission.await) }));
        }
        if admitting.is_none() && executing.is_empty() {
            break;
        }

        // Hoisted so the guards do not borrow what the branch futures own.
        let admission_pending = admitting.is_some();
        let anything_executing = !executing.is_empty();
        let progress = tokio::select! {
            biased;
            () = cancellation.cancelled(), if !cancelled => Progress::Cancelled,
            (index, admission) = awaiting(&mut admitting), if admission_pending => {
                Progress::Admitted(index, admission)
            }
            Some((index, outcome)) = executing.next(), if anything_executing => {
                Progress::Completed(index, outcome)
            }
        };

        match progress {
            Progress::Cancelled => {
                cancelled = true;
                withdraw(admitting_token.as_ref());
                cancel_all(&tokens);
            }
            Progress::Admitted(index, admission) => {
                admitting = None;
                admitting_token = None;
                match admission {
                    // An admission that resolves after cancellation decides
                    // nothing: the slot stays empty and is filled as
                    // `NotStarted` with the rest of the unstarted calls. That
                    // holds for a refusal too — "we stopped before this ran"
                    // is the honest result, and it is the same one the calls
                    // behind it get.
                    _ if cancelled => {}
                    Admission::Refused(outcome) => {
                        cursor.slots[index] = Some(CallOutcome::Ran(outcome));
                    }
                    Admission::Approved => {
                        let token = CancellationToken::new();
                        tokens.push((index, token.clone()));
                        barrier_executing = !calls.parallel_safe(index);
                        let call = calls.run(index, token);
                        executing.push(Box::pin(async move { (index, call.await) }));
                    }
                }
            }
            Progress::Completed(index, outcome) => {
                if !calls.parallel_safe(index) {
                    barrier_executing = false;
                }
                tokens.retain(|(pending, _)| *pending != index);
                cursor.slots[index] = Some(CallOutcome::Ran(outcome));
            }
        }

        if failure.is_none()
            && let Err(error) = cursor.drain(calls)
        {
            failure = Some(error);
            cancelled = true;
            withdraw(admitting_token.as_ref());
            cancel_all(&tokens);
        }
    }

    if let Some(error) = failure {
        return Err(error);
    }
    // Whatever is left uncommitted never started: a batch stops admitting when
    // it is cancelled, and every call the assistant declared still owes the
    // model exactly one result.
    for slot in cursor.slots.iter_mut().skip(cursor.committed) {
        if slot.is_none() {
            *slot = Some(CallOutcome::NotStarted);
        }
    }
    cursor.drain(calls)
}

/// Withdraw the admission in flight, if there is one.
fn withdraw(token: Option<&CancellationToken>) {
    if let Some(token) = token {
        token.cancel();
    }
}

fn cancel_all(tokens: &[(usize, CancellationToken)]) {
    for (_, token) in tokens {
        token.cancel();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::oneshot;

    use super::*;

    /// Bound on every wait in this module's tests: a scheduler bug that stalls
    /// must fail by name, not by running until the harness gives up.
    const TEST_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

    /// What the fake batch recorded, in the order it happened.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Note {
        Announced(usize),
        Admitted(usize),
        Launched(usize),
        Committed(usize, String),
    }

    /// How one call of a fake batch behaves.
    #[derive(Clone)]
    struct Spec {
        parallel_safe: bool,
        /// Refuse at admission instead of executing (an approval denial).
        refused: bool,
        /// The value the call produces, or the error text it fails with.
        result: Result<String, String>,
    }

    impl Spec {
        fn reader(index: usize) -> Self {
            Self {
                parallel_safe: true,
                refused: false,
                result: Ok(format!("read-{index}")),
            }
        }
    }

    /// A batch whose completion order the test decides.
    ///
    /// Each call parks on a oneshot; the test releases them in whatever order
    /// it wants and the committed transcript must not notice.
    struct Fake {
        specs: Vec<Spec>,
        gates: Mutex<Vec<Option<oneshot::Receiver<()>>>>,
        /// Optional park inside a call's admission, for the tests that need an
        /// approval to still be outstanding while other work continues.
        admit_gates: Mutex<Vec<Option<oneshot::Receiver<()>>>>,
        /// Indices inside admission right now, and the high-water mark.
        admitting_now: Mutex<BTreeSet<usize>>,
        admit_peak: AtomicUsize,
        notes: Mutex<Vec<Note>>,
        launches: AtomicUsize,
        /// Indices executing right now, and the high-water mark of that set.
        executing: Mutex<BTreeSet<usize>>,
        peak: AtomicUsize,
        /// Largest overlap observed while a non-parallel-safe call executed.
        barrier_overlap: AtomicUsize,
        /// Calls whose future ran to completion after its token was cancelled.
        unwound: Mutex<Vec<usize>>,
        /// Calls whose ADMISSION was withdrawn through its own token.
        withdrawn: Mutex<Vec<usize>>,
    }

    impl Fake {
        fn new(specs: Vec<Spec>) -> (Self, Vec<oneshot::Sender<()>>) {
            let mut gates = Vec::new();
            let mut admit_gates = Vec::new();
            let mut releases = Vec::new();
            for _ in &specs {
                let (tx, rx) = oneshot::channel();
                gates.push(Some(rx));
                admit_gates.push(None);
                releases.push(tx);
            }
            (
                Self {
                    specs,
                    gates: Mutex::new(gates),
                    admit_gates: Mutex::new(admit_gates),
                    admitting_now: Mutex::new(BTreeSet::new()),
                    admit_peak: AtomicUsize::new(0),
                    notes: Mutex::new(Vec::new()),
                    launches: AtomicUsize::new(0),
                    executing: Mutex::new(BTreeSet::new()),
                    peak: AtomicUsize::new(0),
                    barrier_overlap: AtomicUsize::new(0),
                    unwound: Mutex::new(Vec::new()),
                    withdrawn: Mutex::new(Vec::new()),
                },
                releases,
            )
        }

        /// Park the admission of `index` until the returned sender fires.
        fn park_admission(&self, index: usize) -> oneshot::Sender<()> {
            let (tx, rx) = oneshot::channel();
            self.admit_gates.lock().unwrap()[index] = Some(rx);
            tx
        }

        /// A batch whose calls all complete without being released.
        fn immediate(specs: Vec<Spec>) -> Self {
            let (fake, releases) = Self::new(specs);
            for release in releases {
                let _ = release.send(());
            }
            fake
        }

        fn notes(&self) -> Vec<Note> {
            self.notes.lock().unwrap().clone()
        }

        fn note(&self, note: Note) {
            self.notes.lock().unwrap().push(note);
        }

        fn committed(&self) -> Vec<(usize, String)> {
            self.notes()
                .into_iter()
                .filter_map(|note| match note {
                    Note::Committed(index, text) => Some((index, text)),
                    _ => None,
                })
                .collect()
        }
    }

    impl BatchCalls for Fake {
        type Outcome = Result<String, String>;

        fn len(&self) -> usize {
            self.specs.len()
        }

        fn parallel_safe(&self, index: usize) -> bool {
            self.specs[index].parallel_safe
        }

        fn announce(&self, index: usize) -> anyhow::Result<()> {
            self.note(Note::Announced(index));
            Ok(())
        }

        fn admit(
            &self,
            index: usize,
            cancellation: CancellationToken,
        ) -> CallFuture<'_, Admission<Self::Outcome>> {
            Box::pin(async move {
                let live = {
                    let mut admitting = self.admitting_now.lock().unwrap();
                    admitting.insert(index);
                    admitting.len()
                };
                self.admit_peak.fetch_max(live, Ordering::SeqCst);
                self.note(Note::Admitted(index));
                let gate = self.admit_gates.lock().unwrap()[index].take();
                let mut withdrawn = false;
                if let Some(gate) = gate {
                    // What an interactive policy does: park on the human, and
                    // take the dialog down when the batch withdraws the ask.
                    tokio::select! {
                        _ = gate => {}
                        () = cancellation.cancelled() => {
                            self.withdrawn.lock().unwrap().push(index);
                            withdrawn = true;
                        }
                    }
                }
                self.admitting_now.lock().unwrap().remove(&index);
                if withdrawn {
                    Admission::Refused(Err("approval withdrawn".to_owned()))
                } else if self.specs[index].refused {
                    Admission::Refused(Err("refused".to_owned()))
                } else {
                    Admission::Approved
                }
            })
        }

        fn run(&self, index: usize, token: CancellationToken) -> CallFuture<'_, Self::Outcome> {
            Box::pin(async move {
                self.launches.fetch_add(1, Ordering::SeqCst);
                self.note(Note::Launched(index));
                let live = {
                    let mut executing = self.executing.lock().unwrap();
                    executing.insert(index);
                    executing.len()
                };
                self.peak.fetch_max(live, Ordering::SeqCst);
                if !self.specs[index].parallel_safe {
                    self.barrier_overlap.fetch_max(live, Ordering::SeqCst);
                }
                let gate = self.gates.lock().unwrap()[index].take();
                if let Some(gate) = gate {
                    tokio::select! {
                        _ = gate => {}
                        () = token.cancelled() => {
                            self.unwound.lock().unwrap().push(index);
                        }
                    }
                }
                self.executing.lock().unwrap().remove(&index);
                self.specs[index].result.clone()
            })
        }

        fn commit(&self, index: usize, outcome: CallOutcome<Self::Outcome>) -> anyhow::Result<()> {
            let text = match outcome {
                CallOutcome::Ran(Ok(value)) => value,
                CallOutcome::Ran(Err(error)) => format!("error: {error}"),
                CallOutcome::NotStarted => "not started".to_owned(),
            };
            self.note(Note::Committed(index, text));
            Ok(())
        }
    }

    fn live() -> CancellationToken {
        CancellationToken::new()
    }

    /// What running the same specs strictly one at a time would commit. This
    /// is the reference the ordering property is stated against.
    fn sequential(specs: &[Spec]) -> Vec<(usize, String)> {
        specs
            .iter()
            .enumerate()
            .map(|(index, spec)| {
                let text = if spec.refused {
                    "error: refused".to_owned()
                } else {
                    match &spec.result {
                        Ok(value) => value.clone(),
                        Err(error) => format!("error: {error}"),
                    }
                };
                (index, text)
            })
            .collect()
    }

    /// Drive one batch, finishing its calls in `order`.
    ///
    /// `order` is a *priority* over the calls, not a script: at every moment
    /// the highest-priority call that is actually executing is the one released
    /// next. A denied call never executes and a barrier cannot execute early,
    /// so a fixed script would deadlock the test rather than the scheduler.
    /// What every permutation therefore explores is every completion order the
    /// batch can actually produce.
    async fn commit_under(specs: Vec<Spec>, order: &[usize]) -> Vec<Note> {
        let (fake, mut releases) = Fake::new(specs);
        let releases: Vec<Option<oneshot::Sender<()>>> =
            releases.drain(..).map(Some).collect::<Vec<_>>();
        let releases = Mutex::new(releases);
        let (turn, caller) = (live(), live());
        let cancellation = BatchCancellation {
            turn: &turn,
            caller: &caller,
        };
        let settled = std::sync::atomic::AtomicBool::new(false);
        let driver = async {
            let outcome = run_batch(&fake, MAX_PARALLEL_TOOL_CALLS, &cancellation).await;
            settled.store(true, Ordering::SeqCst);
            outcome
        };
        let releasing = async {
            while !settled.load(Ordering::SeqCst) {
                let executing = fake.executing.lock().unwrap().clone();
                let next = order
                    .iter()
                    .find(|index| executing.contains(index))
                    .copied();
                match next {
                    Some(index) => {
                        let release = releases.lock().unwrap()[index].take();
                        if let Some(release) = release {
                            let _ = release.send(());
                        }
                        // The call has to leave `executing` before the next
                        // pick, or the same index is chosen again forever.
                        while fake.executing.lock().unwrap().contains(&index)
                            && !settled.load(Ordering::SeqCst)
                        {
                            tokio::task::yield_now().await;
                        }
                    }
                    None => tokio::task::yield_now().await,
                }
            }
        };
        let (result, ()) =
            tokio::time::timeout(TEST_BUDGET, futures::future::join(driver, releasing))
                .await
                .expect("the batch must settle inside the test budget");
        result.unwrap();
        fake.notes()
    }

    /// Every permutation of `0..n`, smallest lexicographic first.
    fn permutations(n: usize) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        let mut current: Vec<usize> = (0..n).collect();
        loop {
            out.push(current.clone());
            // Next lexicographic permutation; the loop ends at the last one.
            let Some(pivot) = (0..current.len().saturating_sub(1))
                .rev()
                .find(|i| current[*i] < current[i + 1])
            else {
                break;
            };
            let successor = (pivot + 1..current.len())
                .rev()
                .find(|i| current[*i] > current[pivot])
                .unwrap_or(pivot + 1);
            current.swap(pivot, successor);
            current[pivot + 1..].reverse();
        }
        out
    }

    #[test]
    fn the_permutation_generator_produces_every_distinct_order() {
        for n in 1..=5 {
            let orders = permutations(n);
            let expected: usize = (1..=n).product();
            assert_eq!(orders.len(), expected, "n={n} must yield n! orders");
            let distinct: BTreeSet<Vec<usize>> = orders.into_iter().collect();
            assert_eq!(distinct.len(), expected, "n={n} orders must be distinct");
        }
    }

    /// THE row's acceptance, stated exhaustively: for batch sizes 1..=5, every
    /// completion order (n!) crossed with every parallel-safe/barrier mask
    /// (2^n) — 4282 batches — commits the same sequence as running the calls
    /// one at a time. Exhaustive over this space, so there is nothing to seed
    /// and nothing to shrink.
    #[tokio::test]
    async fn every_completion_order_commits_the_sequence_of_a_one_at_a_time_batch() {
        let mut batches = 0_usize;
        for n in 1..=5_usize {
            for mask in 0..(1_u32 << n) {
                let specs: Vec<Spec> = (0..n)
                    .map(|index| Spec {
                        parallel_safe: mask & (1 << index) != 0,
                        refused: false,
                        result: Ok(format!("value-{index}")),
                    })
                    .collect();
                let expected = sequential(&specs);
                for order in permutations(n) {
                    let notes = commit_under(specs.clone(), &order).await;
                    let committed: Vec<(usize, String)> = notes
                        .iter()
                        .filter_map(|note| match note {
                            Note::Committed(index, text) => Some((*index, text.clone())),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(
                        committed, expected,
                        "n={n} mask={mask:b} completion order {order:?}"
                    );
                    batches += 1;
                }
            }
        }
        assert_eq!(
            batches, 4282,
            "the exhaustive space must not shrink silently"
        );
    }

    /// The same property over a wider space than exhaustion can reach:
    /// randomized sizes, masks, denials and tool failures. The generator is a
    /// fixed-seed xorshift so a failure is replayable from the printed case,
    /// and the failure message carries the exact specs and order — the case is
    /// already minimal in the only dimension that matters, the completion
    /// order, because every order in the space is equally valid.
    #[tokio::test]
    async fn randomized_batches_with_failures_and_denials_commit_in_model_order() {
        const SEED: u64 = 0x05DE_C0DE_0A06;
        let mut rng = SEED;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for case in 0..400_u32 {
            let n = 1 + (next() % 6) as usize;
            let specs: Vec<Spec> = (0..n)
                .map(|index| {
                    let roll = next() % 10;
                    Spec {
                        parallel_safe: next() % 2 == 0,
                        refused: roll == 0,
                        result: if roll == 1 {
                            Err(format!("boom-{index}"))
                        } else {
                            Ok(format!("value-{index}"))
                        },
                    }
                })
                .collect();
            let mut order: Vec<usize> = (0..n).collect();
            for i in (1..n).rev() {
                let j = (next() % (i as u64 + 1)) as usize;
                order.swap(i, j);
            }
            let expected = sequential(&specs);
            let notes = commit_under(specs.clone(), &order).await;
            let committed: Vec<(usize, String)> = notes
                .iter()
                .filter_map(|note| match note {
                    Note::Committed(index, text) => Some((*index, text.clone())),
                    _ => None,
                })
                .collect();
            let shape: Vec<(bool, bool)> = specs
                .iter()
                .map(|spec| (spec.parallel_safe, spec.refused))
                .collect();
            assert_eq!(
                committed, expected,
                "seed {SEED:#x} case {case}: shape {shape:?}, completion order {order:?}"
            );
        }
    }

    /// The property above is only worth anything if its comparison can fail.
    /// Feed the same check a transcript committed in completion order and it
    /// must reject it.
    #[test]
    fn the_ordering_comparison_rejects_a_transcript_committed_in_completion_order() {
        let specs: Vec<Spec> = (0..3).map(Spec::reader).collect();
        let expected = sequential(&specs);
        let by_completion: Vec<(usize, String)> = [2, 0, 1]
            .into_iter()
            .map(|index| (index, format!("read-{index}")))
            .collect();
        assert_ne!(
            by_completion, expected,
            "a completion-ordered transcript must not compare equal to the model order"
        );
        assert_eq!(
            expected,
            vec![
                (0, "read-0".to_owned()),
                (1, "read-1".to_owned()),
                (2, "read-2".to_owned())
            ]
        );
    }

    #[tokio::test]
    async fn a_failing_call_neither_blocks_nor_reorders_the_calls_after_it() {
        let specs = vec![
            Spec::reader(0),
            Spec {
                parallel_safe: true,
                refused: false,
                result: Err("disk on fire".to_owned()),
            },
            Spec::reader(2),
        ];
        // The failure completes first; the calls around it complete last.
        let notes = commit_under(specs, &[1, 2, 0]).await;
        let committed: Vec<(usize, String)> = notes
            .iter()
            .filter_map(|note| match note {
                Note::Committed(index, text) => Some((*index, text.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            committed,
            vec![
                (0, "read-0".to_owned()),
                (1, "error: disk on fire".to_owned()),
                (2, "read-2".to_owned()),
            ],
            "a failure commits at its own index and its successors still run"
        );
    }

    #[tokio::test]
    async fn a_denied_call_neither_launches_nor_stops_the_calls_after_it() {
        let specs = vec![
            Spec::reader(0),
            Spec {
                parallel_safe: true,
                refused: true,
                result: Ok("must not run".to_owned()),
            },
            Spec::reader(2),
        ];
        let fake = Fake::immediate(specs);
        let (turn, caller) = (live(), live());
        tokio::time::timeout(
            TEST_BUDGET,
            run_batch(
                &fake,
                MAX_PARALLEL_TOOL_CALLS,
                &BatchCancellation {
                    turn: &turn,
                    caller: &caller,
                },
            ),
        )
        .await
        .expect("the batch must settle inside the test budget")
        .unwrap();
        assert_eq!(
            fake.committed(),
            vec![
                (0, "read-0".to_owned()),
                (1, "error: refused".to_owned()),
                (2, "read-2".to_owned()),
            ]
        );
        assert!(
            !fake.notes().contains(&Note::Launched(1)),
            "a refused call must never be executed: {:?}",
            fake.notes()
        );
        assert_eq!(fake.launches.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn parallel_safe_calls_actually_execute_at_the_same_time() {
        let specs: Vec<Spec> = (0..3).map(Spec::reader).collect();
        let (fake, releases) = Fake::new(specs);
        let (turn, caller) = (live(), live());
        let cancellation = BatchCancellation {
            turn: &turn,
            caller: &caller,
        };
        let driver = run_batch(&fake, MAX_PARALLEL_TOOL_CALLS, &cancellation);
        // Release nothing until all three are executing: a sequential
        // scheduler cannot reach this point and the test fails by timeout.
        let releasing = async {
            loop {
                if fake.executing.lock().unwrap().len() == 3 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            for release in releases {
                let _ = release.send(());
            }
        };
        let (result, ()) =
            tokio::time::timeout(TEST_BUDGET, futures::future::join(driver, releasing))
                .await
                .expect("three read-only calls must be able to overlap");
        result.unwrap();
        assert_eq!(fake.peak.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_barrier_call_never_overlaps_another_call_in_its_batch() {
        // Two readers, a barrier, two more readers.
        let specs = vec![
            Spec::reader(0),
            Spec::reader(1),
            Spec {
                parallel_safe: false,
                refused: false,
                result: Ok("wrote".to_owned()),
            },
            Spec::reader(3),
            Spec::reader(4),
        ];
        let notes = commit_under(specs, &[1, 0, 2, 4, 3]).await;
        let committed: Vec<usize> = notes
            .iter()
            .filter_map(|note| match note {
                Note::Committed(index, _) => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(committed, vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn a_barrier_call_waits_for_its_predecessors_and_holds_back_its_successors() {
        let specs = vec![
            Spec::reader(0),
            Spec {
                parallel_safe: false,
                refused: false,
                result: Ok("wrote".to_owned()),
            },
            Spec::reader(2),
        ];
        let (fake, mut releases) = Fake::new(specs);
        let (turn, caller) = (live(), live());
        let cancellation = BatchCancellation {
            turn: &turn,
            caller: &caller,
        };
        let releases: Vec<Option<oneshot::Sender<()>>> = releases.drain(..).map(Some).collect();
        let releases = Mutex::new(releases);
        let driver = run_batch(&fake, MAX_PARALLEL_TOOL_CALLS, &cancellation);
        let checking = async {
            // While call 0 is executing, the barrier must not have launched.
            loop {
                if fake.executing.lock().unwrap().contains(&0) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(!fake.notes().contains(&Note::Launched(1)));
            assert!(!fake.notes().contains(&Note::Launched(2)));
            if let Some(release) = releases.lock().unwrap()[0].take() {
                let _ = release.send(());
            }
            // While the barrier is executing, call 2 must not have launched.
            loop {
                if fake.executing.lock().unwrap().contains(&1) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(!fake.notes().contains(&Note::Launched(2)));
            for index in [1, 2] {
                if let Some(release) = releases.lock().unwrap()[index].take() {
                    let _ = release.send(());
                }
            }
        };
        let (result, ()) =
            tokio::time::timeout(TEST_BUDGET, futures::future::join(driver, checking))
                .await
                .expect("the batch must settle inside the test budget");
        result.unwrap();
        assert_eq!(fake.barrier_overlap.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn the_parallel_ceiling_bounds_how_many_calls_execute_at_once() {
        let specs: Vec<Spec> = (0..6).map(Spec::reader).collect();
        let (fake, mut releases) = Fake::new(specs);
        let (turn, caller) = (live(), live());
        let cancellation = BatchCancellation {
            turn: &turn,
            caller: &caller,
        };
        let releases: Vec<Option<oneshot::Sender<()>>> = releases.drain(..).map(Some).collect();
        let releases = Mutex::new(releases);
        let driver = run_batch(&fake, 2, &cancellation);
        let releasing = async {
            for index in 0..6 {
                loop {
                    if fake.notes().contains(&Note::Launched(index)) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                if let Some(release) = releases.lock().unwrap()[index].take() {
                    let _ = release.send(());
                }
            }
        };
        let (result, ()) =
            tokio::time::timeout(TEST_BUDGET, futures::future::join(driver, releasing))
                .await
                .expect("the batch must settle inside the test budget");
        result.unwrap();
        assert_eq!(
            fake.peak.load(Ordering::SeqCst),
            2,
            "a ceiling of two must never be exceeded"
        );
        assert_eq!(fake.launches.load(Ordering::SeqCst), 6);
    }

    #[tokio::test]
    async fn a_zero_ceiling_runs_one_call_at_a_time_rather_than_stalling() {
        let specs: Vec<Spec> = (0..3).map(Spec::reader).collect();
        let fake = Fake::immediate(specs);
        let (turn, caller) = (live(), live());
        tokio::time::timeout(
            TEST_BUDGET,
            run_batch(
                &fake,
                0,
                &BatchCancellation {
                    turn: &turn,
                    caller: &caller,
                },
            ),
        )
        .await
        .expect("a zero ceiling must not stall the batch")
        .unwrap();
        assert_eq!(fake.peak.load(Ordering::SeqCst), 1);
        assert_eq!(fake.committed().len(), 3);
    }

    #[tokio::test]
    async fn admissions_never_overlap_and_happen_in_model_order() {
        let specs: Vec<Spec> = (0..4).map(Spec::reader).collect();
        let fake = Fake::immediate(specs);
        let (turn, caller) = (live(), live());
        tokio::time::timeout(
            TEST_BUDGET,
            run_batch(
                &fake,
                MAX_PARALLEL_TOOL_CALLS,
                &BatchCancellation {
                    turn: &turn,
                    caller: &caller,
                },
            ),
        )
        .await
        .expect("the batch must settle inside the test budget")
        .unwrap();
        let admitted: Vec<usize> = fake
            .notes()
            .into_iter()
            .filter_map(|note| match note {
                Note::Admitted(index) => Some(index),
                _ => None,
            })
            .collect();
        assert_eq!(admitted, vec![0, 1, 2, 3]);
        assert_eq!(fake.admit_peak.load(Ordering::SeqCst), 1);
    }

    /// Admissions that resolve instantly cannot tell a serialized chain from a
    /// concurrent one. Hold one open and prove no other call is even asked
    /// about — this is what stops a batch putting three approval dialogs in
    /// front of a human at once.
    #[tokio::test]
    async fn an_outstanding_approval_stops_any_other_call_being_admitted() {
        let specs: Vec<Spec> = (0..3).map(Spec::reader).collect();
        let fake = Fake::immediate(specs);
        let release_admission = fake.park_admission(1);
        let (turn, caller) = (live(), live());
        let cancellation = BatchCancellation {
            turn: &turn,
            caller: &caller,
        };
        let driver = run_batch(&fake, MAX_PARALLEL_TOOL_CALLS, &cancellation);
        let checking = async {
            loop {
                if fake.admitting_now.lock().unwrap().contains(&1) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            // Give the batch every chance to admit call 2 behind call 1's back.
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
            assert!(
                !fake.notes().contains(&Note::Admitted(2)),
                "no call may be admitted while an earlier approval is outstanding: {:?}",
                fake.notes()
            );
            let _ = release_admission.send(());
        };
        let (result, ()) =
            tokio::time::timeout(TEST_BUDGET, futures::future::join(driver, checking))
                .await
                .expect("the batch must settle inside the test budget");
        result.unwrap();
        assert_eq!(fake.admit_peak.load(Ordering::SeqCst), 1);
        assert_eq!(fake.committed().len(), 3);
    }

    /// An approval parks on a human. The calls already executing must keep
    /// making progress while it does — a batch that simply awaited the
    /// admission would stall every tool it had already started.
    #[tokio::test]
    async fn calls_already_executing_settle_while_an_approval_is_outstanding() {
        let specs: Vec<Spec> = (0..2).map(Spec::reader).collect();
        let (fake, mut releases) = Fake::new(specs);
        let release_admission = fake.park_admission(1);
        let (turn, caller) = (live(), live());
        let cancellation = BatchCancellation {
            turn: &turn,
            caller: &caller,
        };
        let release_zero = releases.remove(0);
        let release_one = releases.remove(0);
        let driver = run_batch(&fake, MAX_PARALLEL_TOOL_CALLS, &cancellation);
        let checking = async {
            loop {
                if fake.admitting_now.lock().unwrap().contains(&1) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let _ = release_zero.send(());
            // Call 0 must commit even though call 1's approval is still open.
            loop {
                if fake
                    .notes()
                    .contains(&Note::Committed(0, "read-0".to_owned()))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                fake.admitting_now.lock().unwrap().contains(&1),
                "the approval must still be outstanding when call 0 commits"
            );
            let _ = release_admission.send(());
            let _ = release_one.send(());
        };
        let (result, ()) =
            tokio::time::timeout(TEST_BUDGET, futures::future::join(driver, checking))
                .await
                .expect("an outstanding approval must not stall executing calls");
        result.unwrap();
        assert_eq!(fake.committed().len(), 2);
    }

    #[tokio::test]
    async fn each_call_is_announced_immediately_before_its_own_commit() {
        let specs: Vec<Spec> = (0..4).map(Spec::reader).collect();
        let notes = commit_under(specs, &[3, 1, 2, 0]).await;
        let surface: Vec<(&'static str, usize)> = notes
            .iter()
            .filter_map(|note| match note {
                Note::Announced(index) => Some(("announce", *index)),
                Note::Committed(index, _) => Some(("commit", *index)),
                _ => None,
            })
            .collect();
        assert_eq!(
            surface,
            vec![
                ("announce", 0),
                ("commit", 0),
                ("announce", 1),
                ("commit", 1),
                ("announce", 2),
                ("commit", 2),
                ("announce", 3),
                ("commit", 3),
            ],
            "at most one call may be visibly open, exactly as in a serial batch"
        );
    }

    #[tokio::test]
    async fn cancelling_a_batch_starts_nothing_further_and_commits_the_rest_as_not_started() {
        let specs: Vec<Spec> = (0..5).map(Spec::reader).collect();
        let (fake, releases) = Fake::new(specs);
        let (turn, caller) = (live(), live());
        let cancellation = BatchCancellation {
            turn: &turn,
            caller: &caller,
        };
        let driver = run_batch(&fake, 2, &cancellation);
        let cancelling = async {
            loop {
                if fake.executing.lock().unwrap().len() == 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            turn.cancel();
        };
        let (result, ()) =
            tokio::time::timeout(TEST_BUDGET, futures::future::join(driver, cancelling))
                .await
                .expect("a cancelled batch must settle inside the test budget");
        // The release senders must outlive the driver, not just the cancelling
        // block. `Fake::run` selects between its gate and its token; dropping a
        // sender resolves the gate too, so a `let _keep = releases` scoped to
        // the cancelling block made both arms ready at once and `select!`
        // picked between them at random — cancellation was observed for one
        // call and the gate for the other. Holding them to here leaves
        // cancellation as the only way out of the select, which is what
        // "nothing releases them" was always meant to mean.
        drop(releases);
        result.unwrap();
        assert_eq!(
            fake.launches.load(Ordering::SeqCst),
            2,
            "only the calls already executing may have been launched: {:?}",
            fake.notes()
        );
        assert_eq!(
            fake.committed(),
            vec![
                (0, "read-0".to_owned()),
                (1, "read-1".to_owned()),
                (2, "not started".to_owned()),
                (3, "not started".to_owned()),
                (4, "not started".to_owned()),
            ]
        );
        let mut unwound = fake.unwound.lock().unwrap().clone();
        unwound.sort_unstable();
        assert_eq!(
            unwound,
            vec![0, 1],
            "an executing call is cancelled through its own token and still awaited"
        );
        let admitted: Vec<usize> = fake
            .notes()
            .into_iter()
            .filter_map(|note| match note {
                Note::Admitted(index) => Some(index),
                _ => None,
            })
            .collect();
        assert_eq!(
            admitted,
            vec![0, 1],
            "a cancelled batch must ask for no further approval: an `ask` world \
             would put a dialog in front of a human for a turn they just stopped"
        );
    }

    /// Cancellation must settle a batch whose admission is PARKED, with no
    /// help from the thing it is parked on. An interactive approval parks
    /// inside `admit` until a human answers; if the only way out is that
    /// answer, a cancelled turn holds its lease, its inbox and `AgentIdle`
    /// hostage to a dialog nobody is going to touch.
    #[tokio::test]
    async fn a_parked_admission_is_withdrawn_by_cancellation_alone() {
        let specs: Vec<Spec> = (0..3).map(Spec::reader).collect();
        let (fake, releases) = Fake::new(specs);
        // Never fired: the human never answers.
        let never_answered = fake.park_admission(1);
        let (turn, caller) = (live(), live());
        let cancellation = BatchCancellation {
            turn: &turn,
            caller: &caller,
        };
        let driver = run_batch(&fake, MAX_PARALLEL_TOOL_CALLS, &cancellation);
        let cancelling = async {
            loop {
                if fake.admitting_now.lock().unwrap().contains(&1) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            turn.cancel();
        };
        let (result, ()) =
            tokio::time::timeout(TEST_BUDGET, futures::future::join(driver, cancelling))
                .await
                .expect("a parked admission must settle from cancellation alone");
        result.unwrap();
        // Held to here on purpose: a dropped sender resolves its gate, and the
        // batch would then have settled for a reason that is not cancellation.
        drop((releases, never_answered));
        assert_eq!(
            *fake.withdrawn.lock().unwrap(),
            vec![1],
            "the batch must withdraw the outstanding ask, not merely ignore it"
        );
        assert_eq!(
            fake.launches.load(Ordering::SeqCst),
            1,
            "a withdrawn admission starts nothing: {:?}",
            fake.notes()
        );
        assert_eq!(
            fake.committed(),
            vec![
                (0, "read-0".to_owned()),
                (1, "not started".to_owned()),
                (2, "not started".to_owned()),
            ],
            "every declared call still owes the model exactly one result"
        );
    }

    /// The dangerous window is an approval that resolves AFTER the turn was
    /// cancelled. Nothing may be executed on the strength of it.
    #[tokio::test]
    async fn an_approval_that_arrives_after_cancellation_starts_nothing() {
        let specs: Vec<Spec> = (0..3).map(Spec::reader).collect();
        let (fake, releases) = Fake::new(specs);
        let release_admission = fake.park_admission(1);
        let (turn, caller) = (live(), live());
        let cancellation = BatchCancellation {
            turn: &turn,
            caller: &caller,
        };
        let driver = run_batch(&fake, MAX_PARALLEL_TOOL_CALLS, &cancellation);
        let cancelling = async {
            loop {
                if fake.admitting_now.lock().unwrap().contains(&1) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            turn.cancel();
            // The approval says yes, but only after the turn was cancelled.
            let _ = release_admission.send(());
            let _keep = releases;
        };
        let (result, ()) =
            tokio::time::timeout(TEST_BUDGET, futures::future::join(driver, cancelling))
                .await
                .expect("a cancelled batch must settle inside the test budget");
        result.unwrap();
        assert_eq!(
            fake.launches.load(Ordering::SeqCst),
            1,
            "only the call already executing may have been launched: {:?}",
            fake.notes()
        );
        assert_eq!(
            fake.committed(),
            vec![
                (0, "read-0".to_owned()),
                (1, "not started".to_owned()),
                (2, "not started".to_owned()),
            ]
        );
    }

    /// The launch counter above is only evidence if it can report a larger
    /// number: an uncancelled batch of the same shape launches all five.
    #[tokio::test]
    async fn the_launch_counter_counts_every_call_when_nothing_is_cancelled() {
        let specs: Vec<Spec> = (0..5).map(Spec::reader).collect();
        let fake = Fake::immediate(specs);
        let (turn, caller) = (live(), live());
        tokio::time::timeout(
            TEST_BUDGET,
            run_batch(
                &fake,
                2,
                &BatchCancellation {
                    turn: &turn,
                    caller: &caller,
                },
            ),
        )
        .await
        .expect("the batch must settle inside the test budget")
        .unwrap();
        assert_eq!(fake.launches.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn a_batch_cancelled_before_it_starts_launches_nothing_and_still_commits_every_call() {
        let specs: Vec<Spec> = (0..3).map(Spec::reader).collect();
        let fake = Fake::immediate(specs);
        let (turn, caller) = (live(), live());
        caller.cancel();
        tokio::time::timeout(
            TEST_BUDGET,
            run_batch(
                &fake,
                MAX_PARALLEL_TOOL_CALLS,
                &BatchCancellation {
                    turn: &turn,
                    caller: &caller,
                },
            ),
        )
        .await
        .expect("an already-cancelled batch must settle immediately")
        .unwrap();
        assert_eq!(fake.launches.load(Ordering::SeqCst), 0);
        assert_eq!(
            fake.committed(),
            vec![
                (0, "not started".to_owned()),
                (1, "not started".to_owned()),
                (2, "not started".to_owned()),
            ],
            "every declared call still gets exactly one result"
        );
    }

    #[tokio::test]
    async fn the_callers_token_cancels_a_batch_the_turn_has_not_cancelled() {
        let specs: Vec<Spec> = (0..3).map(Spec::reader).collect();
        let (fake, releases) = Fake::new(specs);
        let (turn, caller) = (live(), live());
        let cancellation = BatchCancellation {
            turn: &turn,
            caller: &caller,
        };
        let driver = run_batch(&fake, 1, &cancellation);
        let cancelling = async {
            loop {
                if fake.executing.lock().unwrap().contains(&0) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            caller.cancel();
            let _keep = releases;
        };
        let (result, ()) =
            tokio::time::timeout(TEST_BUDGET, futures::future::join(driver, cancelling))
                .await
                .expect("the caller's token must cancel the batch");
        result.unwrap();
        assert_eq!(fake.launches.load(Ordering::SeqCst), 1);
        assert!(!turn.is_cancelled());
    }

    #[tokio::test]
    async fn every_call_is_committed_exactly_once() {
        let specs: Vec<Spec> = (0..5)
            .map(|index| Spec {
                parallel_safe: index % 2 == 0,
                refused: index == 3,
                result: Ok(format!("value-{index}")),
            })
            .collect();
        let notes = commit_under(specs, &[2, 0, 1, 4]).await;
        let committed: Vec<usize> = notes
            .iter()
            .filter_map(|note| match note {
                Note::Committed(index, _) => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(committed, vec![0, 1, 2, 3, 4]);
        let announced: Vec<usize> = notes
            .iter()
            .filter_map(|note| match note {
                Note::Announced(index) => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(announced, vec![0, 1, 2, 3, 4]);
    }

    /// A durable append can fail. When it does, the batch stops admitting,
    /// unwinds what is executing and reports the failure instead of writing a
    /// half-ordered tail.
    #[tokio::test]
    async fn a_commit_failure_stops_the_batch_and_is_reported() {
        struct Failing {
            committed: Mutex<Vec<usize>>,
            launches: AtomicUsize,
        }

        impl BatchCalls for Failing {
            type Outcome = ();

            fn len(&self) -> usize {
                4
            }

            fn parallel_safe(&self, _index: usize) -> bool {
                true
            }

            fn announce(&self, _index: usize) -> anyhow::Result<()> {
                Ok(())
            }

            fn admit(
                &self,
                _index: usize,
                _cancellation: CancellationToken,
            ) -> CallFuture<'_, Admission<Self::Outcome>> {
                Box::pin(async { Admission::Approved })
            }

            fn run(
                &self,
                _index: usize,
                _token: CancellationToken,
            ) -> CallFuture<'_, Self::Outcome> {
                self.launches.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {})
            }

            fn commit(
                &self,
                index: usize,
                _outcome: CallOutcome<Self::Outcome>,
            ) -> anyhow::Result<()> {
                self.committed.lock().unwrap().push(index);
                if index == 1 {
                    anyhow::bail!("session append failed");
                }
                Ok(())
            }
        }

        let calls = Failing {
            committed: Mutex::new(Vec::new()),
            launches: AtomicUsize::new(0),
        };
        let (turn, caller) = (live(), live());
        let error = tokio::time::timeout(
            TEST_BUDGET,
            run_batch(
                &calls,
                MAX_PARALLEL_TOOL_CALLS,
                &BatchCancellation {
                    turn: &turn,
                    caller: &caller,
                },
            ),
        )
        .await
        .expect("the batch must settle inside the test budget")
        .unwrap_err();
        assert!(error.to_string().contains("session append failed"));
        assert_eq!(
            *calls.committed.lock().unwrap(),
            vec![0, 1],
            "nothing commits after the failure"
        );
    }

    /// Explicit read-only tools and guarded orchestration may overlap.
    #[test]
    fn only_declared_safe_effects_admit_overlap() {
        for effect in [
            heycode_tools::ToolEffect::ReadOnly,
            heycode_tools::ToolEffect::Orchestration,
        ] {
            assert!(is_parallel_safe(Some(effect)));
        }
        assert!(
            !is_parallel_safe(Some(heycode_tools::ToolEffect::Mutates)),
            "a mutating tool is a barrier"
        );
        assert!(
            !is_parallel_safe(None),
            "a tool the registry does not know serializes: the default is deny"
        );
    }
}
