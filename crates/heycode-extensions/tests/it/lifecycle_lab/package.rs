//! Package-lifecycle properties: PL06's six operations under generated orders.
//!
//! The register/dispose/reload properties in this lab are about one process's
//! live context. This one is about the durable state that decides which package
//! version a composition will register in the first place, and it is the same
//! shape of claim: a refused transition must leave the last good state exactly
//! as it was, and one step of history must behave like one step of history no
//! matter what order the operator asks for.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use heycode_extensions::lifecycle::{
    InstalledVersions, LifecycleError, PluginLifecycle, PluginState, PluginStateStore,
};
use heycode_extensions::{PluginId, PluginVersion};

use super::lab::{Rng, shrink};

/// How many seeded operation sequences the property explores.
const SEQUENCES: u64 = 256;

/// Versions the generated sequences move between.
const VERSIONS: [&str; 4] = ["1.0.0", "1.1.0", "2.0.0", "2.0.1"];

/// One generated lifecycle operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Install(usize),
    Enable,
    Disable,
    Update(usize),
    Rollback,
    Remove,
    /// Retention pruning the cache, which is the cache's policy and not the
    /// lifecycle's assumption — a rollback target can disappear underneath it.
    Prune(usize),
    /// Retention restoring a version, so a pruned sequence can recover.
    Restore(usize),
}

/// In-memory state store that counts writes, so "a refused transition writes
/// nothing" is observable rather than assumed.
#[derive(Default)]
struct MemoryStore {
    states: Mutex<BTreeMap<String, PluginState>>,
    writes: Mutex<usize>,
}

impl MemoryStore {
    fn writes(&self) -> usize {
        *self.writes.lock().unwrap()
    }

    fn snapshot(&self) -> BTreeMap<String, PluginState> {
        self.states.lock().unwrap().clone()
    }
}

impl PluginStateStore for MemoryStore {
    fn load(&self) -> Result<BTreeMap<String, PluginState>, LifecycleError> {
        Ok(self.states.lock().unwrap().clone())
    }

    fn persist(&self, states: &BTreeMap<String, PluginState>) -> Result<(), LifecycleError> {
        *self.states.lock().unwrap() = states.clone();
        *self.writes.lock().unwrap() += 1;
        Ok(())
    }
}

/// A cache view whose retention the sequence can change.
#[derive(Default)]
struct MutableCache(Mutex<BTreeSet<String>>);

impl InstalledVersions for MutableCache {
    fn versions(&self, _id: &PluginId) -> Vec<PluginVersion> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|raw| PluginVersion::parse(raw.clone()).unwrap())
            .collect()
    }
}

/// What the lifecycle should believe, tracked independently of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Believed {
    active: String,
    previous: Option<String>,
    enabled: bool,
}

/// The failure classes the model predicts, matched by class rather than text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    NotInstalled,
    AlreadyInstalled,
    NothingToRollBackTo,
    VersionUnavailable,
    AlreadyAtVersion,
}

impl Refusal {
    fn of(error: &LifecycleError) -> Self {
        match error {
            LifecycleError::NotInstalled(_) => Self::NotInstalled,
            LifecycleError::AlreadyInstalled(_) => Self::AlreadyInstalled,
            LifecycleError::NothingToRollBackTo(_) => Self::NothingToRollBackTo,
            LifecycleError::VersionUnavailable { .. } => Self::VersionUnavailable,
            LifecycleError::AlreadyAtVersion { .. } => Self::AlreadyAtVersion,
            LifecycleError::Store(_) => unreachable!("the memory store never fails"),
            _ => unreachable!("a new refusal class needs a model arm"),
        }
    }
}

macro_rules! claim {
    ($condition:expr, $($message:tt)*) => {
        if !$condition {
            return Err(format!($($message)*));
        }
    };
}

