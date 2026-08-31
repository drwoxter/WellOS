# ADR-0010: Object Storage for Large Medical Artifacts

Status: Accepted (direction) · Date: 2026-08-29

## Context

Imaging, documents, waveforms, and model artifacts do not belong in PostgreSQL
rows; they need residency-aware, encrypted, content-addressed storage.

## Decision

- Large binary artifacts will live behind an S3-compatible abstraction, one
  bucket namespace per regional cell (residency), with per-tenant encryption
  context and content hashes recorded in PostgreSQL rows that carry the
  authorization and audit context.
- The database remains the source of truth for existence, provenance, and
  access control; object storage holds bytes only.
- Not implemented in the current slice (no large artifacts yet); the seam is
  the artifact tables referencing external content by hash + URI.

## Consequences

- No blob ever bypasses policy/audit (access always resolves through the DB
  record).
- Local development will use MinIO or equivalent via docker-compose when the
  first consumer (imaging/documents) lands.
