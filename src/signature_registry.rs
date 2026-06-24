use rune::ast;

use crate::ast_analyzer::doc_comment_before;
use crate::doc_comment;
use crate::source_text::slice;
use crate::types::{BuiltinSignature, CheckerError, SignatureOrigin, SignatureRegistry};

/// Builds a `SignatureRegistry` from all helper functions in the script
/// (except `target_function_name`, which is the contracted function) and from
/// the supplied builtins. On a name collision the script helper wins.
pub fn build(
	file: &ast::File,
	source: &str,
	target_function_name: &str,
	builtins: &[BuiltinSignature],
) -> Result<SignatureRegistry, CheckerError> {
	let mut registry = SignatureRegistry::default();

	for (item, _) in &file.items {
		let ast::Item::Fn(item_fn) = item else {
			continue;
		};

		let name = slice(source, item_fn.name.span);
		if name == target_function_name {
			continue;
		}

		let Some(doc) = doc_comment_before(source, item_fn) else {
			continue;
		};

		let contract = doc_comment::parse(&doc)?;
		registry.signatures.insert(
			name.to_string(),
			SignatureOrigin::Helper(contract.return_type),
		);
	}

	for builtin in builtins {
		registry
			.signatures
			.entry(builtin.name.clone())
			.or_insert_with(|| SignatureOrigin::Builtin(builtin.return_type.clone()));
	}

	Ok(registry)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ast_analyzer::parse_file;
	use crate::types::{PrimitiveType, TypeDef};

	#[test]
	fn helper_with_contract_is_registered() {
		let source = r#"
            /// @return String
            fn helper() {
                return "x";
            }

            fn process() {
                return helper();
            }
        "#;
		let file = parse_file(source).unwrap();
		let registry = build(&file, source, "process", &[]).unwrap();
		assert_eq!(
			registry.signatures.get("helper"),
			Some(&SignatureOrigin::Helper(TypeDef::Primitive(
				PrimitiveType::String
			)))
		);
	}

	#[test]
	fn helper_without_contract_is_not_registered() {
		let source = r#"
            fn helper() {
                return "x";
            }
            fn process() {
                return helper();
            }
        "#;
		let file = parse_file(source).unwrap();
		let registry = build(&file, source, "process", &[]).unwrap();
		assert!(registry.signatures.get("helper").is_none());
	}

	#[test]
	fn helper_with_invalid_contract_is_error() {
		let source = r#"
            /// @return {
            fn helper() {
                return "x";
            }
            fn process() {
                return helper();
            }
        "#;
		let file = parse_file(source).unwrap();
		let result = build(&file, source, "process", &[]);
		assert!(matches!(
			result,
			Err(CheckerError::InvalidContractSyntax(_))
		));
	}

	#[test]
	fn builtin_is_registered() {
		let source = "fn process() { return 1; }";
		let file = parse_file(source).unwrap();
		let builtins = vec![BuiltinSignature {
			name: "http::get".to_string(),
			return_type: TypeDef::Primitive(PrimitiveType::String),
		}];
		let registry = build(&file, source, "process", &builtins).unwrap();
		assert_eq!(
			registry.signatures.get("http::get"),
			Some(&SignatureOrigin::Builtin(TypeDef::Primitive(
				PrimitiveType::String
			)))
		);
	}

	#[test]
	fn helper_takes_precedence_over_builtin_with_same_name() {
		let source = r#"
            /// @return int
            fn helper() {
                return 1;
            }
            fn process() {
                return helper();
            }
        "#;
		let file = parse_file(source).unwrap();
		let builtins = vec![BuiltinSignature {
			name: "helper".to_string(),
			return_type: TypeDef::Primitive(PrimitiveType::String),
		}];
		let registry = build(&file, source, "process", &builtins).unwrap();
		assert_eq!(
			registry.signatures.get("helper"),
			Some(&SignatureOrigin::Helper(TypeDef::Primitive(
				PrimitiveType::Int
			)))
		);
	}

	#[test]
	fn target_function_itself_is_not_registered_as_helper() {
		let source = r#"
            /// @return String
            fn process() {
                return "x";
            }
        "#;
		let file = parse_file(source).unwrap();
		let registry = build(&file, source, "process", &[]).unwrap();
		assert!(registry.signatures.get("process").is_none());
	}
}
