mod ast_analyzer;
mod doc_comment;
mod signature_registry;
mod source_text;
mod static_checker;
mod types;

use std::collections::{HashMap, HashSet};

use rune::ast;

pub use types::{
	BuiltinSignature, CheckerError, Contract, ContractMismatch, DynamicReason, EnumVariant,
	LiteralValue, ParamDef, PrimitiveType, ReturnSite, ScriptValidationReport, SignatureOrigin,
	SignatureRegistry, StaticCheckResult, TypeDef, ValidationReport, Violation,
};

/// Sestaví [`Contract`] ze stručného zápisu signatury. Syntaxe typů je stejná
/// jako v doc-comment kontraktech (`@param`/`@return`).
///
/// ```
/// use rune_typechecker::contract;
///
/// let expected = contract!((sender: String, event: String) -> Status::Solved);
/// assert_eq!(expected.params.len(), 2);
/// ```
///
/// Při neplatné syntaxi panikaří — je určené pro signatury zapsané napevno
/// v kódu hosta. Pro signatury přicházející za běhu (např. z konfigurace)
/// použijte [`Contract::parse`], které vrací `Result`.
#[macro_export]
macro_rules! contract {
	($($signature:tt)+) => {
		$crate::Contract::parse(stringify!($($signature)+))
			.unwrap_or_else(|e| panic!("invalid contract signature: {e}"))
	};
}

/// Hlavní entry point — validuje skript před uložením (staticky), vč.
/// rekurzivní verifikace pomocných funkcí dosažených přes `ResolvedCall`.
pub fn validate_script(
	source: &str,
	function_name: &str,
	builtins: &[BuiltinSignature],
) -> Result<ScriptValidationReport, CheckerError> {
	let file = ast_analyzer::parse_file(source)?;

	let mut helpers: HashMap<String, ValidationReport> = HashMap::new();
	let mut visited: HashSet<String> = HashSet::new();

	let main = validate_function(
		&file,
		source,
		function_name,
		builtins,
		&mut helpers,
		&mut visited,
	)?;

	let is_valid = main.is_valid && helpers.values().all(|r| r.is_valid);

	Ok(ScriptValidationReport {
		main,
		helpers,
		contract_mismatches: Vec::new(),
		is_valid,
	})
}

/// Jako [`validate_script`], ale navíc ověří, že kontrakt deklarovaný skriptem
/// odpovídá signatuře, kterou od funkce očekává hostitelský systém. Neshody
/// jsou v `report.contract_mismatches` a shazují `report.is_valid`.
///
/// Tím se odchytí i případ, kdy skript kvůli překlepu (`@paran`) nedeklaruje
/// parametry vůbec — samotný kontrakt je pak konzistentní a `validate_script`
/// ho pustí, ale s očekávanou signaturou se rozejde.
pub fn validate_script_against(
	source: &str,
	function_name: &str,
	expected: &Contract,
	builtins: &[BuiltinSignature],
) -> Result<ScriptValidationReport, CheckerError> {
	let mut report = validate_script(source, function_name, builtins)?;
	report.contract_mismatches = compare_contracts(expected, &report.main.contract);
	report.is_valid = report.is_valid && report.contract_mismatches.is_empty();
	Ok(report)
}

