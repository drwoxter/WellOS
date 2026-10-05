//! Structural validator for the bounded FHIR R4 subset WellOS exchanges at
//! its boundary (ADR-0005): `Patient`, `ServiceRequest`, `Specimen`,
//! `Observation`, `DiagnosticReport`, `DocumentReference`, `ImagingStudy`.
//!
//! This is not a conformance engine: it checks the required elements,
//! cardinalities, value sets and reference shapes of the profile WellOS
//! actually emits and accepts, so contract tests fail when the mapping
//! drifts and inbound deliveries are refused before they touch the record.
//! Full FHIR-server conformance is explicitly not claimed.

use serde_json::Value;

pub const SUPPORTED_RESOURCES: &[&str] = &[
    "Patient",
    "ServiceRequest",
    "Specimen",
    "Observation",
    "DiagnosticReport",
    "DocumentReference",
    "ImagingStudy",
];

pub const SERVICE_REQUEST_STATUS: &[&str] = &[
    "draft",
    "active",
    "on-hold",
    "revoked",
    "completed",
    "entered-in-error",
    "unknown",
];
pub const SERVICE_REQUEST_INTENT: &[&str] = &["proposal", "plan", "order", "original-order"];
pub const REQUEST_PRIORITY: &[&str] = &["routine", "urgent", "asap", "stat"];
pub const SPECIMEN_STATUS: &[&str] = &[
    "available",
    "unavailable",
    "unsatisfactory",
    "entered-in-error",
];
pub const OBSERVATION_STATUS: &[&str] = &[
    "registered",
    "preliminary",
    "final",
    "amended",
    "corrected",
    "cancelled",
    "entered-in-error",
    "unknown",
];
pub const DIAGNOSTIC_REPORT_STATUS: &[&str] = &[
    "registered",
    "partial",
    "preliminary",
    "final",
    "amended",
    "corrected",
    "appended",
    "cancelled",
    "entered-in-error",
    "unknown",
];
pub const DOCUMENT_REFERENCE_STATUS: &[&str] = &["current", "superseded", "entered-in-error"];
pub const IMAGING_STUDY_STATUS: &[&str] = &[
    "registered",
    "available",
    "cancelled",
    "entered-in-error",
    "unknown",
];
pub const OBSERVATION_VALUE_ELEMENTS: &[&str] = &[
    "valueQuantity",
    "valueCodeableConcept",
    "valueString",
    "valueBoolean",
    "valueInteger",
    "valueRange",
    "valueRatio",
    "valueSampledData",
    "valueTime",
    "valueDateTime",
    "valuePeriod",
];

/// Validation problems as FHIR-style element paths with a short reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub path: String,
    pub reason: String,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

struct Check<'a> {
    root: &'a str,
    issues: Vec<Issue>,
}

