mod ast_analyzer;
mod doc_comment;
mod macros;
mod method_checker;
mod signature_registry;
mod source_text;
mod static_checker;
pub mod std_methods;
mod types;

#[doc(hidden)]
pub use macros::__method_signature;

use std::collections::{HashMap, HashSet};

use rune::ast;

/// The whole standard library at once; for per-module tables see [`std_methods`].
pub use std_methods::rune_std_methods;
pub use types::{
	BuiltinSignature, CheckerError, Contract, ContractMismatch, DynamicReason, EnumVariant,
	Environment, LiteralValue, MethodRegistry, MethodSignature, MethodViolation, ParamDef,
	PrimitiveType, ReturnSite, ScriptValidationReport, SignatureOrigin, SignatureRegistry,
	StaticCheckResult, TypeDef, ValidationReport, Violation,
};

/// Main entry point — validates a script before saving (statically).
///
/// Verifies that the checked function (and, recursively, helpers reached via
/// `ResolvedCall`) honors its declared doc-comment contract, checks the
/// existence of called instance methods against the host [`Environment`],
/// and — when `expected` is given — compares the declared contract with the
/// signature the host expects of the function (mismatches end up in
/// `report.contract_mismatches`). The signature comparison also catches the
/// case where a typo (`@paran`) makes the script declare no parameters at
/// all — the contract itself is then consistent, but it diverges from the
/// expected signature.
///
/// Defaults for the simpler uses: `expected: None` skips the host-signature
/// comparison, `&Environment::default()` means no builtins and no method
/// table (method calls are then not checked).
pub fn validate_script(
	source: &str,
	function_name: &str,
	expected: Option<&Contract>,
	env: &Environment,
) -> Result<ScriptValidationReport, CheckerError> {
	let file = ast_analyzer::parse_file(source)?;

	let mut helpers: HashMap<String, ValidationReport> = HashMap::new();
	let mut visited: HashSet<String> = HashSet::new();

	let main = validate_function(&file, source, function_name, env, &mut helpers, &mut visited)?;

	let contract_mismatches = match expected {
		Some(expected) => compare_contracts(expected, &main.contract),
		None => Vec::new(),
	};
	let is_valid = main.is_valid
		&& helpers.values().all(|r| r.is_valid)
		&& contract_mismatches.is_empty();

	Ok(ScriptValidationReport {
		main,
		helpers,
		contract_mismatches,
		is_valid,
	})
}

fn compare_contracts(expected: &Contract, actual: &Contract) -> Vec<ContractMismatch> {
	let mut mismatches = Vec::new();

	if expected.params.len() != actual.params.len() {
		mismatches.push(ContractMismatch::ParamCount {
			expected: expected.params.len(),
			actual: actual.params.len(),
		});
	}
	// The script picks its own parameter names — only types are compared.
	for (index, (exp, act)) in expected.params.iter().zip(&actual.params).enumerate() {
		if !exp.type_def.is_compatible_with(&act.type_def) {
			mismatches.push(ContractMismatch::Param {
				index,
				expected: exp.clone(),
				actual: act.clone(),
			});
		}
	}
	if !expected.return_type.is_compatible_with(&actual.return_type) {
		mismatches.push(ContractMismatch::ReturnType {
			expected: expected.return_type.clone(),
			actual: actual.return_type.clone(),
		});
	}

	mismatches
}

fn validate_function(
	file: &ast::File,
	source: &str,
	function_name: &str,
	env: &Environment,
	helpers: &mut HashMap<String, ValidationReport>,
	visited: &mut HashSet<String>,
) -> Result<ValidationReport, CheckerError> {
	let item_fn = ast_analyzer::find_function(file, source, function_name)
		.ok_or_else(|| CheckerError::FunctionNotFound(function_name.to_string()))?;

	let doc =
		ast_analyzer::doc_comment_before(source, item_fn).ok_or(CheckerError::NoDocComment)?;
	let contract = doc_comment::parse(&doc)?;

	let registry = signature_registry::build(file, source, function_name, &env.builtins)?;

	let sites = ast_analyzer::find_return_sites(item_fn, source, &registry);
	let static_result = static_checker::check(&sites, &contract);
	let method_violations =
		method_checker::check(item_fn, source, &contract, &registry, &env.methods);
	let is_valid = static_result.violations.is_empty() && method_violations.is_empty();

	visited.insert(function_name.to_string());

	let mut helper_names = Vec::new();
	for site in &sites {
		collect_resolved_call_names(site, &mut helper_names);
	}

	for name in helper_names {
		if visited.contains(&name) || helpers.contains_key(&name) {
			continue;
		}

		let is_helper_origin = registry
			.signatures
			.get(&name)
			.is_some_and(SignatureOrigin::is_helper);

		if !is_helper_origin {
			// Builtin function (group 3) — has no body, cannot be verified.
			continue;
		}

		let helper_report = validate_function(file, source, &name, env, helpers, visited)?;
		helpers.insert(name, helper_report);
	}

	Ok(ValidationReport {
		function_name: function_name.to_string(),
		contract,
		static_result,
		method_violations,
		is_valid,
	})
}