fn compare_contracts(expected: &Contract, actual: &Contract) -> Vec<ContractMismatch> {
	let mut mismatches = Vec::new();

	if expected.params.len() != actual.params.len() {
		mismatches.push(ContractMismatch::ParamCount {
			expected: expected.params.len(),
			actual: actual.params.len(),
		});
	}
	// Jména parametrů si skript volí sám — porovnávají se jen typy.
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
	builtins: &[BuiltinSignature],
	helpers: &mut HashMap<String, ValidationReport>,
	visited: &mut HashSet<String>,
) -> Result<ValidationReport, CheckerError> {
	let item_fn = ast_analyzer::find_function(file, source, function_name)
		.ok_or_else(|| CheckerError::FunctionNotFound(function_name.to_string()))?;

	let doc =
		ast_analyzer::doc_comment_before(source, item_fn).ok_or(CheckerError::NoDocComment)?;
	let contract = doc_comment::parse(&doc)?;

	let registry = signature_registry::build(file, source, function_name, builtins)?;

	let sites = ast_analyzer::find_return_sites(item_fn, source, &registry);
	let static_result = static_checker::check(&sites, &contract);
	let is_valid = static_result.violations.is_empty();

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
			// Vestavěná funkce (skupina 3) — bez těla, nelze ověřit.
			continue;
		}

		let helper_report = validate_function(file, source, &name, builtins, helpers, visited)?;
		helpers.insert(name, helper_report);
	}

	Ok(ValidationReport {
		function_name: function_name.to_string(),
		contract,
		static_result,
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
		ReturnSite::Unit | ReturnSite::Dynamic(_) => {}
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
		let report = validate_script(source, "process", &[]).unwrap();
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
		let report = validate_script(source, "process", &[]).unwrap();
		assert!(report.is_valid);
	}

	#[test]
	fn block_doc_comment_contract_is_checked() {
		let source = r#"
            /**
             * @param name: String Jméno uživatele
             * @return String
             */
            pub fn process(name) {
                return "ok";
            }
        "#;
		let report = validate_script(source, "process", &[]).unwrap();
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
		let report = validate_script_against(source, "handler", &expected, &[]).unwrap();
		assert!(report.is_valid);
		assert!(report.contract_mismatches.is_empty());
	}

	#[test]
	fn renamed_params_still_match_expected_signature() {
		// Jména parametrů si skript volí sám — rozhodují jen typy na pozicích.
		let source = r#"
            /// @param who: String
            /// @param what: String
            /// @return Status::Solved
            fn handler(who, what) {
                Status::Solved
            }
        "#;
		let expected = contract!((sender: String, event: String) -> Status::Solved);
		let report = validate_script_against(source, "handler", &expected, &[]).unwrap();
		assert!(report.is_valid);
		assert!(report.contract_mismatches.is_empty());
	}

	#[test]
	fn param_tag_typo_is_caught_by_expected_signature() {
		// `@paran` parser ignoruje, kontrakt je sám o sobě konzistentní —
		// neshodu odhalí až porovnání s očekávanou signaturou.
		let source = r#"
            /// @paran sender String
            /// @return Status::Solved
            fn handler(sender) {
                Status::Solved
            }
        "#;
		let expected = contract!((sender: String) -> Status::Solved);
		let report = validate_script_against(source, "handler", &expected, &[]).unwrap();
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
		let report = validate_script_against(source, "handler", &expected, &[]).unwrap();
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
		let report = validate_script_against(source, "handler", &expected, &[]).unwrap();
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
		let report = validate_script_against(source, "handler", &expected, &[]).unwrap();
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
		let report = validate_script_against(source, "handler", &expected, &[]).unwrap();
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
		let report = validate_script_against(source, "handler", &expected, &[]).unwrap();
		assert!(!report.is_valid);
		assert_eq!(report.contract_mismatches.len(), 1);
		assert!(matches!(
			report.contract_mismatches[0],
			ContractMismatch::ReturnType { .. }
		));
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
		// Přesně problém z README: uživatel slíbí String, vrátí int.
		let source = r#"
            /// @return String
            fn process(input) {
                return 42;
            }
        "#;
		let report = validate_script(source, "process", &[]).unwrap();
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
		let report = validate_script(source, "process", &[]).unwrap();
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
		let report = validate_script(source, "process", &[]).unwrap();
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
		let report = validate_script(source, "process", &[]).unwrap();
		// Volající očekává int a helper deklaruje int, takže samotné
		// ResolvedCall na úrovni `process` je verified...
		assert!(report.main.is_valid);
		// ...ale helper svůj vlastní kontrakt neplní, takže celý skript neprojde.
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
		let builtins = vec![BuiltinSignature {
			name: "http::get".to_string(),
			return_type: TypeDef::Primitive(PrimitiveType::String),
		}];
		let report = validate_script(source, "process", &builtins).unwrap();
		assert!(report.is_valid);
		assert!(report.helpers.is_empty());
	}

	#[test]
	fn function_not_found_is_error() {
		let source = "fn other() { return 1; }";
		let result = validate_script(source, "process", &[]);
		assert_eq!(
			result.unwrap_err(),
			CheckerError::FunctionNotFound("process".to_string())
		);
	}

	#[test]
	fn missing_doc_comment_is_error() {
		let source = "fn process() { return 1; }";
		let result = validate_script(source, "process", &[]);
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
		let report = validate_script(source, "process", &[]).unwrap();
		assert!(report.is_valid);
		assert_eq!(report.main.static_result.unverifiable.len(), 1);
	}
}
