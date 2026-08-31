# Product Vision

WellOS is an international, AI-native hospital operating system. dMind is its
governed intelligence layer for clinical, operational, research, and
machine-learning workloads.

## Principles

1. Clinical safety before convenience.
2. Human accountability for every consequential action.
3. AI outputs are suggestions and artifacts — never silently clinical truth.
4. Standards (FHIR, LOINC, UCUM) before proprietary integrations.
5. Modular monolith before microservice sprawl.
6. Rust-first clinical core.
7. Local-first privacy and jurisdictional data residency (regional cells).
8. Zero trust and least privilege.
9. Configuration instead of customer forks.
10. Evidence, citations, and provenance everywhere.
11. Graceful degradation when AI or noncritical services fail.
12. Accessibility (WCAG 2.2 AA) as a safety control.
13. No real PHI in code, fixtures, tests, logs, prompts, or analytics.

## Module map (target state)

dMind Core, Specialty Engines, Specialty Graph, Connect, Model Gateway,
Clinical Board, Scale Fabric, Guard, Research, ML Foundry, Compliance Fabric,
Consent & Rights, Evidence, Brand Studio, Context Fabric, Agent Runtime,
Multimodal Hub, Workflow Studio, Safety & Evaluation Lab, Command Center,
Learning Loop, Exchange.

Today only a thin slice exists: dMind Core (rules + loop), Model Gateway (fake
provider), Guard (policy/audit), Consent & Rights (purpose-based consent), and
a minimal Brand Studio (tenant theme tokens).

## First proof: Closed-Loop Diagnostic Result

The first vertical slice proves the architecture end to end: order → synthetic
result → deterministic criticality rule → alert/task → assistive AI summary →
human review → patient notification → closed loop, with audit and provenance at
every step. Everything else in the module map builds on the same primitives
(policy, audit, outbox, artifacts, consent).
