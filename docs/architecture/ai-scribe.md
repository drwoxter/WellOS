# Consultation recording and dMind AI scribe

Status: development prototype, synthetic data only. This document describes
how a consultation recording becomes a reviewable structured note draft, what
is and is not persisted, how providers are configured, and how failures are
recovered. It complements ADR-0007 (model gateway) and ADR-0008 (AIArtifact
lifecycle).

## Flow

```
browser                         wellos-server                    dmind-gateway
───────                         ─────────────                    ─────────────
consent ─────────────────────▶ POST /encounters/:id/recording-consent
                                 (own active consultation, audited, append-only)
record (MediaRecorder, RAM)
pause / resume / finish
finish ──────────────────────▶ POST /encounters/:id/scribe
   { audio_base64, mime_type,    rate limit (scribe family)
     duration_ms, language }     validate MIME / size / duration / language
                                 authorize (facility scope, care relationship)
                                 require recorded consent
                                 sha256(audio) ───────────────▶ TranscriptionProvider
                                                                 fake (default) | openai_compatible
                                 ◀──────────────────────────── transcript segments
                                 extract sections + flags (deterministic)
                                 validate ScribeDraftV1
                                 lock encounter, re-check consent, read note version
                                 supersede earlier awaiting drafts
                                 INSERT ai_artifacts (scribe_draft, A1, awaiting_review)
                                 audit + provenance events
                                 drop(audio)
review UI ◀───────────────────  { id, output, note_version, limitations, … }
insert / append / dismiss ────▶ POST /encounters/:id/scribe/:artifact/review
                                 lock encounter; artifact FOR UPDATE
                                 verify client version == current note version
                                 verify artifact's bound version == current note version
                                 merge (fill empty | append) + write note + approve
                                 in ONE transaction
```

## Consent and privacy boundaries

- Recording requires an explicit, per-encounter consent recorded by the
  treating practitioner (`encounter_recording_consents`, append-only;
  withdrawal is a new row with `granted = false`). The consent is audited as
  `encounter.recording.consent_recorded` and transcription is refused with
  `409 consent_required` without a current grant. The current decision is
  the newest row by `(recorded_at, id)`. Consent is checked before the audio
  is sent to the provider and **again inside the persistence transaction,
  under the same encounter lock the consent route writes through**: a
  withdrawal that commits while the provider is running causes the returned
  transcript to be discarded (`409 consent_required`, `ai.generation.failed`
  with stage `consent_withdrawn`) — no artifact, transcript or
  `encounter.scribe.transcribed` event is persisted.
- The browser holds the recording in memory only (`BlobPart[]`); nothing is
  written to `localStorage`, IndexedDB or a file. Discarding releases the
  chunks and the microphone tracks.
- The server receives the audio inside the request body, hashes it for
  provenance, hands the bytes to the provider and drops them. **Raw audio is
  never written to the database, disk or logs** in any provider mode; the
  integration suite scans every column of every table for the synthetic
  audio marker and its base64 encoding, and captures `tracing` output at
  `TRACE` level to assert that audio, transcript text, generated note text,
  clinician text and credentials never appear.
- What persists per recording: the SHA-256 of the audio (`input_hash` and a
  `recording:sha256:…` citation), MIME type, duration, segment/section
  counts, provider identity and the structured output below.
- Provider credentials (`WELLOS_SCRIBE_API_KEY`) live in server memory only;
  they are never returned in any response or written to logs.
- Dashboard cockpit preferences (`wellos.cockpit.v1`) store widget order,
  hidden widgets and density only. No patient, encounter or clinical value is
  ever placed in browser storage.

## Request validation

