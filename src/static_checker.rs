use crate::types::{
	Contract, DynamicReason, EnumVariant, LiteralValue, PrimitiveType, ReturnSite,
	StaticCheckResult, TypeDef, Violation,
};

pub fn check(sites: &[ReturnSite], contract: &Contract) -> StaticCheckResult {
	let mut result = StaticCheckResult::default();

	for site in sites {
		// `?` on an unknown type — we don't know what it propagates, but we
		// know it is always None or Err(...). If the contract admits neither,
		// it is a definite violation; otherwise it is statically unverifiable.
		if let ReturnSite::TryPropagation { line } = site {
			if admits_try_propagation(&contract.return_type) {
				result.unverifiable.push(site.clone());
			} else {
				result.violations.push(Violation {
					site: site.clone(),
					expected: contract.return_type.clone(),
					actual: format!(
						"the `?` operator (line {line}) may exit the function early with a None/Err value the contract does not admit"
					),
				});
			}
			continue;
		}

		if matches!(site, ReturnSite::Dynamic(_)) {
			result.unverifiable.push(site.clone());
			continue;
		}

		let actual = site_as_literal(site);
		match compare(&contract.return_type, &actual) {
			Outcome::Verified => result.verified.push(site.clone()),
			Outcome::Unverifiable => result.unverifiable.push(site.clone()),
			Outcome::Violation(actual_desc) => result.violations.push(Violation {
				site: site.clone(),
				expected: contract.return_type.clone(),
				actual: actual_desc,
			}),
		}
	}

	result
}

/// `?` can only leave the function with None or Err(...) — the contract
/// admits that if it declares such a variant (or a whole Option/Result).
fn admits_try_propagation(expected: &TypeDef) -> bool {
	match expected {
		TypeDef::Enum(variants) => variants.iter().any(|v| {
			if v.is_bare_enum_name() {
				matches!(v.path[0].as_str(), "Result" | "Option")
			} else {
				matches!(
					v.path.last().map(String::as_str),
					Some("Err") | Some("None")
				)
			}
		}),
		TypeDef::Nullable(inner) => admits_try_propagation(inner),
		_ => false,
	}
}

fn site_as_literal(site: &ReturnSite) -> LiteralValue {
	match site.clone() {
		ReturnSite::ObjectLiteral(fields) => LiteralValue::Object(fields),
		ReturnSite::PrimitiveLiteral(lv) => lv,
		ReturnSite::EnumLiteral { path, inner } => LiteralValue::Enum { path, inner },
		ReturnSite::Unit => LiteralValue::Unit,
		ReturnSite::ResolvedCall { name, type_def } => {
			LiteralValue::ResolvedCall { name, type_def }
		}
		ReturnSite::Dynamic(reason) => LiteralValue::Dynamic(reason),
		// Already handled in `check` (unverifiable/violation) — never reached.
		ReturnSite::TryPropagation { .. } => LiteralValue::Dynamic(DynamicReason::TryPropagation),
	}
}

enum Outcome {
	Verified,
	Violation(String),
	Unverifiable,
}

fn compare(expected: &TypeDef, actual: &LiteralValue) -> Outcome {
	if let LiteralValue::Dynamic(_) = actual {
		return Outcome::Unverifiable;
	}

	if let LiteralValue::ResolvedCall { name, type_def } = actual {
		return match compare_types(expected, type_def) {
			TypeOutcome::Verified => Outcome::Verified,
			TypeOutcome::Violation(desc) => Outcome::Violation(format!("Function '{name}' {desc}")),
		};
	}

	match (expected, actual) {
		(TypeDef::Primitive(p), lv) => compare_primitive(*p, lv),
		(TypeDef::Unit, LiteralValue::Unit) => Outcome::Verified,
		(TypeDef::Unit, lv) => Outcome::Violation(format!("expected (), got {}", describe(lv))),
		(TypeDef::Nullable(_), LiteralValue::Unit) => Outcome::Verified,
		(TypeDef::Nullable(inner), lv) => compare(inner, lv),
		(TypeDef::List(inner), LiteralValue::List(items)) => compare_list(inner, items),
		(TypeDef::List(_), lv) => {
			Outcome::Violation(format!("expected a list, got {}", describe(lv)))
		}
		(TypeDef::Object(fields), LiteralValue::Object(actual_fields)) => {
			compare_object(fields, actual_fields)
		}
		(TypeDef::Object(_), lv) => {
			Outcome::Violation(format!("expected an object, got {}", describe(lv)))
		}
		(TypeDef::Enum(variants), LiteralValue::Enum { path, inner }) => {
			compare_enum(variants, path, inner.as_deref())
		}
		(TypeDef::Enum(_), lv) => {
			Outcome::Violation(format!("expected an enum variant, got {}", describe(lv)))
		}
	}
}

