use crate::types::{CheckerError, Contract, EnumVariant, ParamDef, PrimitiveType, TypeDef};

/// Parses doc-comment content (the text after `///`, individual lines joined
/// with `\n`) into a structured [`Contract`].
///
/// Line format: `@param name: Type optional description` and
/// `@return Type optional description` — the text after the type is a human
/// description and does not affect the contract.
pub fn parse(text: &str) -> Result<Contract, CheckerError> {
	let mut params = Vec::new();
	let mut return_type = None;

	for line in text.lines() {
		let line = line.trim();
		if !line.starts_with('@') {
			continue;
		}

		if let Some(rest) = line.strip_prefix("@param") {
			let rest = rest.trim();
			let (name, rest) =
				take_ident(rest).ok_or_else(|| err(format!("invalid @param syntax: '{line}'")))?;
			let rest = rest.trim_start().strip_prefix(':').ok_or_else(|| {
				err(format!(
					"expected ':' after param name '{name}' in: '{line}'"
				))
			})?;
			// The rest of the line after the type is an optional description.
			let (type_def, _description) = parse_type(rest.trim_start())?;
			params.push(ParamDef {
				name: name.to_string(),
				type_def,
			});
		} else if let Some(rest) = line.strip_prefix("@return") {
			let (type_def, _description) = parse_type(rest.trim_start())?;
			return_type = Some(type_def);
		}
		// Unknown annotations are ignored (forward compatibility).
	}

	let return_type = return_type.ok_or_else(|| err("missing @return annotation".to_string()))?;

	Ok(Contract {
		params,
		return_type,
	})
}

fn err(msg: String) -> CheckerError {
	CheckerError::InvalidContractSyntax(msg)
}

impl Contract {
	/// Parses a signature of the form `(name: Type, ...) -> Type` into a
	/// [`Contract`]. The type syntax is the same as in doc-comment contracts
	/// (`@param`/`@return`).
	///
	/// For signatures written directly in host code the `contract!` macro,
	/// which calls this function, is more convenient.
	pub fn parse(signature: &str) -> Result<Contract, CheckerError> {
		parse_signature(signature)
	}
}

fn parse_signature(signature: &str) -> Result<Contract, CheckerError> {
	let rest = signature.trim_start();
	let rest = rest
		.strip_prefix('(')
		.ok_or_else(|| err(format!("expected '(' at start of signature: '{rest}'")))?;

	let mut params = Vec::new();
	let mut rest = rest.trim_start();

	if let Some(r) = rest.strip_prefix(')') {
		rest = r;
	} else {
		loop {
			let (name, r) = take_ident(rest)
				.ok_or_else(|| err(format!("expected parameter name in: '{rest}'")))?;
			let r = r
				.trim_start()
				.strip_prefix(':')
				.ok_or_else(|| err(format!("expected ':' after parameter '{name}'")))?;
			let (type_def, r) = parse_type(r.trim_start())?;
			params.push(ParamDef {
				name: name.to_string(),
				type_def,
			});

			let r = r.trim_start();
			if let Some(r2) = r.strip_prefix(',') {
				rest = r2.trim_start();
				if let Some(r3) = rest.strip_prefix(')') {
					rest = r3;
					break;
				}
				continue;
			}

			rest = r
				.strip_prefix(')')
				.ok_or_else(|| err(format!("expected ',' or ')' in params, found: '{r}'")))?;
			break;
		}
	}

	let rest = rest
		.trim_start()
		.strip_prefix("->")
		.ok_or_else(|| err(format!("expected '->' after params in: '{rest}'")))?;
	let (return_type, rest) = parse_type(rest.trim_start())?;
	if !rest.trim().is_empty() {
		return Err(err(format!(
			"unexpected trailing tokens after return type: '{rest}'"
		)));
	}

	Ok(Contract {
		params,
		return_type,
	})
}

/// One element of a union (separated by `|`), before deciding how to interpret the whole type.
enum Atom {
	Primitive(PrimitiveType),
	Object(Vec<(String, TypeDef)>),
	List(Box<TypeDef>),
	Unit,
	/// `path` or `path(inner)` — either an enum variant, or (when it is the
	/// only one in the union and has no inner value) a constructor of a
	/// parameterless variant.
	Path {
		path: Vec<String>,
		inner: Option<Box<TypeDef>>,
	},
}

fn atom_into_type_def(atom: Atom) -> TypeDef {
	match atom {
		Atom::Primitive(p) => TypeDef::Primitive(p),
		Atom::Object(fields) => TypeDef::Object(fields),
		Atom::List(inner) => TypeDef::List(inner),
		Atom::Unit => TypeDef::Unit,
		Atom::Path { path, inner } => TypeDef::Enum(vec![EnumVariant { path, inner }]),
	}
}

