# Interoperability Standards Roadmap

## Position

Standards before proprietary integrations. The internal domain model is **not**
FHIR — it is a purpose-built clinical model (ADR-0005); FHIR R4 is the exchange
boundary, mapped at the edge.

## Implemented today

- **FHIR R4 read facade**: `GET /fhir/r4/Patient/{id}`,
  `GET /fhir/r4/Observation/{id}`, `GET /fhir/r4/ServiceRequest/{id}` — minimal
  conformant resources mapped from internal records, fully authorized and
  audited like every other read.
- **LOINC** codes for laboratory analytes on service requests/observations.
- **UCUM**-aware quantity handling with refusal on unknown conversions.

## Roadmap (ordered)

1. FHIR search parameters and Bundles for the implemented resources.
2. CapabilityStatement + automated FHIR validation in CI (contract tests
   against the official validator).
3. Inbound results via FHIR `DiagnosticReport`/`Observation` (in addition to
   the synthetic adapter), then HL7v2 ORU translation at an adapter, never in
   the core.
4. SMART on FHIR launch for third-party apps (after real identity provider).
5. Bulk Data (flat FHIR) export for research/de-identification pipelines and
   data-subject portability.
6. Terminology service integration (LOINC/SNOMED CT/ICD) with versioned
   value-set binding; SNOMED licensing per jurisdiction.
7. IHE profiles as required per market; national-registry adapters live in
   `Connect` modules per cell.

## Non-goals

Reimplementing a general-purpose FHIR server; storing FHIR resources as the
source of truth; claiming conformance not backed by automated validation.