impl Check<'_> {
    fn fail(&mut self, path: &str, reason: &str) {
        self.issues.push(Issue {
            path: format!("{}.{path}", self.root),
            reason: reason.to_string(),
        });
    }

    fn require_string(&mut self, v: &Value, path: &str) -> Option<String> {
        match v.pointer(&json_pointer(path)).and_then(Value::as_str) {
            Some(s) if !s.trim().is_empty() => Some(s.to_string()),
            Some(_) => {
                self.fail(path, "must not be empty");
                None
            }
            None => {
                self.fail(path, "required string element is missing");
                None
            }
        }
    }

    fn require_code(&mut self, v: &Value, path: &str, allowed: &[&str]) {
        if let Some(s) = self.require_string(v, path) {
            if !allowed.contains(&s.as_str()) {
                self.fail(path, &format!("'{s}' is not in the required value set"));
            }
        }
    }

    fn require_reference(&mut self, v: &Value, path: &str, resource_type: Option<&str>) {
        let Some(r) = self.require_string(v, &format!("{path}/reference")) else {
            return;
        };
        if !reference_is_valid(&r, resource_type) {
            self.fail(
                &format!("{path}/reference"),
                &format!("'{r}' is not a relative reference of the form Type/id"),
            );
        }
    }

    fn require_codeable_concept(&mut self, v: &Value, path: &str) {
        let cc = v.pointer(&json_pointer(path));
        let coding = cc.and_then(|c| c.get("coding")).and_then(Value::as_array);
        let text = cc.and_then(|c| c.get("text")).and_then(Value::as_str);
        match coding {
            Some(list) if !list.is_empty() => {
                for (i, c) in list.iter().enumerate() {
                    if c.get("code")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    {
                        self.fail(
                            &format!("{path}/coding/{i}/code"),
                            "coding.code is required",
                        );
                    }
                    if c.get("system")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    {
                        self.fail(
                            &format!("{path}/coding/{i}/system"),
                            "coding.system is required in this profile",
                        );
                    }
                }
            }
            _ if text.is_some_and(|t| !t.trim().is_empty()) => {}
            _ => self.fail(path, "requires at least one coding or a text"),
        }
    }

    fn require_array(&mut self, v: &Value, path: &str, min: usize) -> Vec<Value> {
        match v.pointer(&json_pointer(path)).and_then(Value::as_array) {
            Some(a) if a.len() >= min => a.clone(),
            Some(_) => {
                self.fail(path, &format!("requires at least {min} element(s)"));
                Vec::new()
            }
            None if min == 0 => Vec::new(),
            None => {
                self.fail(path, "required array element is missing");
                Vec::new()
            }
        }
    }

    fn optional_datetime(&mut self, v: &Value, path: &str) {
        if let Some(s) = v.pointer(&json_pointer(path)) {
            match s.as_str() {
                Some(s) if chrono::DateTime::parse_from_rfc3339(s).is_ok() => {}
                Some(s) if chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() => {}
                _ => self.fail(path, "must be an ISO 8601 date or dateTime"),
            }
        }
    }
}

fn json_pointer(path: &str) -> String {
    format!("/{path}")
}

/// Relative literal reference: `Type/id` where `Type` is a FHIR resource
/// type (upper camel case) and `id` is a FHIR id (`[A-Za-z0-9\-\.]{1,64}`),
/// or a `#id` reference to a contained resource.
pub fn reference_is_valid(reference: &str, resource_type: Option<&str>) -> bool {
    if let Some(local) = reference.strip_prefix('#') {
        return resource_type.is_none() && id_is_valid(local);
    }
    let Some((ty, id)) = reference.split_once('/') else {
        return false;
    };
    let ty_ok = ty.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && ty.chars().all(|c| c.is_ascii_alphanumeric());
    ty_ok && id_is_valid(id) && resource_type.is_none_or(|want| want == ty)
}

pub fn id_is_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

