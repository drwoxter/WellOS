# ADR-0011: Internationalization and Brand Studio Tokens

Status: Accepted · Date: 2026-08-29

## Context

WellOS is international (first languages: English, Spanish) and multi-tenant:
hospitals need their own look without code forks. Language and presentation
are also safety concerns (a misread label is a hazard).

## Decision

- All UI strings live in a typed dictionary (`apps/web/lib/i18n.ts`) keyed per
  language; EN and ES are mandatory for every key; the document `lang`
  attribute follows the selection. Clinical codes (LOINC) and units are never
  translated.
- Theming is token-based: two built-in themes (light/dark) via
  `data-theme` CSS custom properties; tenant brand tokens are stored as JSONB
  on the tenant (`brand` column) — configuration, not forks (Brand Studio
  direction).
- AI summaries are generated in the clinician's language (the gateway request
  carries the language).

## Consequences

- Adding a language is additive dictionary work plus review.
- Full locale formatting (dates, decimal separators) and RTL support are
  future work; the token structure anticipates them.