fn compare_primitive(expected: PrimitiveType, actual: &LiteralValue) -> Outcome {
	let ok = match (expected, actual) {
		(PrimitiveType::String, LiteralValue::String(_)) => true,
		(PrimitiveType::Int, LiteralValue::Int(_)) => true,
		(PrimitiveType::Float, LiteralValue::Float(_)) => true,
		(PrimitiveType::Bool, LiteralValue::Bool(_)) => true,
		_ => false,
	};

	if ok {
		Outcome::Verified
	} else {
		Outcome::Violation(format!("expected {expected}, got {}", describe(actual)))
	}
}

fn compare_list(inner: &TypeDef, items: &[LiteralValue]) -> Outcome {
	let mut unverifiable = false;

	for item in items {
		match compare(inner, item) {
			Outcome::Verified => {}
			Outcome::Unverifiable => unverifiable = true,
			Outcome::Violation(desc) => return Outcome::Violation(format!("list item {desc}")),
		}
	}

	if unverifiable {
		Outcome::Unverifiable
	} else {
		Outcome::Verified
	}
}

fn compare_object(
	expected_fields: &[(String, TypeDef)],
	actual_fields: &[(String, LiteralValue)],
) -> Outcome {
	let mut unverifiable = false;

	for (name, expected_type) in expected_fields {
		let Some((_, value)) = actual_fields.iter().find(|(n, _)| n == name) else {
			return Outcome::Violation(format!(
				"missing field '{name}' (expected {expected_type})"
			));
		};

		match compare(expected_type, value) {
			Outcome::Verified => {}
			Outcome::Unverifiable => unverifiable = true,
			Outcome::Violation(desc) => {
				return Outcome::Violation(format!("field '{name}': {desc}"));
			}
		}
	}

	if unverifiable {
		Outcome::Unverifiable
	} else {
		Outcome::Verified
	}
}

fn compare_enum(
	variants: &[EnumVariant],
	path: &[String],
	inner: Option<&LiteralValue>,
) -> Outcome {
	let Some(variant) = variants.iter().find(|v| v.accepts_path(path)) else {
		return Outcome::Violation(format!("unexpected variant '{}'", path.join("::")));
	};

	if variant.is_bare_enum_name() {
		// A bare enum name says nothing about the variants' inner values —
		// any is accepted.
		return Outcome::Verified;
	}

	match (&variant.inner, inner) {
		(None, None) => Outcome::Verified,
		(Some(expected_inner), Some(actual_inner)) => compare(expected_inner, actual_inner),
		_ => Outcome::Violation(format!(
			"variant '{}' has mismatched arity",
			path.join("::")
		)),
	}
}

enum TypeOutcome {
	Verified,
	Violation(String),
}

/// Structural comparison of two declared types (for `ResolvedCall` — the type
/// of the called helper/builtin against the type the caller expects).
fn compare_types(expected: &TypeDef, actual: &TypeDef) -> TypeOutcome {
	match (expected, actual) {
		(TypeDef::Primitive(e), TypeDef::Primitive(a)) if e == a => TypeOutcome::Verified,
		(TypeDef::Unit, TypeDef::Unit) => TypeOutcome::Verified,
		(TypeDef::Nullable(_), TypeDef::Unit) => TypeOutcome::Verified,
		(TypeDef::Nullable(e), TypeDef::Nullable(a)) => compare_types(e, a),
		(TypeDef::Nullable(e), a) => compare_types(e, a),
		(e, TypeDef::Nullable(a)) => compare_types(e, a),
		(TypeDef::List(e), TypeDef::List(a)) => compare_types(e, a),
		(TypeDef::Object(expected_fields), TypeDef::Object(actual_fields)) => {
			for (name, expected_type) in expected_fields {
				let Some((_, actual_type)) = actual_fields.iter().find(|(n, _)| n == name) else {
					return TypeOutcome::Violation(format!(
						"returns an object missing field '{name}' (expected {expected_type})"
					));
				};
				if let TypeOutcome::Violation(desc) = compare_types(expected_type, actual_type) {
					return TypeOutcome::Violation(format!("field '{name}': {desc}"));
				}
			}
			TypeOutcome::Verified
		}
		(TypeDef::Enum(expected_variants), TypeDef::Enum(actual_variants)) => {
			for actual_variant in actual_variants {
				let Some(expected_variant) = expected_variants
					.iter()
					.find(|v| v.matches_name(actual_variant))
				else {
					return TypeOutcome::Violation(format!(
						"may return unexpected variant '{}'",
						actual_variant.path.join("::")
					));
				};
				if expected_variant.is_bare_enum_name() || actual_variant.is_bare_enum_name() {
					// A bare enum name says nothing about the inner value.
					continue;
				}
				match (&expected_variant.inner, &actual_variant.inner) {
					(None, None) => {}
					(Some(e), Some(a)) => {
						if let TypeOutcome::Violation(desc) = compare_types(e, a) {
							return TypeOutcome::Violation(format!(
								"variant '{}': {desc}",
								actual_variant.path.join("::")
							));
						}
					}
					_ => {
						return TypeOutcome::Violation(format!(
							"variant '{}' has mismatched arity",
							actual_variant.path.join("::")
						));
					}
				}
			}
			TypeOutcome::Verified
		}
		(e, a) => TypeOutcome::Violation(format!("returns {a}, expected {e}")),
	}
}

