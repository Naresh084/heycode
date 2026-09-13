mod escape_matrix;
mod linux_runtime;
mod path_policy_matrix;
mod seatbelt_runtime;
mod windows_runtime;

/// libtest's name for `test` declared in `module`, given `module_path!()` at
/// the call site.
///
/// Several tests re-exec the test binary and select their child entry point
/// with `--exact`, which matches the full `a::b::name` path. libtest drops the
/// binary crate root from that path, so strip the leading segment. Deriving the
/// name rather than hard-coding it keeps these helpers working if the test
/// files move between modules again.
pub fn test_name(module: &str, test: &str) -> String {
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    format!("{module}::{test}")
}
