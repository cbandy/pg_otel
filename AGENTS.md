This is a pgrx extension for emitting telemetry about PostgreSQL at runtime.

- **Target Personas**: `docs/personas.md` defines the primary users and their operational priorities.

## Development

- **Workspace Crates**:
  - `pg_otel`: The OpenTelemetry extension.
  - `sequencing`: Standalone contract-test extension for empirically cataloging PostgreSQL behavior before changing `pg_otel`.
- **Testing**: Use `cargo pgrx test` (NOT `cargo test`) to run tests.
- **Workflow**: Test with `cargo pgrx test pg15` for fast iteration; before completing changes that touch hooks, shared memory, or Postgres internals, run `cargo fmt --check`, `cargo clippy --all-targets`, and verify across every `pgXX` feature in the crate's `Cargo.toml`.
- **PostgreSQL Ground Truth**: Verify hook signatures and Postgres behavior against the C headers and backend source in `${PGRX_HOME:-~/.pgrx}/` (or via `cargo pgrx info path <pgXX>`) rather than assuming single-version behavior generalizes.
- **Version Conditionals**: In `std::cfg_select!`, match older PostgreSQL versions explicitly and place the newest behavior in the `_ =>` catch-all arm.

## Architectural Decision Records (ADRs)

ADRs are located in `docs/decisions/` and follow the [MADR](https://adr.github.io/madr) format.

- **Consult Existing ADRs**: When exploring, proposing, or modifying components, review relevant records in `docs/decisions/`.
- **Proposing Decisions**: When proposing architectural changes or significant technical trade-offs, follow the template in `docs/decisions/adr-template/README.md`.
- **Process & Concurrency Constraints**: See `.agents/rules/postgres-constraints.md` for critical constraints (e.g., no threading; non-blocking hooks).

## PostgreSQL Technical References

- **Process Architecture & Lifecycles**: `docs/postgres/lifecycle.md` details the Postmaster supervisor model, background worker lifecycles, shared-memory startup hooks, latches, and shutdown phases.
- **Extension Hook Catalog**: `docs/postgres/hooks.md` catalogs all shared-library extension hooks, function signatures, process contexts, chaining conventions, and callback registries.
- **Query, Transaction & SQL Hook Lifecycles**: `docs/postgres/sql-hooks.md` details query processing, transaction lifecycles, and hook execution ordering across utility, transaction, and DML commands.
- **Autovacuum Lifecycles**: `docs/postgres/autovacuum.md` details worker lifecycles, per-table transaction flows, hook availability, and telemetry mechanisms.
