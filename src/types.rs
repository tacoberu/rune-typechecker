use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Contract {
	pub params: Vec<ParamDef>,
	pub return_type: TypeDef,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamDef {
	pub name: String,
	pub type_def: TypeDef,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeDef {
	Primitive(PrimitiveType),
	Object(Vec<(String, TypeDef)>),
	Enum(Vec<EnumVariant>),
	Nullable(Box<TypeDef>),
	List(Box<TypeDef>),
	Unit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveType {
	String,
	Int,
	Float,
	Bool,
	Bytes,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
	pub path: Vec<String>,
	pub inner: Option<Box<TypeDef>>,
}

impl EnumVariant {
	/// `Status` — a bare enum name (single-segment path with no inner value).
	/// As long as no variants are enumerated, it matches any variant of the
	/// same enum (`Status::*`).
	pub fn is_bare_enum_name(&self) -> bool {
		self.path.len() == 1 && self.inner.is_none()
	}

	/// Whether this declared alternative accepts an enum variant with the given `path`.
	pub fn accepts_path(&self, path: &[String]) -> bool {
		if self.is_bare_enum_name() {
			self.path.first() == path.first()
		} else {
			self.path.last() == path.last()
		}
	}

	/// Name-level comparison of two alternatives (symmetric): if one side is a
	/// bare enum name, matching the enum is enough; otherwise variants are
	/// compared.
	pub fn matches_name(&self, other: &EnumVariant) -> bool {
		if self.is_bare_enum_name() || other.is_bare_enum_name() {
			self.path.first() == other.path.first()
		} else {
			self.path.last() == other.path.last()
		}
	}

	/// Compatibility including the inner value — it is only checked between
	/// concrete variants; a bare enum name says nothing about the payload.
	pub fn is_compatible_with(&self, other: &EnumVariant) -> bool {
		if !self.matches_name(other) {
			return false;
		}
		if self.is_bare_enum_name() || other.is_bare_enum_name() {
			return true;
		}
		match (&self.inner, &other.inner) {
			(None, None) => true,
			(Some(a), Some(b)) => a.is_compatible_with(b),
			_ => false,
		}
	}
}

impl TypeDef {
	/// Symmetric compatibility of two declared types. Unlike `==`, a bare enum
	/// name (`Status`) matches any union of its variants
	/// (`Status::Solved | Status::Continue`) — and vice versa.
	pub fn is_compatible_with(&self, other: &TypeDef) -> bool {
		match (self, other) {
			(TypeDef::Primitive(a), TypeDef::Primitive(b)) => a == b,
			(TypeDef::Unit, TypeDef::Unit) => true,
			(TypeDef::Nullable(a), TypeDef::Nullable(b)) => a.is_compatible_with(b),
			(TypeDef::List(a), TypeDef::List(b)) => a.is_compatible_with(b),
			(TypeDef::Object(xs), TypeDef::Object(ys)) => {
				xs.len() == ys.len()
					&& xs.iter().all(|(name, x)| {
						ys.iter().any(|(n, y)| n == name && x.is_compatible_with(y))
					})
			}
			(TypeDef::Enum(xs), TypeDef::Enum(ys)) => {
				xs.iter()
					.all(|x| ys.iter().any(|y| x.is_compatible_with(y)))
					&& ys
						.iter()
						.all(|y| xs.iter().any(|x| x.is_compatible_with(y)))
			}
			_ => false,
		}
	}
}

// Types are displayed in the same syntax the contract notation uses (both
// `@param`/`@return` and `Contract::parse`), so messages can be read — and if
// needed copied verbatim — as a contract.

impl std::fmt::Display for PrimitiveType {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(match self {
			PrimitiveType::String => "String",
			PrimitiveType::Int => "int",
			PrimitiveType::Float => "float",
			PrimitiveType::Bool => "bool",
			PrimitiveType::Bytes => "bytes",
		})
	}
}

impl std::fmt::Display for EnumVariant {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.path.join("::"))?;
		if let Some(inner) = &self.inner {
			write!(f, "({inner})")?;
		}
		Ok(())
	}
}

impl std::fmt::Display for TypeDef {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			TypeDef::Primitive(p) => write!(f, "{p}"),
			TypeDef::Object(fields) => {
				if fields.is_empty() {
					return write!(f, "{{}}");
				}
				write!(f, "{{ ")?;
				for (i, (name, type_def)) in fields.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{name}: {type_def}")?;
				}
				write!(f, " }}")
			}
			TypeDef::Enum(variants) => {
				for (i, variant) in variants.iter().enumerate() {
					if i > 0 {
						write!(f, " | ")?;
					}
					write!(f, "{variant}")?;
				}
				Ok(())
			}
			TypeDef::Nullable(inner) => write!(f, "{inner} | ()"),
			TypeDef::List(inner) => write!(f, "[{inner}]"),
			TypeDef::Unit => write!(f, "()"),
		}
	}
}

