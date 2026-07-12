// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Structural validation of the committed `openapi.json` (PR-04a).
//!
//! The renter API contract is hand-authored and committed at the repo root; this
//! module is the teeth behind `cargo xtask codegen --check`. It validates the spec
//! syntactically (it parses as JSON) and structurally (required objects present,
//! every internal `$ref` resolves, every golden-path route is covered, and the
//! RFC 9457 error taxonomy is complete). Full generated-vs-committed diffing is
//! deferred to PR-11, once real axum handlers exist to regenerate the spec from.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// The Phase-1 golden-path routes the contract must cover: auth, job
/// submit/get/list, log streaming, dataset push refs, and deploy. The mock server
/// (PR-04b) answers exactly this set, so the list is the single source of truth for
/// both the validator and the mock.
pub(crate) const GOLDEN_PATH_ROUTES: &[(&str, &str)] = &[
    ("GET", "/v1/account"),
    ("POST", "/v1/jobs"),
    ("GET", "/v1/jobs"),
    ("GET", "/v1/jobs/{id}"),
    ("GET", "/v1/jobs/{id}/logs"),
    ("POST", "/v1/jobs/{id}/cancel"),
    ("POST", "/v1/datasets"),
    ("POST", "/v1/datasets/{id}/versions"),
    ("POST", "/v1/datasets/{id}/versions/{vid}/commit"),
    ("POST", "/v1/deployments"),
    ("GET", "/v1/deployments/{id}"),
];

/// The closed RFC 9457 error-code taxonomy (renter-api.md §1.5). The committed
/// `ErrorCode` enum must contain at least these; it may only grow (additive-only).
const REQUIRED_ERROR_CODES: &[&str] = &[
    "unauthenticated",
    "insufficient_scope",
    "not_found",
    "idempotency_key_reused",
    "validation_failed",
    "manifest_invalid",
    "image_unknown",
    "recipe_unknown",
    "model_unavailable",
    "price_ceiling_unmet",
    "no_capacity",
    "quota_exceeded",
    "rate_limited",
    "insufficient_balance",
    "spend_cap_exceeded",
    "privacy_unavailable",
    "conflict",
    "internal",
];

/// HTTP method keys recognised inside a Path Item Object.
const HTTP_METHODS: &[&str] = &[
    "get", "put", "post", "delete", "patch", "options", "head", "trace",
];

/// A summary of a spec that passed validation, printed by the `codegen` verb.
#[derive(Debug)]
pub(crate) struct SpecReport {
    pub(crate) operations: usize,
    pub(crate) schemas: usize,
    pub(crate) error_codes: usize,
    pub(crate) golden_routes: usize,
}

/// Read, parse, and structurally validate the spec at `path`.
///
/// Returns a summary on success, or an `anyhow` error whose message enumerates
/// every structural problem found (so a maintainer sees all of them at once).
pub(crate) fn load_and_validate(path: &Path) -> Result<SpecReport> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let spec: Value = serde_json::from_str(&text)
        .with_context(|| format!("parsing {} as JSON", path.display()))?;
    match validate_spec(&spec) {
        Ok(report) => Ok(report),
        Err(problems) => {
            let mut msg = format!("{} failed OpenAPI validation:", path.display());
            for problem in &problems {
                msg.push_str("\n  - ");
                msg.push_str(problem);
            }
            bail!("{msg}");
        }
    }
}

/// Validate an already-parsed spec, collecting every problem rather than
/// failing on the first.
pub(crate) fn validate_spec(spec: &Value) -> std::result::Result<SpecReport, Vec<String>> {
    let mut problems = Vec::new();

    check_top_level(spec, &mut problems);
    let operations = check_paths(spec, &mut problems);
    let schemas = check_components(spec, &mut problems);
    let error_codes = check_error_taxonomy(spec, &mut problems);
    let golden_routes = check_golden_routes(spec, &mut problems);
    check_refs(spec, &mut problems);
    check_problem_media_type(spec, &mut problems);

    if problems.is_empty() {
        Ok(SpecReport {
            operations,
            schemas,
            error_codes,
            golden_routes,
        })
    } else {
        Err(problems)
    }
}

/// `openapi` version marker plus a well-formed `info` block.
fn check_top_level(spec: &Value, problems: &mut Vec<String>) {
    match spec.get("openapi").and_then(Value::as_str) {
        Some(version) if version.starts_with("3.") => {}
        Some(version) => problems.push(format!("`openapi` is `{version}`, expected a 3.x version")),
        None => problems.push("missing top-level `openapi` version string".to_owned()),
    }
    let Some(info) = spec.get("info").and_then(Value::as_object) else {
        problems.push("missing `info` object".to_owned());
        return;
    };
    for field in ["title", "version"] {
        match info.get(field).and_then(Value::as_str) {
            Some(value) if !value.is_empty() => {}
            _ => problems.push(format!("`info.{field}` must be a non-empty string")),
        }
    }
}

