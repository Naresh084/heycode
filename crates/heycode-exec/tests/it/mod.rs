mod exact_process;
mod filesystem_contract;
mod filesystem_policy;
mod lsp_service;
mod retained_output;
mod sandbox_capabilities;
mod sandbox_routing;
mod shell_contract;
mod subprocess_contract;
mod subprocess_raw;
mod terminal_registry;

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