fn atom_into_enum_variant(atom: Atom) -> Result<EnumVariant, CheckerError> {
	match atom {
		Atom::Path { path, inner } => Ok(EnumVariant { path, inner }),
		_ => Err(err(
			"cannot mix enum variants with other types in a union".to_string()
		)),
	}
}

/// Parses a type (including `|` unions) and returns the unconsumed rest of the input.
fn parse_type(input: &str) -> Result<(TypeDef, &str), CheckerError> {
	let mut atoms = Vec::new();
	let mut rest = input.trim_start();

	loop {
		let (atom, r) = parse_atom(rest)?;
		atoms.push(atom);
		rest = r.trim_start();

		if let Some(r) = rest.strip_prefix('|') {
			rest = r.trim_start();
		} else {
			break;
		}
	}

	let unit_count = atoms.iter().filter(|a| matches!(a, Atom::Unit)).count();

	if atoms.len() == 1 {
		let type_def = atom_into_type_def(atoms.pop().unwrap());
		return Ok((type_def, rest));
	}

	if unit_count > 1 {
		return Err(err(
			"union contains more than one '()' alternative".to_string()
		));
	}

	if unit_count == 1 {
		let non_unit: Vec<Atom> = atoms
			.into_iter()
			.filter(|a| !matches!(a, Atom::Unit))
			.collect();

		let inner = if non_unit.len() == 1 {
			atom_into_type_def(non_unit.into_iter().next().unwrap())
		} else {
			let variants: Result<Vec<EnumVariant>, CheckerError> =
				non_unit.into_iter().map(atom_into_enum_variant).collect();
			TypeDef::Enum(variants?)
		};

		return Ok((TypeDef::Nullable(Box::new(inner)), rest));
	}

	let variants: Result<Vec<EnumVariant>, CheckerError> =
		atoms.into_iter().map(atom_into_enum_variant).collect();
	Ok((TypeDef::Enum(variants?), rest))
}

fn parse_atom(input: &str) -> Result<(Atom, &str), CheckerError> {
	let input = input.trim_start();

	if let Some(rest) = input.strip_prefix('{') {
		return parse_object(rest);
	}

	if let Some(rest) = input.strip_prefix('[') {
		let (inner, rest) = parse_type(rest)?;
		let rest = rest.trim_start();
		let rest = rest
			.strip_prefix(']')
			.ok_or_else(|| err(format!("expected ']' in: '{rest}'")))?;
		return Ok((Atom::List(Box::new(inner)), rest));
	}

	if let Some(rest) = input.strip_prefix('(') {
		let rest = rest.trim_start();
		let rest = rest
			.strip_prefix(')')
			.ok_or_else(|| err(format!("expected ')' for unit type in: '({rest}'")))?;
		return Ok((Atom::Unit, rest));
	}

	let (first, rest) =
		take_ident(input).ok_or_else(|| err(format!("expected a type, found: '{input}'")))?;

	let mut path = vec![first.to_string()];
	let mut rest = rest;

	// Whitespace around `::` is tolerated — `stringify!` in the `contract!`
	// macro may separate path tokens with spaces (`Status :: Solved`).
	while let Some(r) = rest.trim_start().strip_prefix("::") {
		let (seg, r) = take_ident(r.trim_start())
			.ok_or_else(|| err(format!("expected identifier after '::' in: '{rest}'")))?;
		path.push(seg.to_string());
		rest = r;
	}

	let rest_trimmed = rest.trim_start();
	if let Some(r) = rest_trimmed.strip_prefix('(') {
		let (inner, r) = parse_type(r)?;
		let r = r.trim_start();
		let r = r
			.strip_prefix(')')
			.ok_or_else(|| err(format!("expected ')' in: '{r}'")))?;
		return Ok((
			Atom::Path {
				path,
				inner: Some(Box::new(inner)),
			},
			r,
		));
	}

	if path.len() == 1 {
		if let Some(primitive) = primitive_from_name(&path[0]) {
			return Ok((Atom::Primitive(primitive), rest));
		}
	}

	Ok((Atom::Path { path, inner: None }, rest))
}

