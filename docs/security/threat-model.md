# Threat Model

Scope: the implemented system (web UI, API server, PostgreSQL, dMind model
and transcription gateways with `disabled`/`openai_compatible` providers;
`fake` fixtures exist only in `dev-fixtures` builds). Method: STRIDE per
trust boundary. The repository ships synthetic data only; the model is
written for the intended clinical use.

## Assets

Patient records (future PHI), audit trail integrity, AI artifacts and their
provenance, credentials/tokens, tenant isolation guarantees, scheduling
integrity (one patient per slot, one slot per resource), patient-access
grants, personal-calendar busy intervals, retained addresses and live
transport locations, notification outbox contents, notification adapter
secrets (SMTP password, webhook signing secret), location encryption keys.

## STRIDE summary

| Threat | Vector | Mitigations (implemented) | Planned |
| --- | --- | --- | --- |
| Spoofing | Forged identity | OIDC JWT validation against static or discovery-resolved JWKS (issuer-pinned, HTTPS-only, cached with bounded auto-refresh; signature/iss/aud/exp/nbf/iat, asymmetric algorithms only), `(issuer, sub)`→local identity mapping; optional MFA enforcement from validated `amr`/`acr` claims (fails closed); hashed scoped service credentials with expiry/revocation and an audited admin API; opaque hashed browser sessions with rotation/revocation; dev tokens only in explicit local development (startup fails closed otherwise); tenant/roles derived server-side only | Token binding, SCIM provisioning |
| Tampering | Modify clinical history or audit | Append-only observations & audit; amendments linked, never overwrite; parameterized SQL throughout | Audit hash chain; Postgres RLS; WORM storage for audit |
| Repudiation | Deny having acted | Every access/transition/AI event audited with actor, purpose, correlation id; break-glass requires reason | Time-stamping service |
| Information disclosure | Cross-tenant reads, resource-ID probing, PHI in logs/events, token theft via XSS | Tenant scoping in all queries; cross-tenant probes return 404 identical to missing resources (denial still audited); outbox/logs carry ids not clinical payloads; no access tokens in cookies — only opaque hashed `wss_` sessions in HttpOnly cookies via the BFF, CSRF double-submit on state-changing requests, security headers (nosniff/no-referrer/frame-deny/CSP/HSTS); external AI off by default (`disabled` provider), `WELLOS_ALLOW_EXTERNAL_AI` opt-in, exact host allowlist + HTTPS + no redirects for egress, consent gate, API keys/prompts/transcripts/responses never logged | Field-level encryption; redaction layer at model gateway |
| Denial of service | Flooding ingestion, login endpoints or AI calls | Idempotent ingestion; AI async and non-blocking; bounded DB pool; shared PostgreSQL-backed rate limiting (anonymous login/callback per hashed client address, per-principal patient search / credential admin / general API, 429 + Retry-After, fail-closed store); per-user break-glass rate limit; transcription rate-limit family; AI provider timeouts, response-size caps, bounded retries, per-replica concurrency limit, hourly per-tenant/per-task quotas and reuse of identical artifacts (no duplicate spend) | Token-bucket limits, WAF |
| Elevation of privilege | Role abuse, break-glass misuse, purpose-header widening, cross-facility access | Central least-privilege policy; typed purpose-of-use matrix (headers can only narrow access); facility scope enforced centrally with trusted-relationship facility derivation and an explicit NULL-facility allowlist; service credentials scope-limited and unable to act as humans; break-glass requires dedicated role + emergency purpose, read-only, same-tenant, facility-covered, bounded reason, per-user hourly limit, immutable event with mandatory privacy/security review | Anomaly detection on break-glass patterns |

## Abuse cases exercised by tests

Cross-tenant access (404, incl. break-glass), nurse closing a loop
(denied), research role touching clinical data (denied), audit read by
non-audit roles or wrong purpose (denied), unauthorized physicians using
break-glass (denied), break-glass mutations (denied), break-glass rate
limiting and once-only review, expired/revoked/wrong-scope/malformed
service credentials (rejected), invalid OIDC signature/issuer/audience/
expiry/subject (rejected), unknown `kid` after rotation (refreshed) and
unknown keys with unavailable JWKS (rejected), missing/malformed MFA claims
under MFA policy (rejected), expired/idle/revoked/rotated-away sessions
(rejected), missing/wrong CSRF token on writes (rejected), cross-tenant
service-credential admin (404), emergency search without the break-glass
role (denied), dev tokens/fake providers/synthetic seed in staging or
production and on builds without `dev-fixtures` (refused at startup),
missing or unknown `WELLOS_ENV` (refused), schema-invalid or uncited model
output (rejected, nothing stored), quota exhaustion (429), identical AI
request (reused, provider not called), duplicate inbound
results (no duplicates), stale version writes (409), cross-facility
reads/search/registration/encounters/orders/worklists (denied or filtered),
break-glass outside its assigned facility (denied), OIDC login
state/nonce/PKCE mismatch, replayed or expired login transactions, provider
and token-exchange errors (all rejected; no provider tokens in responses,
cookies or URLs), rate-limit exhaustion incl. parallel requests and
unavailable store (denied with Retry-After / fail-closed).

## dMind Access (scheduling, self-service, calendars, transport)

Additional trust boundaries introduced by Access v1 (see
`docs/architecture/dmind-access.md` and hazards H-23 to H-33):

