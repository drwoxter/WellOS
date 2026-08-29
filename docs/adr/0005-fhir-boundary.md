# ADR-0005: FHIR at the Boundary, Purpose-Built Domain Model Inside

Status: Accepted · Date: 2026-08-29

## Context

FHIR is the interoperability lingua franca, but FHIR resources are exchange
documents: storing them as the source of truth pushes optionality and
polymorphism into every code path and weakens invariants (loop states, unit
safety, versioning).

## Decision

The internal model is purpose-built Rust types with strict invariants. FHIR R4
is an edge mapping: a read facade today (`/fhir/r4/Patient|Observation|
ServiceRequest`), full search/Bundles/validation on the roadmap. LOINC and
UCUM are used natively inside the domain where they strengthen safety.

## Consequences

- Domain invariants stay enforceable; FHIR conformance is a mapping concern
  testable in isolation (validator-backed contract tests planned).
- Mapping code is a permanent tax, accepted deliberately.
- No claim of FHIR-server conformance until automated validation says so.