/// Decide what the operation should do, from the model alone. The order of the
/// guards mirrors the order of the checks in `PluginLifecycle`, because "which
/// refusal wins" is itself part of the contract.
fn predict(
    op: Op,
    state: &Option<Believed>,
    cached: &BTreeSet<String>,
) -> Result<Option<Believed>, Refusal> {
    match op {
        Op::Install(index) => {
            let version = VERSIONS[index % VERSIONS.len()];
            if state.is_some() {
                return Err(Refusal::AlreadyInstalled);
            }
            if !cached.contains(version) {
                return Err(Refusal::VersionUnavailable);
            }
            Ok(Some(Believed {
                active: version.to_owned(),
                previous: None,
                enabled: true,
            }))
        }
        Op::Enable | Op::Disable => {
            let mut believed = state.clone().ok_or(Refusal::NotInstalled)?;
            believed.enabled = op == Op::Enable;
            Ok(Some(believed))
        }
        Op::Update(index) => {
            let version = VERSIONS[index % VERSIONS.len()];
            let mut believed = state.clone().ok_or(Refusal::NotInstalled)?;
            if believed.active == version {
                return Err(Refusal::AlreadyAtVersion);
            }
            if !cached.contains(version) {
                return Err(Refusal::VersionUnavailable);
            }
            believed.previous = Some(std::mem::replace(&mut believed.active, version.to_owned()));
            Ok(Some(believed))
        }
        Op::Rollback => {
            let mut believed = state.clone().ok_or(Refusal::NotInstalled)?;
            let previous = believed
                .previous
                .clone()
                .ok_or(Refusal::NothingToRollBackTo)?;
            if !cached.contains(&previous) {
                return Err(Refusal::VersionUnavailable);
            }
            believed.previous = Some(std::mem::replace(&mut believed.active, previous));
            Ok(Some(believed))
        }
        Op::Remove => {
            state.as_ref().ok_or(Refusal::NotInstalled)?;
            Ok(None)
        }
        Op::Prune(_) | Op::Restore(_) => unreachable!("retention is not a lifecycle operation"),
    }
}

/// Run one generated operation sequence against the lifecycle and the model,
/// returning the state both agree on at the end.
fn check(ops: &[Op]) -> Result<Option<Believed>, String> {
    let store = Arc::new(MemoryStore::default());
    let cache = Arc::new(MutableCache::default());
    for version in VERSIONS {
        cache.0.lock().unwrap().insert(version.to_owned());
    }
    let lifecycle = PluginLifecycle::new(
        Arc::clone(&store) as Arc<dyn PluginStateStore>,
        Arc::clone(&cache) as Arc<dyn InstalledVersions>,
    );
    let id = PluginId::new("acme/tools").unwrap();

    let mut believed: Option<Believed> = None;

    for op in ops {
        if let Op::Prune(index) | Op::Restore(index) = op {
            let version = VERSIONS[index % VERSIONS.len()].to_owned();
            let mut cached = cache.0.lock().unwrap();
            if matches!(op, Op::Prune(_)) {
                cached.remove(&version);
            } else {
                cached.insert(version);
            }
            continue;
        }

        let cached = cache.0.lock().unwrap().clone();
        let expected = predict(*op, &believed, &cached);
        let before = store.snapshot();
        let writes = store.writes();

        let actual = match op {
            Op::Install(index) => lifecycle.install(
                &id,
                &PluginVersion::parse(VERSIONS[index % VERSIONS.len()].to_owned()).unwrap(),
            ),
            Op::Enable => lifecycle.set_enabled(&id, true),
            Op::Disable => lifecycle.set_enabled(&id, false),
            Op::Update(index) => lifecycle.update(
                &id,
                &PluginVersion::parse(VERSIONS[index % VERSIONS.len()].to_owned()).unwrap(),
            ),
            Op::Rollback => lifecycle.rollback(&id).map(|_| ()),
            Op::Remove => lifecycle.remove(&id),
            Op::Prune(_) | Op::Restore(_) => unreachable!("retention was handled above"),
        };

        match (expected, actual) {
            (Ok(next), Ok(())) => {
                believed = next;
                claim!(
                    store.writes() == writes + 1,
                    "{op:?} succeeded without writing the store"
                );
            }
            (Err(refusal), Err(error)) => {
                let got = Refusal::of(&error);
                claim!(
                    got == refusal,
                    "{op:?} was refused as {got:?}, expected {refusal:?}"
                );
                claim!(
                    store.snapshot() == before,
                    "a refused {op:?} changed the recorded state"
                );
                claim!(
                    store.writes() == writes,
                    "a refused {op:?} wrote the store anyway"
                );
            }
            (Ok(_), Err(error)) => {
                return Err(format!(
                    "{op:?} should have succeeded, was refused: {error}"
                ));
            }
            (Err(refusal), Ok(())) => {
                return Err(format!("{op:?} should have been refused as {refusal:?}"));
            }
        }

        let listed = lifecycle.list().unwrap();
        match (&believed, listed.as_slice()) {
            (None, []) => {}
            (Some(believed), [state]) => {
                claim!(
                    state.id.as_str() == id.as_str()
                        && state.active.to_string() == believed.active
                        && state.previous.as_ref().map(ToString::to_string) == believed.previous
                        && state.enabled == believed.enabled,
                    "after {op:?} the recorded state is {state:?}, expected {believed:?}"
                );
            }
            (believed, listed) => {
                return Err(format!(
                    "after {op:?} the store lists {listed:?}, expected {believed:?}"
                ));
            }
        }
    }
    Ok(believed)
}