| Threat | Vector | Mitigations (implemented) | Planned |
| --- | --- | --- | --- |
| Spoofing a patient or representative | Browser-supplied `patient_id`; claim by name/birth date/MRN; expired/revoked grant | `/api/v1/me/...` derives the patient set from active `patient_access_grants` of the authenticated OIDC subject only; grants are staff-issued (`patient_grant.manage`), audited, time-boxed and revocable; the selected patient must be in the grant set or the response is the generic `not_found` | Self-service grant requests with staff verification queue |
| Tampering with bookings | Concurrent holds/acceptances, stale versions, replayed requests | `resource_bookings` exclusion constraint; every booking/transition in one transaction with row locks; optimistic `version` (`409 stale_version`); `Idempotency-Key` with body hash (`409 idempotency_conflict`); hold bound to the principal who placed it | — |
| Tampering with AI output | Model adds/removes candidates, demotes urgent patients, dispatches transport | Typed Access operations validate that rankings are permutations of supplied candidate ids and that recovery orders cover every entry once; deterministic urgency/waiting floors applied after ranking; no AI operation has a write path to bookings, waitlists or transport; emergency transport requires a human with `transport.coordinate` | — |
| Information disclosure (calendar) | Raw `.ics` retained; titles/attendees stored; staff reading busy time | In-memory parse with hard bounds; only busy intervals + tz + hash persisted; `scheduling_calendar` consent; staff see only the matcher's rejection reason, never intervals; disconnect deletes derived data; connect/sync/disconnect audited | — |
| Information disclosure (location) | Addresses/coordinates in clear; unauthorised reads; stale live trails | AES-256-GCM with configured keyring (`WELLOS_LOCATION_ENCRYPTION_KEYS`), fail-closed in staging/production; `scheduling_location` / `transport_coordination` consents; reads limited to `transport.coordinate` on the specific episode and audited; live positions only while `scheduled`/`en_route`/`picked_up`, purged after `WELLOS_LIVE_LOCATION_TTL_SECS`; transport personnel have logistics-only permissions (no chart) | HSM/KMS-backed key custody |
| Information disclosure (notifications) | PHI in subjects, logs, webhook bodies | Subjects and webhook payloads carry ids, kind, time and locale only; bodies are rendered server-side per channel; logs never include recipient content; SMTP requires TLS (`starttls`/`implicit`, `none` refused outside development) and the webhook is HMAC-SHA256 signed (`x-wellos-signature` over `timestamp.body`), HTTPS, host-allowlisted, no redirects | — |
| Denial of service | Calendar bombs; matcher abuse; notification storms; worker duplication | ICS bounds (1 MB / 2 000 events / 400 occurrences / 5 000 intervals / 180 days / 200 000 lines); `scheduling` rate-limit family (`WELLOS_RATE_SCHEDULING_PER_MIN`); bounded candidate sets and one model call per ranking; durable notifications claimed with `FOR UPDATE SKIP LOCKED`, bounded retries and dead-letter; hold expiry and offer caps (`MAX_OFFERS_PER_EVENT`) | — |
| Elevation of privilege | Catalog/specialty as permission; representative reading charts; transport operator reading clinical data | Catalog membership grants nothing; authorization is the functional-role matrix (`catalog.*`, `resource.manage`, `scheduling.*`, `patient.self_service`, `patient_grant.manage`, `waitlist.manage`, `transport.coordinate`, `capacity.review`, `notification.read`) plus facility and care-team relationships; `patient_representative` and `transport_coordinator` have no chart/result/note/risk permissions; purpose-of-use enforced on every Access route | — |

Abuse cases exercised by tests: self-service without a grant / with a revoked
grant / for a sibling patient (404), concurrent acceptance of one slot or one
waitlist offer (exactly one wins), a required room shared by two
professionals (never double booked), red-flag text (forced to clinical
triage; matching blocked), dMind disabled/degraded (deterministic order,
booking unaffected), rankings that are not permutations (rejected), staff
override without reason or outside scope (rejected), notification retry and
dead-letter, transport without consent or without an encryption key in a
deployed environment (refused), emergency transport without a human
coordinator (refused), malformed/oversized ICS (bounded, nothing stored).

## Residual risk: rate limiting

Fixed windows allow a boundary burst of up to twice the per-minute limit.
Anonymous denials are logged (no principal exists to audit); the anonymous
key hashes the socket peer address; an asserted client address
(`x-wellos-client-address` or the rightmost `x-forwarded-for` entry) is
honored only when the immediate peer is listed in
`WELLOS_TRUSTED_PROXIES`. The PKCE code verifier is
stored plaintext in the short-lived single-use `login_transactions` row;
encryption at rest is delegated to the database deployment.

## Residual risk: break-glass abuse controls

The per-user hourly rate limit is persistent (database-backed) and
configurable, but there is no anomaly detection or automatic alerting on
unusual break-glass volume; detection relies on the mandatory post-hoc
review queue. An authorized emergency user within the rate limit can read
any same-tenant patient record; the compensating controls are the immutable
event trail and privacy-role review.

## Residual risk: dMind Access

The exclusion constraint protects single resources; combined multi-resource
feasibility, patient busy time and travel are checked transactionally. Live
location purge and notification delivery depend on the scheduling worker
being alive (runbook alert). Delivery receipts from e-mail providers are not
consumed. Representative verification is a staff process outside the
system. Key custody for location encryption is a deployment concern.

## Assumptions

TLS terminated by the deployment environment; database credentials via
environment; no untrusted code in the deployment; single-cell deployment (no
cross-cell traffic to protect yet).