fn describe(lv: &LiteralValue) -> String {
	match lv {
		LiteralValue::String(_) => "String".to_string(),
		LiteralValue::Int(_) => "int".to_string(),
		LiteralValue::Float(_) => "float".to_string(),
		LiteralValue::Bool(_) => "bool".to_string(),
		LiteralValue::Object(_) => "object".to_string(),
		LiteralValue::Enum { path, .. } => format!("enum variant '{}'", path.join("::")),
		LiteralValue::List(_) => "list".to_string(),
		LiteralValue::Unit => "()".to_string(),
		LiteralValue::ResolvedCall { name, .. } => format!("result of '{name}'"),
		LiteralValue::Dynamic(_) => "dynamic value".to_string(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::types::DynamicReason;

	fn contract(return_type: TypeDef) -> Contract {
		Contract {
			params: Vec::new(),
			return_type,
		}
	}

	#[test]
	fn primitive_match_is_verified() {
		let result = check(
			&[ReturnSite::PrimitiveLiteral(LiteralValue::String(
				"ok".into(),
			))],
			&contract(TypeDef::Primitive(PrimitiveType::String)),
		);
		assert_eq!(result.verified.len(), 1);
		assert!(result.violations.is_empty());
		assert!(result.unverifiable.is_empty());
	}

	#[test]
	fn primitive_mismatch_is_violation() {
		let result = check(
			&[ReturnSite::PrimitiveLiteral(LiteralValue::Int(42))],
			&contract(TypeDef::Primitive(PrimitiveType::String)),
		);
		assert_eq!(result.violations.len(), 1);
		assert_eq!(result.violations[0].actual, "expected String, got int");
	}

	#[test]
	fn dynamic_is_unverifiable() {
		let result = check(
			&[ReturnSite::Dynamic(DynamicReason::Variable("x".into()))],
			&contract(TypeDef::Primitive(PrimitiveType::String)),
		);
		assert_eq!(result.unverifiable.len(), 1);
	}

	#[test]
	fn object_extra_fields_allowed() {
		let site = ReturnSite::ObjectLiteral(vec![
			("status".into(), LiteralValue::String("ok".into())),
			("extra".into(), LiteralValue::Bool(true)),
		]);
		let expected = contract(TypeDef::Object(vec![(
			"status".into(),
			TypeDef::Primitive(PrimitiveType::String),
		)]));
		let result = check(&[site], &expected);
		assert_eq!(result.verified.len(), 1);
	}

	#[test]
	fn object_missing_field_is_violation() {
		let site = ReturnSite::ObjectLiteral(vec![("code".into(), LiteralValue::Int(1))]);
		let expected = contract(TypeDef::Object(vec![(
			"status".into(),
			TypeDef::Primitive(PrimitiveType::String),
		)]));
		let result = check(&[site], &expected);
		assert_eq!(result.violations.len(), 1);
		assert!(
			result.violations[0]
				.actual
				.contains("missing field 'status'")
		);
	}

	#[test]
	fn object_with_dynamic_field_is_unverifiable_not_violation() {
		let site = ReturnSite::ObjectLiteral(vec![(
			"status".into(),
			LiteralValue::Dynamic(DynamicReason::UnannotatedCall("compute".into())),
		)]);
		let expected = contract(TypeDef::Object(vec![(
			"status".into(),
			TypeDef::Primitive(PrimitiveType::String),
		)]));
		let result = check(&[site], &expected);
		assert_eq!(result.unverifiable.len(), 1);
		assert!(result.violations.is_empty());
	}

	#[test]
	fn object_with_dynamic_field_and_wrong_static_field_is_violation() {
		let site = ReturnSite::ObjectLiteral(vec![
			("status".into(), LiteralValue::Int(1)),
			(
				"extra".into(),
				LiteralValue::Dynamic(DynamicReason::Expression),
			),
		]);
		let expected = contract(TypeDef::Object(vec![(
			"status".into(),
			TypeDef::Primitive(PrimitiveType::String),
		)]));
		let result = check(&[site], &expected);
		assert_eq!(result.violations.len(), 1);
	}

	#[test]
	fn enum_variant_matches_by_last_segment() {
		let site = ReturnSite::EnumLiteral {
			path: vec!["Result".into(), "Ok".into()],
			inner: Some(Box::new(LiteralValue::Int(42))),
		};
		let expected = contract(TypeDef::Enum(vec![
			EnumVariant {
				path: vec!["Result".into(), "Ok".into()],
				inner: Some(Box::new(TypeDef::Primitive(PrimitiveType::Int))),
			},
			EnumVariant {
				path: vec!["Result".into(), "Err".into()],
				inner: Some(Box::new(TypeDef::Primitive(PrimitiveType::String))),
			},
		]));
		let result = check(&[site], &expected);
		assert_eq!(result.verified.len(), 1);
	}

	#[test]
	fn enum_variant_not_allowed_is_violation() {
		let site = ReturnSite::EnumLiteral {
			path: vec!["Foo".into()],
			inner: None,
		};
		let expected = contract(TypeDef::Enum(vec![EnumVariant {
			path: vec!["Result".into(), "Ok".into()],
			inner: Some(Box::new(TypeDef::Primitive(PrimitiveType::Int))),
		}]));
		let result = check(&[site], &expected);
		assert_eq!(result.violations.len(), 1);
	}

	#[test]
	fn bare_enum_name_accepts_any_variant() {
		// `@return Status` — with no enumerated variants, any variant of the
		// Status enum passes, inner value included.
		let bare = contract(TypeDef::Enum(vec![EnumVariant {
			path: vec!["Status".into()],
			inner: None,
		}]));

		let plain = ReturnSite::EnumLiteral {
			path: vec!["Status".into(), "Solved".into()],
			inner: None,
		};
		let with_inner = ReturnSite::EnumLiteral {
			path: vec!["Status".into(), "Failed".into()],
			inner: Some(Box::new(LiteralValue::String("why".into()))),
		};
		let result = check(&[plain, with_inner], &bare);
		assert_eq!(result.verified.len(), 2);
		assert!(result.violations.is_empty());
	}

	#[test]
	fn bare_enum_name_rejects_other_enum() {
		let bare = contract(TypeDef::Enum(vec![EnumVariant {
			path: vec!["Status".into()],
			inner: None,
		}]));
		let site = ReturnSite::EnumLiteral {
			path: vec!["EventType".into(), "Click".into()],
			inner: None,
		};
		let result = check(&[site], &bare);
		assert_eq!(result.violations.len(), 1);
	}

	#[test]
	fn list_items_checked_recursively() {
		let site = ReturnSite::PrimitiveLiteral(LiteralValue::List(vec![
			LiteralValue::String("a".into()),
			LiteralValue::String("b".into()),
		]));
		let expected = contract(TypeDef::List(Box::new(TypeDef::Primitive(
			PrimitiveType::String,
		))));
		let result = check(&[site], &expected);
		assert_eq!(result.verified.len(), 1);
	}

	#[test]
	fn empty_list_is_always_valid() {
		let site = ReturnSite::PrimitiveLiteral(LiteralValue::List(vec![]));
		let expected = contract(TypeDef::List(Box::new(TypeDef::Primitive(
			PrimitiveType::Int,
		))));
		let result = check(&[site], &expected);
		assert_eq!(result.verified.len(), 1);
	}

	#[test]
	fn nullable_accepts_unit_and_inner_type() {
		let expected = contract(TypeDef::Nullable(Box::new(TypeDef::Primitive(
			PrimitiveType::String,
		))));
		let result = check(
			&[
				ReturnSite::Unit,
				ReturnSite::PrimitiveLiteral(LiteralValue::String("x".into())),
			],
			&expected,
		);
		assert_eq!(result.verified.len(), 2);
	}

	#[test]
	fn resolved_call_matching_type_is_verified() {
		let site = ReturnSite::ResolvedCall {
			name: "helper".into(),
			type_def: TypeDef::Primitive(PrimitiveType::String),
		};
		let result = check(
			&[site],
			&contract(TypeDef::Primitive(PrimitiveType::String)),
		);
		assert_eq!(result.verified.len(), 1);
	}

	#[test]
	fn resolved_call_mismatched_type_is_violation() {
		let site = ReturnSite::ResolvedCall {
			name: "helper".into(),
			type_def: TypeDef::Primitive(PrimitiveType::Int),
		};
		let result = check(
			&[site],
			&contract(TypeDef::Primitive(PrimitiveType::String)),
		);
		assert_eq!(result.violations.len(), 1);
		assert!(result.violations[0].actual.contains("helper"));
	}
}