| Check | Bound | Error |
| --- | --- | --- |
| Rate limit | `WELLOS_RATE_SCRIBE_PER_MIN` per principal (default 6) | `429` + `Retry-After` |
| Body size | 9 MiB (base64 envelope) | `413 recording_too_large` |
| Audio size | 6 MiB decoded | `413 recording_too_large` |
| MIME type | `audio/webm`, `audio/ogg`, `audio/mp4`, `audio/mpeg`, `audio/wav` (parameters stripped) | `400 unsupported_media_type` |
| Duration | 1 s … 20 min | `400 recording_too_short` / `recording_too_long` |
| Language | `en` (default) or `es` | `400 validation_failed` |
| Consent | current grant by this practitioner | `409 consent_required` |
| Encounter | own, active `consultation` in facility scope | `404` / `403` / `409` (existing anti-probing semantics) |

## Providers

`dmind_gateway::scribe::TranscriptionProvider` is the only seam the server
knows. Selection is by environment and fails closed:

| `WELLOS_SCRIBE_PROVIDER` | Behaviour |
| --- | --- |
| `fake` (default) | Deterministic offline transcript scripted from a fixed EN/ES consultation; timecodes are proportional to `duration_ms`. Same input → same output. Used by CI, tests and the demo. Never touches the network. |
| `openai_compatible` | Multipart POST to `WELLOS_SCRIBE_ENDPOINT` (full URL) with `WELLOS_SCRIBE_MODEL` and bearer `WELLOS_SCRIBE_API_KEY`; `verbose_json` response mapped to segments. Bounded by `WELLOS_SCRIBE_TIMEOUT_SECS` (default 60) and `WELLOS_SCRIBE_MAX_RETRIES` (0–5, default 2, fixed back-off) on 5xx/429/timeouts only. Missing endpoint/model/key aborts startup. Covered by a mocked in-process HTTP server; CI makes no external calls. |

Destination policy (`validate_scribe_endpoint`, evaluated once at startup, fail closed): outside development the endpoint must be `https://`, name a DNS host (IP literals, `localhost`/`*.localhost` and embedded `user:pw@` are refused) and that host must appear verbatim in `WELLOS_SCRIBE_ALLOWED_HOSTS` (comma-separated exact `host` or `host:port` entries; no wildcards or paths; a bare entry matches only the default port). In development a loopback `http://` mock is accepted and the allowlist is optional but still enforced when set. The HTTP client never follows redirects, so a compromised or misconfigured endpoint cannot bounce the recording and credential to another host; a 3xx is reported as a provider outage.

Provider errors are classified, never forwarded verbatim:

| `ScribeError` | HTTP | Meaning |
| --- | --- | --- |
| `Unavailable` | `503 scribe_unavailable` | transient; the browser keeps the recording and offers **Retry transcription** |
| `InvalidOutput` | `502 scribe_invalid_output` | provider or structuring produced unusable output; retryable |
| `Rejected` | `422 audio_rejected` | provider refused the audio; record again |

Every failure is audited as `ai.generation.failed` with the stage
(`transcription` or `structure`) and no payload content.

## Structured output contract: `scribe-draft.v1`

The artifact `output` is a `ScribeDraftV1` (`wellos-domain::ai`), validated
server-side before insertion and stored with `output_schema =
"scribe-draft.v1"`:

```
ScribeDraftV1 {
  schema_version: "scribe-draft.v1",
  encounter_id, source_note_version: Option<i64>, language: "en" | "es",
  transcript: [ { index, start_ms, end_ms, speaker?, text, confidence } ],
  sections:   [ { section, text, confidence, review_needed, reasons[], segments[] } ],
  flags:      [ { kind: contradiction | uncertain, message, segments[], sections[] } ],
  transcription: ProviderInfo { provider, model, model_version },
  extraction:    ProviderInfo,
  generated_at, limitations[]
}
```

`validate()` enforces: exact schema version; sequential transcript indexes
with monotonic timecodes and non-empty text; `section` drawn from the eight
note sections (`reason_for_encounter`, `history_present_illness`,
`medical_history`, `review_of_systems`, `physical_exam`, `assessment`,
`plan`, `follow_up`) without duplicates and with non-empty text; every
section/flag `segments` reference resolving to a transcript index; every
flag `sections` reference resolving to a proposed section. A draft that
fails validation is never stored (`502 scribe_invalid_output`).