/// DICOM UID: 1–64 characters, dot-separated non-empty numeric components
/// without leading zeros (except the component `0` itself).
pub fn dicom_uid_is_valid(uid: &str) -> bool {
    !uid.is_empty()
        && uid.len() <= 64
        && uid.split('.').all(|part| {
            !part.is_empty()
                && part.chars().all(|c| c.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'))
        })
}

/// Validate a resource against the WellOS boundary profile.
///
/// `require_id` is set for resources read from the facade (they must carry
/// the server-assigned logical id); inbound resources may omit it.
pub fn validate(resource: &Value, require_id: bool) -> Result<(), Vec<Issue>> {
    let Some(ty) = resource.get("resourceType").and_then(Value::as_str) else {
        return Err(vec![Issue {
            path: "resourceType".into(),
            reason: "required".into(),
        }]);
    };
    if !SUPPORTED_RESOURCES.contains(&ty) {
        return Err(vec![Issue {
            path: "resourceType".into(),
            reason: format!("'{ty}' is outside the supported subset"),
        }]);
    }
    let mut c = Check {
        root: ty,
        issues: Vec::new(),
    };
    if require_id {
        match resource.get("id").and_then(Value::as_str) {
            Some(id) if id_is_valid(id) => {}
            Some(_) => c.fail("id", "is not a valid FHIR id"),
            None => c.fail("id", "required"),
        }
    }
    match ty {
        "Patient" => {
            let names = c.require_array(resource, "name", 1);
            for (i, n) in names.iter().enumerate() {
                if n.get("family").and_then(Value::as_str).is_none()
                    && n.get("given")
                        .and_then(Value::as_array)
                        .is_none_or(Vec::is_empty)
                    && n.get("text").and_then(Value::as_str).is_none()
                {
                    c.fail(&format!("name/{i}"), "requires family, given or text");
                }
            }
            c.optional_datetime(resource, "birthDate");
        }
        "ServiceRequest" => {
            c.require_code(resource, "status", SERVICE_REQUEST_STATUS);
            c.require_code(resource, "intent", SERVICE_REQUEST_INTENT);
            c.require_codeable_concept(resource, "code");
            c.require_reference(resource, "subject", Some("Patient"));
            if resource.get("priority").is_some() {
                c.require_code(resource, "priority", REQUEST_PRIORITY);
            }
            if resource.get("encounter").is_some() {
                c.require_reference(resource, "encounter", Some("Encounter"));
            }
            c.optional_datetime(resource, "authoredOn");
        }
        "Specimen" => {
            if resource.get("status").is_some() {
                c.require_code(resource, "status", SPECIMEN_STATUS);
            }
            c.require_codeable_concept(resource, "type");
            c.require_reference(resource, "subject", Some("Patient"));
            for (i, r) in c.require_array(resource, "request", 0).iter().enumerate() {
                let path = format!("request/{i}");
                if !r
                    .get("reference")
                    .and_then(Value::as_str)
                    .is_some_and(|s| reference_is_valid(s, Some("ServiceRequest")))
                {
                    c.fail(&path, "must reference a ServiceRequest");
                }
            }
            c.optional_datetime(resource, "collection/collectedDateTime");
        }
        "Observation" => {
            c.require_code(resource, "status", OBSERVATION_STATUS);
            c.require_codeable_concept(resource, "code");
            c.require_reference(resource, "subject", Some("Patient"));
            let values: Vec<&str> = OBSERVATION_VALUE_ELEMENTS
                .iter()
                .copied()
                .filter(|k| resource.get(k).is_some())
                .collect();
            let absent = resource.get("dataAbsentReason").is_some();
            match (values.len(), absent) {
                (0, false) => c.fail("value[x]", "a value or dataAbsentReason is required"),
                (1, false) | (0, true) => {}
                (1, true) => c.fail(
                    "value[x]",
                    "value and dataAbsentReason are mutually exclusive",
                ),
                _ => c.fail("value[x]", "at most one value[x] element is allowed"),
            }
            if let Some(q) = resource.get("valueQuantity") {
                if !q.get("value").is_some_and(Value::is_number) {
                    c.fail("valueQuantity/value", "must be a number");
                }
                if q.get("unit")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    c.fail("valueQuantity/unit", "required in this profile");
                }
                if q.get("system").and_then(Value::as_str) != Some("http://unitsofmeasure.org") {
                    c.fail("valueQuantity/system", "must be UCUM");
                }
            }
            if resource.get("valueCodeableConcept").is_some() {
                c.require_codeable_concept(resource, "valueCodeableConcept");
            }
            c.optional_datetime(resource, "effectiveDateTime");
            c.optional_datetime(resource, "valueDateTime");
            if resource.get("interpretation").is_some() {
                for (i, _) in c
                    .require_array(resource, "interpretation", 1)
                    .iter()
                    .enumerate()
                {
                    c.require_codeable_concept(resource, &format!("interpretation/{i}"));
                }
            }
        }
        "DiagnosticReport" => {
            c.require_code(resource, "status", DIAGNOSTIC_REPORT_STATUS);
            c.require_codeable_concept(resource, "code");
            c.require_reference(resource, "subject", Some("Patient"));
            for (i, r) in c.require_array(resource, "result", 0).iter().enumerate() {
                if !r.get("reference").and_then(Value::as_str).is_some_and(|s| {
                    reference_is_valid(s, Some("Observation"))
                        || reference_is_valid(s, None) && s.starts_with('#')
                }) {
                    c.fail(&format!("result/{i}"), "must reference an Observation");
                }
            }
            for (i, r) in c.require_array(resource, "basedOn", 0).iter().enumerate() {
                if !r
                    .get("reference")
                    .and_then(Value::as_str)
                    .is_some_and(|s| reference_is_valid(s, Some("ServiceRequest")))
                {
                    c.fail(&format!("basedOn/{i}"), "must reference a ServiceRequest");
                }
            }
            for (i, _) in c
                .require_array(resource, "conclusionCode", 0)
                .iter()
                .enumerate()
            {
                c.require_codeable_concept(resource, &format!("conclusionCode/{i}"));
            }
            for (i, inner) in c.require_array(resource, "contained", 0).iter().enumerate() {
                if inner.get("resourceType").and_then(Value::as_str) != Some("Observation") {
                    c.fail(
                        &format!("contained/{i}"),
                        "only contained Observations are accepted",
                    );
                    continue;
                }
                if inner
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(|s| !id_is_valid(s))
                {
                    c.fail(
                        &format!("contained/{i}/id"),
                        "contained resources require an id",
                    );
                }
                if let Err(mut inner_issues) = validate(inner, false) {
                    for issue in &mut inner_issues {
                        issue.path = format!("DiagnosticReport.contained/{i}.{}", issue.path);
                    }
                    c.issues.extend(inner_issues);
                }
            }
            c.optional_datetime(resource, "effectiveDateTime");
            c.optional_datetime(resource, "issued");
        }
        "DocumentReference" => {
            c.require_code(resource, "status", DOCUMENT_REFERENCE_STATUS);
            c.require_reference(resource, "subject", Some("Patient"));
            let content = c.require_array(resource, "content", 1);
            for (i, item) in content.iter().enumerate() {
                let att = item.get("attachment");
                if att
                    .and_then(|a| a.get("contentType"))
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    c.fail(
                        &format!("content/{i}/attachment/contentType"),
                        "required in this profile",
                    );
                }
                if att.and_then(|a| a.get("data")).is_some() {
                    c.fail(
                        &format!("content/{i}/attachment/data"),
                        "inline document bytes are never exchanged; use url",
                    );
                }
            }
            c.optional_datetime(resource, "date");
        }
        "ImagingStudy" => {
            c.require_code(resource, "status", IMAGING_STUDY_STATUS);
            c.require_reference(resource, "subject", Some("Patient"));
            let identifiers = c.require_array(resource, "identifier", 1);
            let study_uid = identifiers.iter().find_map(|i| {
                (i.get("system").and_then(Value::as_str) == Some("urn:dicom:uid"))
                    .then(|| i.get("value").and_then(Value::as_str))
                    .flatten()
            });
            match study_uid {
                Some(v) => {
                    if !v.strip_prefix("urn:oid:").is_some_and(dicom_uid_is_valid) {
                        c.fail(
                            "identifier",
                            "Study Instance UID must be urn:oid:<DICOM UID>",
                        );
                    }
                }
                None => c.fail(
                    "identifier",
                    "a urn:dicom:uid Study Instance UID is required",
                ),
            }
            for (i, s) in c.require_array(resource, "series", 0).iter().enumerate() {
                if !s
                    .get("uid")
                    .and_then(Value::as_str)
                    .is_some_and(dicom_uid_is_valid)
                {
                    c.fail(
                        &format!("series/{i}/uid"),
                        "a valid DICOM Series Instance UID is required",
                    );
                }
                if s.get("modality")
                    .and_then(|m| m.get("code"))
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    c.fail(
                        &format!("series/{i}/modality"),
                        "modality coding is required",
                    );
                }
            }
            if let Some(ep) = resource.get("endpoint") {
                if !ep.is_array() {
                    c.fail("endpoint", "must be an array of references");
                }
            }
            c.optional_datetime(resource, "started");
        }
        _ => unreachable!("resource type checked against SUPPORTED_RESOURCES"),
    }
    if c.issues.is_empty() {
        Ok(())
    } else {
        Err(c.issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_minimal_valid_resources() {
        let obs = json!({
            "resourceType": "Observation", "id": "o1", "status": "final",
            "code": { "coding": [{ "system": "http://loinc.org", "code": "2823-3" }] },
            "subject": { "reference": "Patient/p1" },
            "valueQuantity": { "value": 5.1, "unit": "mmol/L", "system": "http://unitsofmeasure.org", "code": "mmol/L" }
        });
        assert_eq!(validate(&obs, true), Ok(()));
        let study = json!({
            "resourceType": "ImagingStudy", "id": "s1", "status": "available",
            "subject": { "reference": "Patient/p1" },
            "identifier": [{ "system": "urn:dicom:uid", "value": "urn:oid:1.2.840.113619.2.55.3.1" }],
            "series": [{ "uid": "1.2.840.113619.2.55.3.1.1", "modality": { "system": "http://dicom.nema.org/resources/ontology/DCM", "code": "CR" } }]
        });
        assert_eq!(validate(&study, true), Ok(()));
    }

    #[test]
    fn rejects_unsupported_type_bad_codes_and_references() {
        assert!(validate(&json!({ "resourceType": "Bundle" }), false).is_err());
        let sr = json!({
            "resourceType": "ServiceRequest", "status": "done", "intent": "order",
            "code": { "coding": [{ "code": "x" }] },
            "subject": { "reference": "p1" }
        });
        let issues = validate(&sr, true).unwrap_err();
        let paths: Vec<_> = issues.iter().map(|i| i.path.as_str()).collect();
        assert!(paths.contains(&"ServiceRequest.id"));
        assert!(paths.contains(&"ServiceRequest.status"));
        assert!(paths.contains(&"ServiceRequest.code/coding/0/system"));
        assert!(paths.contains(&"ServiceRequest.subject/reference"));
    }

    #[test]
    fn observation_requires_exactly_one_value() {
        let base = json!({
            "resourceType": "Observation", "status": "final",
            "code": { "text": "note" }, "subject": { "reference": "Patient/p1" }
        });
        assert!(validate(&base, false).is_err());
        let mut two = base.clone();
        two["valueString"] = json!("a");
        two["valueBoolean"] = json!(true);
        assert!(validate(&two, false).is_err());
        let mut absent = base.clone();
        absent["dataAbsentReason"] = json!({ "coding": [{ "system": "http://terminology.hl7.org/CodeSystem/data-absent-reason", "code": "unknown" }] });
        assert_eq!(validate(&absent, false), Ok(()));
    }

    #[test]
    fn document_reference_refuses_inline_bytes() {
        let doc = json!({
            "resourceType": "DocumentReference", "status": "current",
            "subject": { "reference": "Patient/p1" },
            "content": [{ "attachment": { "contentType": "application/pdf", "data": "AAAA" } }]
        });
        let issues = validate(&doc, false).unwrap_err();
        assert!(issues.iter().any(|i| i.path.ends_with("attachment/data")));
    }

    #[test]
    fn dicom_uid_rules() {
        assert!(dicom_uid_is_valid("1.2.840.10008.5.1.4.1.1.2"));
        assert!(dicom_uid_is_valid("1.0.3"));
        assert!(!dicom_uid_is_valid("1.02.3"));
        assert!(!dicom_uid_is_valid("1..3"));
        assert!(!dicom_uid_is_valid(""));
        assert!(!dicom_uid_is_valid(&"1".repeat(65)));
    }
}
