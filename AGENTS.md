This is a pgrx extension for emitting telemetry about PostgreSQL at runtime.

- **Target Personas**: `docs/personas.md` defines the primary users and their operational priorities.

## Architectural Decision Records (ADRs)

ADRs are located in `docs/decisions/` and follow the [MADR](https://adr.github.io/madr) format.

- **Consult Existing ADRs**: When exploring, proposing, or modifying components, review relevant records in `docs/decisions/`.
- **Proposing Decisions**: When proposing architectural changes or significant technical trade-offs, follow the template in `docs/decisions/adr-template/README.md`.
- **Process & Concurrency Constraints**: See `.agents/rules/postgres-constraints.md` for critical constraints (e.g., no threading; non-blocking hooks).

## PostgreSQL Technical References

- **Process Architecture & Lifecycles**: `docs/postgres/lifecycle.md` details the Postmaster supervisor model, background worker lifecycles, shared-memory startup hooks, latches, and shutdown phases.
- **Extension Hook Catalog**: `docs/postgres/hooks.md` catalogs all shared-library extension hooks, function signatures, process contexts, chaining conventions, and callback registries.
- **SQL Hook Execution Matrix**: `docs/postgres/sql-hooks.md` details statement execution ordering across utility, transaction, and DML hooks.
- **Autovacuum Lifecycles**: `docs/postgres/autovacuum.md` details worker lifecycles, per-table transaction flows, hook availability, and telemetry mechanisms.
