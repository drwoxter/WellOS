# ADR-0002: Rust-First Backend, TypeScript/Next.js Frontend

Status: Accepted · Date: 2026-08-29

## Context

The clinical core needs memory safety, strong typing for clinical invariants
(units, IDs, state machines), and predictable performance. The UI needs rapid,
accessible iteration.

## Decision

- Backend: Rust workspace — Axum + Tokio (HTTP), SQLx (checked SQL), Serde,
  `rust_decimal` for clinical quantities (never floats in domain logic).
- Pure domain crate (`wellos-domain`) with no I/O so invariants are unit-tested
  in isolation.
- Frontend: Next.js with strict TypeScript, translations in EN/ES.

## Consequences

- Typed IDs (UUIDv7 wrappers) and unit-safe quantities make whole error
  classes unrepresentable.
- Slower feature velocity than a dynamic stack, accepted for a safety-critical
  core.
- Two toolchains in CI (cargo + npm).
