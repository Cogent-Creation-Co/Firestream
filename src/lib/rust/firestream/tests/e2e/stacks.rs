// Canonical stack set for the e2e harness.
//
// HTTP probe paths/ports/credentials are NOT here — they live in
// `probes::for_stack` because they are runtime-type-specific and not part of
// any user-facing configuration. Adding a stack here without adding a probe
// matrix in `probes.rs` would silently degrade to a TCP-only check.

/// Canonical stacks driven by `make test-e2e`. Order matches the macro
/// invocations in `tests/e2e.rs`. Typed as a sized array (rather than `&[&str]`)
/// so rust-analyzer doesn't false-positive on the const-context slice coercion.
pub const CANONICAL: [&str; 9] = [
    "airflow",
    "postgresql",
    "redis",
    "kafka",
    "spark",
    "jupyterhub",
    "superset",
    "odoo",
    "supabase",
];

/// True iff `name` is a recognised canonical stack.
pub fn is_canonical(name: &str) -> bool {
    CANONICAL.iter().any(|n| *n == name)
}