/// Every path is a valid template; every operation has a unique `operationId`
/// and at least one response, each response documented or a resolvable `$ref`.
fn check_paths(spec: &Value, problems: &mut Vec<String>) -> usize {
    let Some(paths) = spec.get("paths").and_then(Value::as_object) else {
        problems.push("missing `paths` object".to_owned());
        return 0;
    };
    if paths.is_empty() {
        problems.push("`paths` is empty".to_owned());
    }
    let mut operations = 0;
    let mut operation_ids = std::collections::BTreeSet::new();
    for (path, item) in paths {
        if !path.starts_with('/') {
            problems.push(format!("path `{path}` must start with `/`"));
        }
        let Some(item) = item.as_object() else {
            problems.push(format!("path item `{path}` is not an object"));
            continue;
        };
        for (method, op) in item
            .iter()
            .filter(|(k, _)| HTTP_METHODS.contains(&k.as_str()))
        {
            operations += 1;
            check_operation(path, method, op, &mut operation_ids, problems);
        }
    }
    operations
}

/// Validate one operation object.
fn check_operation(
    path: &str,
    method: &str,
    op: &Value,
    operation_ids: &mut std::collections::BTreeSet<String>,
    problems: &mut Vec<String>,
) {
    let where_ = format!("{} {path}", method.to_uppercase());
    match op.get("operationId").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => {
            if !operation_ids.insert(id.to_owned()) {
                problems.push(format!("duplicate operationId `{id}` at {where_}"));
            }
        }
        _ => problems.push(format!("{where_} is missing a non-empty `operationId`")),
    }
    let Some(responses) = op.get("responses").and_then(Value::as_object) else {
        problems.push(format!("{where_} is missing a `responses` object"));
        return;
    };
    if responses.is_empty() {
        problems.push(format!("{where_} has no responses"));
    }
    for (status, response) in responses {
        if status != "default" && !(status.len() == 3 && status.bytes().all(|b| b.is_ascii_digit()))
        {
            problems.push(format!("{where_} has an invalid response key `{status}`"));
        }
        // Inline responses (not a `$ref`) must carry a description per the spec;
        // `$ref` targets are checked for resolution separately.
        if response.get("$ref").is_none()
            && response
                .get("description")
                .and_then(Value::as_str)
                .is_none()
        {
            problems.push(format!(
                "{where_} response `{status}` needs a `description`"
            ));
        }
    }
}

/// `components.schemas` exists and carries the error model.
fn check_components(spec: &Value, problems: &mut Vec<String>) -> usize {
    let Some(schemas) = spec
        .pointer("/components/schemas")
        .and_then(Value::as_object)
    else {
        problems.push("missing `components.schemas`".to_owned());
        return 0;
    };
    for required in ["Problem", "ErrorCode"] {
        if !schemas.contains_key(required) {
            problems.push(format!("`components.schemas` is missing `{required}`"));
        }
    }
    if let Some(required) = spec
        .pointer("/components/schemas/Problem/required")
        .and_then(Value::as_array)
    {
        let present: std::collections::BTreeSet<&str> =
            required.iter().filter_map(Value::as_str).collect();
        for field in ["type", "title", "status", "code"] {
            if !present.contains(field) {
                problems.push(format!(
                    "`Problem` schema must require `{field}` (RFC 9457)"
                ));
            }
        }
    } else if schemas.contains_key("Problem") {
        problems.push("`Problem` schema must list `required` members".to_owned());
    }
    schemas.len()
}

/// The `ErrorCode` enum must cover the whole closed taxonomy.
fn check_error_taxonomy(spec: &Value, problems: &mut Vec<String>) -> usize {
    let Some(codes) = spec
        .pointer("/components/schemas/ErrorCode/enum")
        .and_then(Value::as_array)
    else {
        problems.push("`ErrorCode` schema must define a string `enum`".to_owned());
        return 0;
    };
    let present: std::collections::BTreeSet<&str> =
        codes.iter().filter_map(Value::as_str).collect();
    let string_count = codes.iter().filter(|v| v.is_string()).count();
    if string_count != codes.len() {
        problems.push("`ErrorCode.enum` must contain only strings".to_owned());
    }
    if present.len() != string_count {
        problems.push("`ErrorCode.enum` contains duplicate values".to_owned());
    }
    for code in REQUIRED_ERROR_CODES {
        if !present.contains(code) {
            problems.push(format!(
                "`ErrorCode.enum` is missing the taxonomy code `{code}`"
            ));
        }
    }
    present.len()
}

