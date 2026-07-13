//! Concise notations for host-side declarations: [`contract!`] for expected
//! function signatures, [`methods!`] for method tables. Both stringify their
//! input and hand it to the contract-notation parser ([`crate::Contract::parse`],
//! [`crate::TypeDef::parse`]), so the type syntax is identical to doc-comment
//! contracts — and both panic on invalid syntax, being meant for declarations
//! hardcoded in host code. For input arriving at runtime use the parsing
//! functions directly; they return a `Result`.

use crate::types::{MethodSignature, TypeDef};

/// Builds a [`Contract`](crate::Contract) from a concise signature notation.
/// The type syntax is the same as in doc-comment contracts
/// (`@param`/`@return`).
///
/// ```
/// use rune_typechecker::contract;
///
/// let expected = contract!((sender: String, event: String) -> Status::Solved);
/// assert_eq!(expected.params.len(), 2);
/// ```
///
/// Panics on invalid syntax — it is meant for signatures hardcoded in host
/// code. For signatures arriving at runtime (e.g. from configuration) use
/// [`Contract::parse`](crate::Contract::parse), which returns a `Result`.
#[macro_export]
macro_rules! contract {
	($($signature:tt)+) => {
		$crate::Contract::parse(stringify!($($signature)+))
			.unwrap_or_else(|e| panic!("invalid contract signature: {e}"))
	};
}

/// Builds a `Vec<MethodSignature>` from a concise notation. Entries are
/// separated by `;` (return types may contain commas — `{ a: int, b: bool }`);
/// the return type uses the contract type syntax and is optional — without
/// `->` the method exists with an unknown return type.
///
/// ```
/// use rune_typechecker::methods;
///
/// let table = methods![
/// 	Sender::name() -> String;
/// 	AppContext::lookup() -> Option::Some(ComponentContext) | Option::None;
/// 	ComponentContext::set_value();
/// ];
/// assert_eq!(table.len(), 3);
/// assert_eq!(table[0].receiver, "Sender");
/// ```
///
/// Panics on an invalid type — it is meant for tables hardcoded in host
/// code, like the [`contract!`] macro.
#[macro_export]
macro_rules! methods {
	// -- internal: list assembly ----------------------------------------
	(@parse [$($out:expr,)*]) => {
		::std::vec![$($out,)*]
	};
	(@parse [$($out:expr,)*] $recv:ident :: $name:ident ( ) ; $($rest:tt)*) => {
		$crate::methods!(@parse [$($out,)*
			$crate::MethodSignature::new(::core::stringify!($recv), ::core::stringify!($name), ::core::option::Option::None),
		] $($rest)*)
	};
	(@parse [$($out:expr,)*] $recv:ident :: $name:ident ( )) => {
		$crate::methods!(@parse [$($out,)*
			$crate::MethodSignature::new(::core::stringify!($recv), ::core::stringify!($name), ::core::option::Option::None),
		])
	};
	(@parse [$($out:expr,)*] $recv:ident :: $name:ident ( ) -> $($rest:tt)*) => {
		$crate::methods!(@ret [$($out,)*] $recv $name [] $($rest)*)
	};
	// -- internal: accumulate the return type up to `;` or the end ------
	(@ret [$($out:expr,)*] $recv:ident $name:ident [$($ty:tt)+] ; $($rest:tt)*) => {
		$crate::methods!(@parse [$($out,)*
			$crate::__method_signature(::core::stringify!($recv), ::core::stringify!($name), ::core::stringify!($($ty)+)),
		] $($rest)*)
	};
	(@ret [$($out:expr,)*] $recv:ident $name:ident [$($ty:tt)+]) => {
		$crate::methods!(@parse [$($out,)*
			$crate::__method_signature(::core::stringify!($recv), ::core::stringify!($name), ::core::stringify!($($ty)+)),
		])
	};
	(@ret [$($out:expr,)*] $recv:ident $name:ident [$($ty:tt)*] $t:tt $($rest:tt)*) => {
		$crate::methods!(@ret [$($out,)*] $recv $name [$($ty)* $t] $($rest)*)
	};
	// -- public entry ----------------------------------------------------
	($($rest:tt)*) => {
		$crate::methods!(@parse [] $($rest)*)
	};
}

/// Support of the [`methods!`] macro — do not use directly.
#[doc(hidden)]
pub fn __method_signature(receiver: &str, name: &str, return_type: &str) -> MethodSignature {
	let type_def = TypeDef::parse(return_type)
		.unwrap_or_else(|e| panic!("invalid method return type `{return_type}`: {e}"));
	MethodSignature::new(receiver, name, Some(type_def))
}