fn collect_resolved_call_names(site: &ReturnSite, out: &mut Vec<String>) {
	match site {
		ReturnSite::ResolvedCall { name, .. } => out.push(name.clone()),
		ReturnSite::ObjectLiteral(fields) => {
			for (_, value) in fields {
				collect_resolved_call_names_literal(value, out);
			}
		}
		ReturnSite::EnumLiteral { inner, .. } => {
			if let Some(inner) = inner {
				collect_resolved_call_names_literal(inner, out);
			}
		}
		ReturnSite::PrimitiveLiteral(lv) => collect_resolved_call_names_literal(lv, out),
		ReturnSite::Unit | ReturnSite::Dynamic(_) | ReturnSite::TryPropagation { .. } => {}
	}
}

fn collect_resolved_call_names_literal(lv: &LiteralValue, out: &mut Vec<String>) {
	match lv {
		LiteralValue::ResolvedCall { name, .. } => out.push(name.clone()),
		LiteralValue::Object(fields) => {
			for (_, value) in fields {
				collect_resolved_call_names_literal(value, out);
			}
		}
		LiteralValue::List(items) => {
			for item in items {
				collect_resolved_call_names_literal(item, out);
			}
		}
		LiteralValue::Enum { inner, .. } => {
			if let Some(inner) = inner {
				collect_resolved_call_names_literal(inner, out);
			}
		}
		LiteralValue::String(_)
		| LiteralValue::Int(_)
		| LiteralValue::Float(_)
		| LiteralValue::Bool(_)
		| LiteralValue::Unit
		| LiteralValue::Dynamic(_) => {}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn honest_contract_is_valid() {
		let source = r#"
            /// @param name: String
            /// @return String
            fn process(name) {
                return "ok";
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(report.is_valid);
		assert!(report.main.static_result.violations.is_empty());
		assert!(report.helpers.is_empty());
	}

	#[test]
	fn pub_fn_doc_comment_is_found() {
		let source = r#"
            /// @param name: String
            /// @return String
            pub fn process(name) {
                return "ok";
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(report.is_valid);
	}

	#[test]
	fn block_doc_comment_contract_is_checked() {
		let source = r#"
            /**
             * @param name: String The user's name
             * @return String
             */
            pub fn process(name) {
                return "ok";
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(report.is_valid);
		assert_eq!(report.main.contract.params.len(), 1);
	}

	#[test]
	fn matching_expected_signature_is_valid() {
		let source = r#"
            /// @param sender: String
            /// @param event: String
            /// @return Status::Solved
            fn handler(sender, event) {
                Status::Solved
            }
        "#;
		let expected = contract!((sender: String, event: String) -> Status::Solved);
		let report = validate_script(source, "handler", Some(&expected), &Environment::default()).unwrap();
		assert!(report.is_valid);
		assert!(report.contract_mismatches.is_empty());
	}

	#[test]
	fn renamed_params_still_match_expected_signature() {
		// The script picks its own parameter names — only positional types matter.
		let source = r#"
            /// @param who: String
            /// @param what: String
            /// @return Status::Solved
            fn handler(who, what) {
                Status::Solved
            }
        "#;
		let expected = contract!((sender: String, event: String) -> Status::Solved);
		let report = validate_script(source, "handler", Some(&expected), &Environment::default()).unwrap();
		assert!(report.is_valid);
		assert!(report.contract_mismatches.is_empty());
	}

	#[test]
	fn param_tag_typo_is_caught_by_expected_signature() {
		// The parser ignores `@paran`; the contract itself is consistent —
		// only the comparison with the expected signature reveals the mismatch.
		let source = r#"
            /// @paran sender String
            /// @return Status::Solved
            fn handler(sender) {
                Status::Solved
            }
        "#;
		let expected = contract!((sender: String) -> Status::Solved);
		let report = validate_script(source, "handler", Some(&expected), &Environment::default()).unwrap();
		assert!(!report.is_valid);
		assert!(report.main.is_valid);
		assert_eq!(
			report.contract_mismatches,
			vec![ContractMismatch::ParamCount {
				expected: 1,
				actual: 0,
			}]
		);
	}

	#[test]
	fn wrong_param_and_return_types_are_reported() {
		let source = r#"
            /// @param sender: int
            /// @return String
            fn handler(sender) {
                return "ok";
            }
        "#;
		let expected = contract!((sender: String) -> Status::Solved);
		let report = validate_script(source, "handler", Some(&expected), &Environment::default()).unwrap();
		assert!(!report.is_valid);
		assert_eq!(report.contract_mismatches.len(), 2);
		assert!(matches!(
			report.contract_mismatches[0],
			ContractMismatch::Param { index: 0, .. }
		));
		assert!(matches!(
			report.contract_mismatches[1],
			ContractMismatch::ReturnType { .. }
		));
	}

	#[test]
	fn mismatch_messages_use_contract_syntax() {
		let source = r#"
            /// @param sender: Sender
            /// @param event: EventType
            /// @param context: EventType
            /// @return Status::Solved | Status::Continue | Status::Quit
            pub fn handler(sender, event, context) {
                Status::Solved
            }
        "#;
		let expected = contract!(
			(sender: Sender, event: EventType, context: AppContext | WindowContext | ComponentContext)
				-> Status::Solved | Status::Continue | Status::Quit
		);
		let report = validate_script(source, "handler", Some(&expected), &Environment::default()).unwrap();
		let messages: Vec<String> = report
			.contract_mismatches
			.iter()
			.map(|m| m.to_string())
			.collect();
		assert_eq!(
			messages,
			vec![
				"param 2 'context': expected `AppContext | WindowContext | ComponentContext`, \
				 script declares `EventType`"
					.to_string()
			]
		);
	}

	#[test]
	fn bare_enum_name_matches_enumerated_variants() {
		let source = r#"
            /// @param sender: Sender
            /// @param event: EventType
            /// @param context: AppContext | WindowContext | ComponentContext
            /// @return Status
            pub fn handler(sender, event, context) {
                Status::Solved
            }
        "#;
		let expected = contract!(
			(sender: Sender, event: EventType, context: AppContext | WindowContext | ComponentContext)
				-> Status::Solved | Status::Continue | Status::Quit
		);
		let report = validate_script(source, "handler", Some(&expected), &Environment::default()).unwrap();
		assert!(report.contract_mismatches.is_empty());
		assert!(report.is_valid);
	}

	#[test]
	fn enumerated_variants_match_bare_enum_name_in_expected() {
		let source = r#"
            /// @return Status::Solved | Status::Continue
            fn handler() {
                Status::Continue
            }
        "#;
		let expected = contract!(() -> Status);
		let report = validate_script(source, "handler", Some(&expected), &Environment::default()).unwrap();
		assert!(report.contract_mismatches.is_empty());
		assert!(report.is_valid);
	}

	#[test]
	fn bare_name_of_different_enum_does_not_match() {
		let source = r#"
            /// @return EventType
            fn handler() {
                EventType::Click
            }
        "#;
		let expected = contract!(() -> Status::Solved | Status::Continue);
		let report = validate_script(source, "handler", Some(&expected), &Environment::default()).unwrap();
		assert!(!report.is_valid);
		assert_eq!(report.contract_mismatches.len(), 1);
		assert!(matches!(
			report.contract_mismatches[0],
			ContractMismatch::ReturnType { .. }
		));
	}

	#[test]
	fn try_operator_propagates_helper_error_variant() {
		let source = r#"
            /// @param input: String
            /// @return Result::Ok(int) | Result::Err(String)
            fn process(input) {
                let value = parse(input)?;
                return Ok(value);
            }

            /// @param input: String
            /// @return Result::Ok(int) | Result::Err(String)
            fn parse(input) {
                if input == "" {
                    return Err("empty");
                }
                return Ok(42);
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(report.is_valid);
		// `parse` is a helper reached via `?` — recursively verified.
		assert!(report.helpers.contains_key("parse"));
	}

	#[test]
	fn try_operator_error_variant_not_in_contract_is_violation() {
		// The contract promises only Ok, but `?` may propagate Err from `parse`.
		let source = r#"
            /// @param input: String
            /// @return Result::Ok(int)
            fn process(input) {
                let value = parse(input)?;
                return Ok(value);
            }

            /// @param input: String
            /// @return Result::Ok(int) | Result::Err(String)
            fn parse(input) {
                return Ok(42);
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(!report.is_valid);
		assert_eq!(report.main.static_result.violations.len(), 1);
		assert!(
			report.main.static_result.violations[0]
				.actual
				.contains("Result::Err")
		);
	}

	#[test]
	fn try_on_unknown_type_without_error_variant_is_violation() {
		// `?` always propagates None or Err(...) — the `int` contract admits
		// neither, so it is a definite violation, not an unverifiable site.
		let source = r#"
            /// @param input: String
            /// @return int
            fn process(input) {
                let value = input.parse_int()?;
                return 1;
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(!report.is_valid);
		assert_eq!(report.main.static_result.violations.len(), 1);
		assert!(matches!(
			report.main.static_result.violations[0].site,
			ReturnSite::TryPropagation { line: 5 }
		));
	}

	#[test]
	fn try_on_unknown_type_with_error_variant_is_unverifiable() {
		// The contract accounts for Err — we don't know what `?` propagates,
		// but the error branch is admissible; it stays unverifiable.
		let source = r#"
            /// @param input: String
            /// @return Ok(int) | Err(String)
            fn process(input) {
                let value = input.parse_int()?;
                return Ok(1);
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(report.is_valid);
		assert!(
			report
				.main
				.static_result
				.unverifiable
				.iter()
				.any(|s| matches!(s, ReturnSite::TryPropagation { line: 5 }))
		);
	}

	#[test]
	fn try_with_bare_result_contract_is_unverifiable() {
		// The bare enum name `Result` admits Err too — `?` is admissible.
		let source = r#"
            /// @param input: String
            /// @return Result
            fn process(input) {
                let value = input.parse_int()?;
                return Ok(value);
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(report.is_valid);
		assert!(
			report
				.main
				.static_result
				.unverifiable
				.iter()
				.any(|s| matches!(s, ReturnSite::TryPropagation { .. }))
		);
	}

	#[test]
	fn try_with_nullable_error_contract_is_unverifiable() {
		// The Nullable wrapper (`| ()`) must not mask an admitted Err variant.
		let source = r#"
            /// @param input: String
            /// @return Result::Ok(int) | Result::Err(String) | ()
            fn process(input) {
                let value = input.parse_int()?;
                return Ok(42);
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(report.is_valid);
		assert!(
			report
				.main
				.static_result
				.unverifiable
				.iter()
				.any(|s| matches!(s, ReturnSite::TryPropagation { .. }))
		);
	}

	#[test]
	fn try_propagating_none_literal_is_verified() {
		// `None?` propagates a literal — against a contract with Option::None
		// it passes as a statically verified site, not merely unverifiable.
		let source = r#"
            /// @param input: String
            /// @return Option::Some(int) | Option::None
            fn process(input) {
                None?;
                return Some(1);
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(report.is_valid);
		assert!(report.main.static_result.unverifiable.is_empty());
	}

	#[test]
	fn try_propagating_err_literal_not_in_contract_is_violation() {
		let source = r#"
            /// @param input: String
            /// @return Result::Ok(int)
            fn process(input) {
                Err("boom")?;
                return Ok(1);
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(!report.is_valid);
		assert_eq!(report.main.static_result.violations.len(), 1);
		assert!(
			report.main.static_result.violations[0]
				.actual
				.contains("Result::Err")
		);
	}

	#[test]
	fn try_value_unwraps_ok_inner_type() {
		let source = r#"
            /// @param input: String
            /// @return Result::Ok(int) | Result::Err(String)
            fn process(input) {
                Ok(parse(input)?)
            }

            /// @param input: String
            /// @return Result::Ok(int) | Result::Err(String)
            fn parse(input) {
                return Ok(42);
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(report.is_valid);
		// Both the tail `Ok(int)` and the propagated `Err(String)` are statically verified.
		assert!(report.main.static_result.unverifiable.is_empty());
		assert_eq!(report.main.static_result.verified.len(), 2);
	}

	#[test]
	fn methods_macro_builds_signatures() {
		let table = methods![
			Sender::name() -> String;
			Sender::fullname();
			AppContext::lookup() -> Option::Some(ComponentContext) | Option::None;
			Commands::mod_entries() -> [ModEntry];
			Stats::snapshot() -> { count: int, label: String };
		];
		assert_eq!(table.len(), 5);
		assert_eq!(table[0].receiver, "Sender");
		assert_eq!(table[0].name, "name");
		assert_eq!(
			table[0].return_type,
			Some(TypeDef::Primitive(PrimitiveType::String))
		);
		assert_eq!(table[1].return_type, None);
		assert_eq!(
			table[2].return_type,
			Some(TypeDef::parse("Option::Some(ComponentContext) | Option::None").unwrap())
		);
		assert_eq!(
			table[3].return_type,
			Some(TypeDef::parse("[ModEntry]").unwrap())
		);
		assert_eq!(
			table[4].return_type,
			Some(TypeDef::parse("{ count: int, label: String }").unwrap())
		);
	}

	#[test]
	fn methods_macro_without_trailing_semicolon_and_empty() {
		let table = methods![Sender::name() -> String];
		assert_eq!(table.len(), 1);
		let empty: Vec<MethodSignature> = methods![];
		assert!(empty.is_empty());
	}

	#[test]
	fn contract_macro_builds_expected_types() {
		let c = contract!((items: [int], meta: { id: int, name: String }) -> String | ());
		assert_eq!(c.params.len(), 2);
		assert_eq!(
			c.params[0].type_def,
			TypeDef::List(Box::new(TypeDef::Primitive(PrimitiveType::Int)))
		);
		assert_eq!(
			c.params[1].type_def,
			TypeDef::Object(vec![
				("id".to_string(), TypeDef::Primitive(PrimitiveType::Int)),
				(
					"name".to_string(),
					TypeDef::Primitive(PrimitiveType::String)
				),
			])
		);
		assert_eq!(
			c.return_type,
			TypeDef::Nullable(Box::new(TypeDef::Primitive(PrimitiveType::String)))
		);
	}

	#[test]
	fn broken_contract_is_caught() {
		// Exactly the README problem: the user promises String, returns int.
		let source = r#"
            /// @return String
            fn process(input) {
                return 42;
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(!report.is_valid);
		assert_eq!(report.main.static_result.violations.len(), 1);
		assert_eq!(
			report.main.static_result.violations[0].actual,
			"expected String, got int"
		);
	}

	#[test]
	fn object_shape_end_to_end() {
		let source = r#"
            /// @return { status: String, code: int, active: bool }
            fn process(input) {
                return #{ status: "ok", code: 42, active: true };
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(report.is_valid);
	}

	#[test]
	fn helper_is_recursively_verified_and_passes() {
		let source = r#"
            /// @return int
            fn helper() {
                return 42;
            }

            /// @return int
            fn process() {
                return helper();
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(report.is_valid);
		assert!(report.helpers.contains_key("helper"));
		assert!(report.helpers["helper"].is_valid);
	}

	#[test]
	fn helper_violating_its_own_contract_fails_whole_script() {
		let source = r#"
            /// @return int
            fn helper() {
                return "not an int";
            }

            /// @return int
            fn process() {
                return helper();
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		// The caller expects int and the helper declares int, so the
		// ResolvedCall itself is verified at the `process` level...
		assert!(report.main.is_valid);
		// ...but the helper breaks its own contract, so the whole script fails.
		assert!(!report.helpers["helper"].is_valid);
		assert!(!report.is_valid);
	}

	#[test]
	fn builtin_is_trusted_and_not_recursively_verified() {
		let source = r#"
            /// @return String
            fn process() {
                return http::get("https://example.com");
            }
        "#;
		let env = Environment {
			builtins: vec![BuiltinSignature {
				name: "http::get".to_string(),
				return_type: TypeDef::Primitive(PrimitiveType::String),
			}],
			..Environment::default()
		};
		let report = validate_script(source, "process", None, &env).unwrap();
		assert!(report.is_valid);
		assert!(report.helpers.is_empty());
	}

	#[test]
	fn function_not_found_is_error() {
		let source = "fn other() { return 1; }";
		let result = validate_script(source, "process", None, &Environment::default());
		assert_eq!(
			result.unwrap_err(),
			CheckerError::FunctionNotFound("process".to_string())
		);
	}

	#[test]
	fn missing_doc_comment_is_error() {
		let source = "fn process() { return 1; }";
		let result = validate_script(source, "process", None, &Environment::default());
		assert_eq!(result.unwrap_err(), CheckerError::NoDocComment);
	}

	#[test]
	fn dynamic_return_does_not_block_validity() {
		let source = r#"
            /// @return String
            fn process(input) {
                return compute(input);
            }
        "#;
		let report = validate_script(source, "process", None, &Environment::default()).unwrap();
		assert!(report.is_valid);
		assert_eq!(report.main.static_result.unverifiable.len(), 1);
	}
}