Section extraction is deterministic keyword/speaker mapping in the gateway
(no model call); `review_needed` and `reasons` are set for low-confidence
segments, patient-only sources and contradictions. Speaker attribution is
authoritative for the clinician-authored sections (examination, assessment,
plan, follow-up): only segments labelled as the clinician can fill them.
Providers that do not label speakers never populate those sections — a
matching statement is surfaced as an uncertainty flag instead, so nothing
spoken by an unknown party can be mistaken for the clinician's findings or
plan. The UI shows confidence, reasons and a **Check these** list of flags
with timecode links into the transcript.

## Note-version binding and application

- The draft records `source_note_version` (the version read under the
  encounter lock at insertion; `null` when no note exists yet) and cites
  `encounter_note:<id>:v<n>`.
- Any note save, vitals or diagnosis mutation, sign or cancellation
  supersedes awaiting scribe drafts in the same transaction (shared with the
  encounter-summary aid).
- `POST …/scribe/:artifact/review` with `decision: "apply"` requires the
  client's `version` to equal the current note version; otherwise `409
  version_conflict` (or `409 version_required`). Independently of what the
  client submits, the artifact itself must still be bound to the current
  note: its stored `note_version` (or, after partial application, the
  version its own last application produced) is compared with the current
  version under the lock and any mismatch — including `null`/`n` transitions
  — is refused with `409 artifact_stale`. Reloading a changed note therefore
  never makes an older draft applicable again; the workspace exposes the
  same comparison as `scribe_draft.stale` so the UI withholds insertion.
  Superseded/dismissed drafts return `409 artifact_not_reviewable`; closed
  encounters are refused before any write.
- Merge rules are enforced server-side and mirrored in the UI
  (`mergeSection`): `fill` succeeds only when the target section is empty
  (`409 section_not_empty` otherwise); `append` adds the proposal below the
  clinician's text separated by a blank line; a section can be applied once
  (`409 section_already_applied`); character limits apply
  (`400 section_limit_exceeded`). Clinician text is never overwritten.
- Application writes the merged draft note (bumping its version), records
  the applied sections in `review_detail`, sets the artifact to `approved`
  and audits `encounter.scribe.applied` + `ai.artifact.reviewed` —
  atomically. The response carries the
  merged sections and new version so the client hydrates exactly what was
  stored while preserving unsaved text typed meanwhile.
- `decision: "dismiss"` marks the draft `rejected` without touching the note
  (`encounter.scribe.dismissed`); it is a decision only and does not depend
  on the note version, so a stale draft can still be dismissed.

## Failure recovery (browser)

| Situation | Behaviour |
| --- | --- |
| Browser cannot record / no device / permission denied | Distinct messages; documentation continues manually |
| Provider `503`/`502`, network error, rate limit | Recording retained in memory; **Retry transcription** re-submits the same blob |
| `413`, `400` bounds | Specific message; **Record again** |
| Encounter closed meanwhile | Draft shown as no longer applicable |
| Note changed since draft | Draft flagged stale; each section must be checked before inserting; server refuses stale versions |
| Navigation / sign-out with an active recording | Recording is in memory only and is dropped with the page; unsaved note text is protected by the existing guard |

## Limitations

- Assistive only: the scribe cannot diagnose, prescribe, order, sign,
  complete or alter signed records; every insertion is an explicit clinician
  action against a draft note.
- The fake provider ignores the audio content; speaker labels and confidence
  values are synthetic in development and provider-supplied (not clinically
  validated) with a real adapter.
- Section extraction is deterministic keyword mapping, not a clinical
  language model; it is designed to be predictable for review, not
  exhaustive.
- Single-shot transcription (no streaming), 20-minute / 6 MiB cap, EN/ES
  only.
- No permanent audio storage, no re-listening after the request completes.