/// One seeded operation sequence.
fn generate(rng: &mut Rng) -> Vec<Op> {
    let length = 4 + rng.below(20);
    let mut ops = Vec::with_capacity(length);
    for _ in 0..length {
        ops.push(match rng.below(16) {
            0..=2 => Op::Install(rng.below(VERSIONS.len())),
            3..=6 => Op::Update(rng.below(VERSIONS.len())),
            7..=9 => Op::Rollback,
            10 => Op::Enable,
            11 => Op::Disable,
            12 => Op::Remove,
            13 | 14 => Op::Prune(rng.below(VERSIONS.len())),
            _ => Op::Restore(rng.below(VERSIONS.len())),
        });
    }
    ops
}

/// PL06's rule is the same one K10 applies to reloads: a transition that cannot
/// resolve its target changes nothing, and the last good state keeps serving.
/// For any generated order of the six operations interleaved with cache
/// retention changes, the recorded state matches the model, every refusal is
/// the class the model predicts, and a refused operation leaves the store
/// byte-identical and unwritten.
#[test]
fn every_generated_package_operation_order_matches_the_model_and_never_writes_on_refusal() {
    for seed in 1..=SEQUENCES {
        let ops = generate(&mut Rng::seeded(seed));
        if let Err(failure) = check(&ops) {
            let minimal = shrink(&ops, |candidate| check(candidate).is_err());
            let detail = check(&minimal).err().unwrap_or(failure);
            panic!(
                "seed {seed}: {detail}\n  minimal sequence ({} ops): {minimal:?}",
                minimal.len()
            );
        }
    }
}

/// Rollback swaps rather than assigns, so an operator who rolls back by mistake
/// is one command from undoing it. Over every generated sequence that reaches a
/// state with usable history, a second rollback returns to exactly the state the
/// first one left — an implementation that merely *assigned* `previous` would
/// strand the operator one version away with no way back.
#[test]
fn a_rollback_of_a_rollback_returns_to_exactly_where_it_started() {
    let mut involutions = 0_usize;
    for seed in 1..=SEQUENCES {
        let ops = generate(&mut Rng::seeded(seed));
        let base = match check(&ops) {
            Ok(state) => state,
            Err(failure) => panic!("seed {seed}: {failure}"),
        };
        let mut once = ops.clone();
        once.push(Op::Rollback);
        let after_one = match check(&once) {
            Ok(state) => state,
            Err(failure) => panic!("seed {seed} with one rollback: {failure}"),
        };
        let mut twice = once.clone();
        twice.push(Op::Rollback);
        let after_two = match check(&twice) {
            Ok(state) => state,
            Err(failure) => panic!("seed {seed} with two rollbacks: {failure}"),
        };
        if after_one != base && after_two != after_one {
            involutions += 1;
            assert_eq!(
                after_two, base,
                "seed {seed}: two rollbacks landed on {after_two:?}, not back on {base:?}"
            );
        }
    }
    assert!(
        involutions >= SEQUENCES as usize / 8,
        "only {involutions} of {SEQUENCES} sequences reached a state where two \
         rollbacks both moved; the involution is barely exercised"
    );
}
