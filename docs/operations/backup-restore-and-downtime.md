# Backup, Restore, and Downtime

Development-stage procedures. Production automation (scheduled backups, PITR,
tested restores per regional cell) is roadmap item 10.

## Backup (development)

```bash
docker exec wellos-pg pg_dump -U wellos -Fc wellos > wellos-$(date +%Y%m%d%H%M%S).dump
```

## Restore smoke procedure

```bash
docker exec -i wellos-pg createdb -U wellos wellos_restore
docker exec -i wellos-pg pg_restore -U wellos -d wellos_restore < wellos-<ts>.dump
# verify: row counts of patients/observations/audit_events match the source
docker exec wellos-pg psql -U wellos -d wellos_restore -c \
  "select (select count(*) from patients), (select count(*) from observations), (select count(*) from audit_events);"
```

Restores must be verified against audit-event counts — an audit gap is a
restore failure even if clinical rows match.

## Downtime behavior

- **Database down**: `/ready` fails; API returns 5xx; no partial clinical
  writes are possible (single-transaction writes).
- **AI provider down**: clinical workflow continues; artifacts recorded as
  `unavailable`; no retry storm (generation is post-commit, per result).
- **Web UI down**: API remains directly usable; FHIR facade unaffected.

## Production direction (per regional cell)

- WAL archiving + PITR; encrypted offsite copies within the cell's
  jurisdiction (residency rule: backups never leave the cell region).
- Quarterly restore drills with checksum + audit-count verification.
- Documented RPO/RTO targets set with clinical stakeholders (critical-result
  flow tolerates minutes, not hours).