fn parse_object(input: &str) -> Result<(Atom, &str), CheckerError> {
	let mut fields = Vec::new();
	let mut rest = input.trim_start();

	if let Some(r) = rest.strip_prefix('}') {
		return Ok((Atom::Object(fields), r));
	}

	loop {
		let (name, r) =
			take_ident(rest).ok_or_else(|| err(format!("expected field name in: '{rest}'")))?;
		let r = r.trim_start();
		let r = r
			.strip_prefix(':')
			.ok_or_else(|| err(format!("expected ':' after field '{name}'")))?;
		let (type_def, r) = parse_type(r)?;
		fields.push((name.to_string(), type_def));

		let r = r.trim_start();
		if let Some(r) = r.strip_prefix(',') {
			rest = r.trim_start();
			if let Some(r) = rest.strip_prefix('}') {
				return Ok((Atom::Object(fields), r));
			}
			continue;
		}

		let r = r
			.strip_prefix('}')
			.ok_or_else(|| err(format!("expected ',' or '}}' in object, found: '{r}'")))?;
		return Ok((Atom::Object(fields), r));
	}
}

fn primitive_from_name(name: &str) -> Option<PrimitiveType> {
	match name {
		"String" => Some(PrimitiveType::String),
		"int" => Some(PrimitiveType::Int),
		"float" => Some(PrimitiveType::Float),
		"bool" => Some(PrimitiveType::Bool),
		"bytes" => Some(PrimitiveType::Bytes),
		_ => None,
	}
}