/// Signature of a builtin function supplied by the host system (group 3).
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinSignature {
	pub name: String,
	pub return_type: TypeDef,
}

/// Origin of a return type in the `SignatureRegistry`.
#[derive(Debug, Clone, PartialEq)]
pub enum SignatureOrigin {
	/// Script helper function — has a body; recursively verified on ResolvedCall.
	Helper(TypeDef),
	/// Builtin function — has no body; accepted as supplied.
	Builtin(TypeDef),
}

impl SignatureOrigin {
	pub fn type_def(&self) -> &TypeDef {
		match self {
			SignatureOrigin::Helper(t) => t,
			SignatureOrigin::Builtin(t) => t,
		}
	}

	pub fn is_helper(&self) -> bool {
		matches!(self, SignatureOrigin::Helper(_))
	}
}

#[derive(Debug, Clone, Default)]
pub struct SignatureRegistry {
	pub signatures: HashMap<String, SignatureOrigin>,
}

/// Why a particular expression could not be evaluated statically (see docs/future-type-inference.md).
#[derive(Debug, Clone, PartialEq)]
pub enum DynamicReason {
	/// return x; — a local variable, no dataflow tracking.
	Variable(String),
	/// return helper(x); helper not found in the SignatureRegistry.
	UnannotatedCall(String),
	/// return f(x); where f is an expression/variable, not a function name.
	IndirectCall,
	/// return value.compute(); — a method call on a value.
	MethodCall(String),
	/// `expr?` on an expression of unknown type — may propagate an error
	/// variant we do not know statically.
	TryPropagation,
	/// An operator, field/index access, or any other expression.
	Expression,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
	String(String),
	Int(i64),
	Float(f64),
	Bool(bool),
	Object(Vec<(String, LiteralValue)>),
	Enum {
		path: Vec<String>,
		inner: Option<Box<LiteralValue>>,
	},
	List(Vec<LiteralValue>),
	Unit,
	ResolvedCall {
		name: String,
		type_def: TypeDef,
	},
	Dynamic(DynamicReason),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReturnSite {
	ObjectLiteral(Vec<(String, LiteralValue)>),
	PrimitiveLiteral(LiteralValue),
	EnumLiteral {
		path: Vec<String>,
		inner: Option<Box<LiteralValue>>,
	},
	Unit,
	ResolvedCall {
		name: String,
		type_def: TypeDef,
	},
	Dynamic(DynamicReason),
	/// The `?` operator — a hidden early return; on None/Err it exits the
	/// function with a value the contract typically does not admit.
	TryPropagation {
		line: usize,
	},
}

#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
	pub site: ReturnSite,
	pub expected: TypeDef,
	pub actual: String,
}

#[derive(Debug, Clone, Default)]
pub struct StaticCheckResult {
	pub verified: Vec<ReturnSite>,
	pub unverifiable: Vec<ReturnSite>,
	pub violations: Vec<Violation>,
}

#[derive(Debug, Clone)]
pub struct ValidationReport {
	pub function_name: String,
	pub contract: Contract,
	pub static_result: StaticCheckResult,
	pub is_valid: bool,
}

/// Mismatch between the contract declared by the script and the signature
/// the host system expects of the function (see `validate_script_against`).
#[derive(Debug, Clone, PartialEq)]
pub enum ContractMismatch {
	/// The script declares a different number of parameters than the host expects.
	ParamCount { expected: usize, actual: usize },
	/// The parameter at the given position differs in type (names are not
	/// compared — the script names its parameters as it likes).
	Param {
		index: usize,
		expected: ParamDef,
		actual: ParamDef,
	},
	/// The return type differs.
	ReturnType { expected: TypeDef, actual: TypeDef },
}

impl std::fmt::Display for ContractMismatch {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			ContractMismatch::ParamCount { expected, actual } => {
				write!(f, "expected {expected} params, script declares {actual}")
			}
			ContractMismatch::Param {
				index,
				expected,
				actual,
			} => write!(
				f,
				"param {index} '{}': expected `{}`, script declares `{}`",
				expected.name, expected.type_def, actual.type_def
			),
			ContractMismatch::ReturnType { expected, actual } => {
				write!(
					f,
					"return type: expected `{expected}`, script declares `{actual}`"
				)
			}
		}
	}
}

#[derive(Debug, Clone)]
pub struct ScriptValidationReport {
	pub main: ValidationReport,
	pub helpers: HashMap<String, ValidationReport>,
	/// Mismatches against the host-expected signature — filled only by
	/// `validate_script_against`; `validate_script` leaves it empty.
	pub contract_mismatches: Vec<ContractMismatch>,
	pub is_valid: bool,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CheckerError {
	#[error("Function '{0}' not found in script")]
	FunctionNotFound(String),
	#[error("Function has no contract doc-comment")]
	NoDocComment,
	#[error("Invalid contract syntax: {0}")]
	InvalidContractSyntax(String),
	#[error("Rune parse error: {0}")]
	RuneParseError(String),
}