/// Every golden-path route is present in `paths`.
fn check_golden_routes(spec: &Value, problems: &mut Vec<String>) -> usize {
    let paths = spec.get("paths").and_then(Value::as_object);
    let mut covered = 0;
    for (method, path) in GOLDEN_PATH_ROUTES {
        let found = paths
            .and_then(|p| p.get(*path))
            .and_then(|item| item.get(method.to_lowercase()))
            .is_some();
        if found {
            covered += 1;
        } else {
            problems.push(format!(
                "golden-path route {method} {path} is not in the spec"
            ));
        }
    }
    covered
}

/// Every internal `$ref` resolves; external refs are rejected in the committed spec.
fn check_refs(spec: &Value, problems: &mut Vec<String>) {
    let mut refs = Vec::new();
    collect_refs(spec, &mut refs);
    for reference in refs {
        let Some(pointer) = reference.strip_prefix('#') else {
            problems.push(format!("external `$ref` `{reference}` is not allowed"));
            continue;
        };
        if spec.pointer(pointer).is_none() {
            problems.push(format!("dangling `$ref` `{reference}`"));
        }
    }
}

/// Collect every `$ref` string value anywhere in the document.
fn collect_refs<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref") {
                out.push(reference);
            }
            for child in map.values() {
                collect_refs(child, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_refs(child, out);
            }
        }
        _ => {}
    }
}

/// The RFC 9457 media type must be used by at least one error response.
fn check_problem_media_type(spec: &Value, problems: &mut Vec<String>) {
    if !contains_key(spec, "application/problem+json") {
        problems.push("no response uses the `application/problem+json` media type".to_owned());
    }
}

/// Whether `key` appears as an object key anywhere in the document.
fn contains_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key(key) || map.values().any(|child| contains_key(child, key))
        }
        Value::Array(items) => items.iter().any(|child| contains_key(child, key)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed spec, embedded so the test needs no working directory.
    const COMMITTED: &str = include_str!("../../openapi.json");

    fn committed() -> Value {
        serde_json::from_str(COMMITTED).expect("committed openapi.json is valid JSON")
    }

    #[test]
    fn committed_spec_validates() {
        let report = validate_spec(&committed()).expect("committed openapi.json validates");
        assert_eq!(report.golden_routes, GOLDEN_PATH_ROUTES.len());
        assert!(report.error_codes >= REQUIRED_ERROR_CODES.len());
        // The spec is additive-only after PR-04, so it may grow past the golden set;
        // require the golden routes to be covered, not that nothing else exists.
        assert!(report.operations >= GOLDEN_PATH_ROUTES.len());
    }

    #[test]
    fn taxonomy_is_complete() {
        let codes = committed();
        let enumerated = codes
            .pointer("/components/schemas/ErrorCode/enum")
            .and_then(Value::as_array)
            .expect("ErrorCode.enum present");
        for required in REQUIRED_ERROR_CODES {
            assert!(
                enumerated.iter().any(|c| c.as_str() == Some(*required)),
                "missing taxonomy code {required}"
            );
        }
    }

    #[test]
    fn dangling_ref_is_rejected() {
        let mut spec = committed();
        spec["paths"]["/v1/account"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
            ["$ref"] = Value::String("#/components/schemas/DoesNotExist".to_owned());
        let problems = validate_spec(&spec).expect_err("dangling ref must fail");
        assert!(problems.iter().any(|p| p.contains("DoesNotExist")));
    }

    #[test]
    fn missing_golden_route_is_rejected() {
        let mut spec = committed();
        spec["paths"]
            .as_object_mut()
            .expect("paths object")
            .remove("/v1/deployments");
        let problems = validate_spec(&spec).expect_err("missing route must fail");
        assert!(problems.iter().any(|p| p.contains("POST /v1/deployments")));
    }

    #[test]
    fn incomplete_taxonomy_is_rejected() {
        let mut spec = committed();
        let enumerated = spec["components"]["schemas"]["ErrorCode"]["enum"]
            .as_array_mut()
            .expect("enum array");
        enumerated.retain(|c| c.as_str() != Some("no_capacity"));
        let problems = validate_spec(&spec).expect_err("incomplete taxonomy must fail");
        assert!(problems.iter().any(|p| p.contains("no_capacity")));
    }
}