fn take_ident(input: &str) -> Option<(&str, &str)> {
	let mut chars = input.char_indices();
	let (_, first) = chars.next()?;
	if !(first.is_alphabetic() || first == '_') {
		return None;
	}

	let mut end = input.len();
	for (idx, c) in chars {
		if !(c.is_alphanumeric() || c == '_') {
			end = idx;
			break;
		}
	}

	Some((&input[..end], &input[end..]))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn primitive_types() {
		let contract =
			parse("@param name: String\n@param age: int\n@param active: bool\n@return String")
				.unwrap();
		assert_eq!(contract.params.len(), 3);
		assert_eq!(contract.params[0].name, "name");
		assert_eq!(
			contract.params[0].type_def,
			TypeDef::Primitive(PrimitiveType::String)
		);
		assert_eq!(
			contract.params[1].type_def,
			TypeDef::Primitive(PrimitiveType::Int)
		);
		assert_eq!(
			contract.params[2].type_def,
			TypeDef::Primitive(PrimitiveType::Bool)
		);
		assert_eq!(
			contract.return_type,
			TypeDef::Primitive(PrimitiveType::String)
		);
	}

	#[test]
	fn object_shape() {
		let contract = parse("@return { status: String, code: int, active: bool }").unwrap();
		assert_eq!(
			contract.return_type,
			TypeDef::Object(vec![
				(
					"status".to_string(),
					TypeDef::Primitive(PrimitiveType::String)
				),
				("code".to_string(), TypeDef::Primitive(PrimitiveType::Int)),
				(
					"active".to_string(),
					TypeDef::Primitive(PrimitiveType::Bool)
				),
			])
		);
	}

	#[test]
	fn enum_variant() {
		let contract = parse("@return Result::Ok(int) | Result::Err(String)").unwrap();
		assert_eq!(
			contract.return_type,
			TypeDef::Enum(vec![
				EnumVariant {
					path: vec!["Result".to_string(), "Ok".to_string()],
					inner: Some(Box::new(TypeDef::Primitive(PrimitiveType::Int))),
				},
				EnumVariant {
					path: vec!["Result".to_string(), "Err".to_string()],
					inner: Some(Box::new(TypeDef::Primitive(PrimitiveType::String))),
				},
			])
		);
	}

	#[test]
	fn nested_types() {
		let contract =
			parse("@return { status: String, data: { id: int, name: String } }").unwrap();
		assert_eq!(
			contract.return_type,
			TypeDef::Object(vec![
				(
					"status".to_string(),
					TypeDef::Primitive(PrimitiveType::String)
				),
				(
					"data".to_string(),
					TypeDef::Object(vec![
						("id".to_string(), TypeDef::Primitive(PrimitiveType::Int)),
						(
							"name".to_string(),
							TypeDef::Primitive(PrimitiveType::String)
						),
					])
				),
			])
		);
	}

	#[test]
	fn list_type() {
		let contract = parse("@return [String]").unwrap();
		assert_eq!(
			contract.return_type,
			TypeDef::List(Box::new(TypeDef::Primitive(PrimitiveType::String)))
		);
	}

	#[test]
	fn list_combinations() {
		let contract = parse("@return { items: [int] }").unwrap();
		assert_eq!(
			contract.return_type,
			TypeDef::Object(vec![(
				"items".to_string(),
				TypeDef::List(Box::new(TypeDef::Primitive(PrimitiveType::Int)))
			)])
		);

		let contract = parse("@return [{ id: int, name: String }]").unwrap();
		assert_eq!(
			contract.return_type,
			TypeDef::List(Box::new(TypeDef::Object(vec![
				("id".to_string(), TypeDef::Primitive(PrimitiveType::Int)),
				(
					"name".to_string(),
					TypeDef::Primitive(PrimitiveType::String)
				),
			])))
		);
	}

	#[test]
	fn nullable() {
		let contract = parse("@return String | ()").unwrap();
		assert_eq!(
			contract.return_type,
			TypeDef::Nullable(Box::new(TypeDef::Primitive(PrimitiveType::String)))
		);
	}

	#[test]
	fn ignores_unknown_annotations_and_non_at_lines() {
		let contract = parse("some free text\n@unknown foo\n@return int").unwrap();
		assert_eq!(contract.return_type, TypeDef::Primitive(PrimitiveType::Int));
	}

	#[test]
	fn signature_basic() {
		let contract = Contract::parse("(sender: String, count: int) -> bool").unwrap();
		assert_eq!(contract.params.len(), 2);
		assert_eq!(contract.params[0].name, "sender");
		assert_eq!(
			contract.params[0].type_def,
			TypeDef::Primitive(PrimitiveType::String)
		);
		assert_eq!(contract.params[1].name, "count");
		assert_eq!(
			contract.params[1].type_def,
			TypeDef::Primitive(PrimitiveType::Int)
		);
		assert_eq!(
			contract.return_type,
			TypeDef::Primitive(PrimitiveType::Bool)
		);
	}

	#[test]
	fn signature_no_params_unit_return() {
		let contract = Contract::parse("() -> ()").unwrap();
		assert!(contract.params.is_empty());
		assert_eq!(contract.return_type, TypeDef::Unit);
	}

	#[test]
	fn signature_enum_path_with_spaces_around_separators() {
		// The shape produced by `stringify!` in the `contract!` macro.
		let contract = Contract::parse("( sender : String ) -> Status :: Solved").unwrap();
		assert_eq!(contract.params[0].name, "sender");
		assert_eq!(
			contract.return_type,
			TypeDef::Enum(vec![EnumVariant {
				path: vec!["Status".to_string(), "Solved".to_string()],
				inner: None,
			}])
		);
	}

	#[test]
	fn signature_missing_arrow_is_error() {
		let result = Contract::parse("(sender: String)");
		assert!(matches!(
			result,
			Err(CheckerError::InvalidContractSyntax(_))
		));
	}

	#[test]
	fn signature_trailing_tokens_is_error() {
		let result = Contract::parse("() -> int garbage");
		assert!(matches!(
			result,
			Err(CheckerError::InvalidContractSyntax(_))
		));
	}

	#[test]
	fn missing_return_is_error() {
		let result = parse("@param name: String");
		assert!(matches!(
			result,
			Err(CheckerError::InvalidContractSyntax(_))
		));
	}

	#[test]
	fn param_without_colon_is_error() {
		// The old format `@param name Type` (without a colon) is no longer accepted.
		let result = parse("@param name String\n@return int");
		assert!(matches!(
			result,
			Err(CheckerError::InvalidContractSyntax(_))
		));
	}

	#[test]
	fn description_after_type_is_allowed() {
		let contract = parse(
			"@param sender: String Who sent the message\n@return Status::Solved Processing outcome",
		)
		.unwrap();
		assert_eq!(contract.params.len(), 1);
		assert_eq!(contract.params[0].name, "sender");
		assert_eq!(
			contract.params[0].type_def,
			TypeDef::Primitive(PrimitiveType::String)
		);
		assert_eq!(
			contract.return_type,
			TypeDef::Enum(vec![EnumVariant {
				path: vec!["Status".to_string(), "Solved".to_string()],
				inner: None,
			}])
		);
	}

	#[test]
	fn description_does_not_swallow_union_type() {
		// A description may follow a union too — `|` continues the type only
		// when it immediately follows the previous alternative.
		let contract = parse("@param id: String | () empty for anonymous\n@return int").unwrap();
		assert_eq!(
			contract.params[0].type_def,
			TypeDef::Nullable(Box::new(TypeDef::Primitive(PrimitiveType::String)))
		);
	}

	#[test]
	fn invalid_syntax_is_error() {
		let result = parse("@return {");
		assert!(matches!(
			result,
			Err(CheckerError::InvalidContractSyntax(_))
		));
	}
}
