use rune::SourceId;
use rune::ast;
use rune::ast::OptionSpanned;

use crate::source_text::{line_of, lit_str_value, slice};
use crate::types::{
	CheckerError, DynamicReason, EnumVariant, LiteralValue, ReturnSite, SignatureRegistry, TypeDef,
};

pub fn parse_file(source: &str) -> Result<ast::File, CheckerError> {
	rune::parse::parse_all::<ast::File>(source, SourceId::EMPTY, false)
		.map_err(|e| CheckerError::RuneParseError(e.to_string()))
}

/// Finds a top-level function in the file by name. Returns `None` if it does not exist.
pub fn find_function<'a>(file: &'a ast::File, source: &str, name: &str) -> Option<&'a ast::ItemFn> {
	file.items.iter().find_map(|(item, _)| match item {
		ast::Item::Fn(item_fn) if slice(source, item_fn.name.span) == name => Some(item_fn),
		_ => None,
	})
}

/// Returns the joined content of the doc-comment immediately preceding the
/// given function, or `None` if there is none. Both formats are supported:
/// line style `///` (lines without the prefix) and block style `/** ... */`
/// (lines without the decorative leading `*`).
///
/// Implemented as a plain backwards scan of the source text from the start of
/// the function, not via `ItemFn::attributes` — the `#[doc = ...]` attributes
/// synthesized from `///` can only be recognized in rune through an internal
/// (`pub(crate)`) resolve mechanism that is not accessible from here.
pub fn doc_comment_before(file_source: &str, item_fn: &ast::ItemFn) -> Option<String> {
	// Scanning must start from the very beginning of the declaration —
	// `pub`/`const`/`async` precede the `fn` token, and otherwise the backward
	// pass would stop at the modifier line fragment instead of the doc-comment.
	let start = item_fn
		.visibility
		.option_span()
		.map(|s| s.start)
		.into_iter()
		.chain(item_fn.const_token.map(|t| t.span.start))
		.chain(item_fn.async_token.map(|t| t.span.start))
		.chain([item_fn.fn_token.span.start])
		.min()
		.unwrap();
	let before = &file_source[..start.into_usize()];

	line_doc_comment(before).or_else(|| block_doc_comment(before))
}

/// Line format: a contiguous block of `///` lines right above the declaration.
fn line_doc_comment(before: &str) -> Option<String> {
	let mut doc_lines = Vec::new();
	for line in before.lines().rev() {
		let trimmed = line.trim();
		if trimmed.is_empty() {
			continue;
		}
		if let Some(rest) = trimmed.strip_prefix("///") {
			doc_lines.push(rest.trim_start().to_string());
		} else {
			break;
		}
	}

	if doc_lines.is_empty() {
		return None;
	}

	doc_lines.reverse();
	Some(doc_lines.join("\n"))
}

/// Block format: `/** ... */` ending right above the declaration. Each content
/// line has its decorative leading `*` stripped (JSDoc style).
fn block_doc_comment(before: &str) -> Option<String> {
	let trimmed = before.trim_end();
	let content = trimmed.strip_suffix("*/")?;
	let open = content.rfind("/**")?;
	let content = &content[open + 3..];
	if content.contains("*/") {
		// The found `/**` belongs to an earlier, already closed comment — the
		// block right above the function is a plain `/* ... */`.
		return None;
	}

	let lines: Vec<&str> = content
		.lines()
		.map(|line| {
			let line = line.trim();
			line.strip_prefix('*').map(str::trim_start).unwrap_or(line)
		})
		.collect();

	let joined = lines.join("\n");
	let joined = joined.trim();
	if joined.is_empty() {
		return None;
	}
	Some(joined.to_string())
}

/// Finds all sites where `item_fn` returns a value (explicit `return`
/// anywhere in the body, as well as the implicit tail expression).
pub fn find_return_sites(
	item_fn: &ast::ItemFn,
	source: &str,
	registry: &SignatureRegistry,
) -> Vec<ReturnSite> {
	let mut sites = Vec::new();
	scan_returns_in_block(&item_fn.body, source, registry, &mut sites);
	collect_tail_sites_block(&item_fn.body, source, registry, &mut sites);
	sites
}

// ---------------------------------------------------------------------------
// Explicit `return` anywhere in the body (incl. nested if/match/loop blocks).
// ---------------------------------------------------------------------------

fn scan_returns_in_block(
	block: &ast::Block,
	source: &str,
	registry: &SignatureRegistry,
	out: &mut Vec<ReturnSite>,
) {
	for stmt in &block.statements {
		match stmt {
			ast::Stmt::Local(local) => scan_returns_in_expr(&local.expr, source, registry, out),
			ast::Stmt::Expr(expr) => scan_returns_in_expr(expr, source, registry, out),
			ast::Stmt::Semi(semi) => scan_returns_in_expr(&semi.expr, source, registry, out),
			ast::Stmt::Item(..) => {}
			_ => {}
		}
	}
}

fn scan_returns_in_expr(
	expr: &ast::Expr,
	source: &str,
	registry: &SignatureRegistry,
	out: &mut Vec<ReturnSite>,
) {
	match expr {
		ast::Expr::Return(ret) => {
			let site = match &ret.expr {
				Some(inner) => {
					scan_returns_in_expr(inner, source, registry, out);
					classify_top(inner, source, registry)
				}
				None => ReturnSite::Unit,
			};
			out.push(site);
		}
		ast::Expr::Block(b) => scan_returns_in_block(&b.block, source, registry, out),
		ast::Expr::If(if_expr) => {
			scan_returns_in_condition(&if_expr.condition, source, registry, out);
			scan_returns_in_block(&if_expr.block, source, registry, out);
			for else_if in &if_expr.expr_else_ifs {
				scan_returns_in_condition(&else_if.condition, source, registry, out);
				scan_returns_in_block(&else_if.block, source, registry, out);
			}
			if let Some(else_) = &if_expr.expr_else {
				scan_returns_in_block(&else_.block, source, registry, out);
			}
		}
		ast::Expr::Match(m) => {
			scan_returns_in_expr(&m.expr, source, registry, out);
			for (branch, _) in &m.branches {
				if let Some((_, cond)) = &branch.condition {
					scan_returns_in_expr(cond, source, registry, out);
				}
				scan_returns_in_expr(&branch.body, source, registry, out);
			}
		}
		ast::Expr::Loop(l) => scan_returns_in_block(&l.body, source, registry, out),
		ast::Expr::While(w) => {
			scan_returns_in_condition(&w.condition, source, registry, out);
			scan_returns_in_block(&w.body, source, registry, out);
		}
		ast::Expr::For(f) => {
			scan_returns_in_expr(&f.iter, source, registry, out);
			scan_returns_in_block(&f.body, source, registry, out);
		}
		ast::Expr::Binary(b) => {
			scan_returns_in_expr(&b.lhs, source, registry, out);
			scan_returns_in_expr(&b.rhs, source, registry, out);
		}
		ast::Expr::Unary(u) => scan_returns_in_expr(&u.expr, source, registry, out),
		ast::Expr::Group(g) => scan_returns_in_expr(&g.expr, source, registry, out),
		ast::Expr::Try(t) => {
			scan_returns_in_expr(&t.expr, source, registry, out);
			// `expr?` is a hidden early return of the error variant.
			if let Some(site) = try_early_return_site(t, source, registry) {
				out.push(site);
			}
		}
		ast::Expr::Await(a) => scan_returns_in_expr(&a.expr, source, registry, out),
		ast::Expr::Assign(a) => {
			scan_returns_in_expr(&a.lhs, source, registry, out);
			scan_returns_in_expr(&a.rhs, source, registry, out);
		}
		ast::Expr::FieldAccess(fa) => scan_returns_in_expr(&fa.expr, source, registry, out),
		ast::Expr::Index(idx) => {
			scan_returns_in_expr(&idx.target, source, registry, out);
			scan_returns_in_expr(&idx.index, source, registry, out);
		}
		ast::Expr::Call(call) => {
			scan_returns_in_expr(&call.expr, source, registry, out);
			for (arg, _) in call.args.iter() {
				scan_returns_in_expr(arg, source, registry, out);
			}
		}
		ast::Expr::Object(obj) => {
			for (assign, _) in obj.assignments.iter() {
				if let Some((_, value)) = &assign.assign {
					scan_returns_in_expr(value, source, registry, out);
				}
			}
		}
		ast::Expr::Vec(v) => {
			for (item, _) in v.items.iter() {
				scan_returns_in_expr(item, source, registry, out);
			}
		}
		ast::Expr::Break(b) => {
			if let Some(brk) = &b.expr {
				scan_returns_in_expr(brk, source, registry, out);
			}
		}
		ast::Expr::Yield(y) => {
			if let Some(inner) = &y.expr {
				scan_returns_in_expr(inner, source, registry, out);
			}
		}
		// Closures have their own return scope — a `return` inside belongs to
		// them, not to the surrounding function body.
		ast::Expr::Closure(_) => {}
		_ => {}
	}
}

fn scan_returns_in_condition(
	condition: &ast::Condition,
	source: &str,
	registry: &SignatureRegistry,
	out: &mut Vec<ReturnSite>,
) {
	match condition {
		ast::Condition::Expr(e) => scan_returns_in_expr(e, source, registry, out),
		ast::Condition::ExprLet(expr_let) => {
			scan_returns_in_expr(&expr_let.expr, source, registry, out)
		}
		_ => {}
	}
}

// ---------------------------------------------------------------------------
// Implicit (tail) return value of the function body.
// ---------------------------------------------------------------------------

enum BlockTail<'a> {
	/// The block implicitly evaluates to `()` (the last statement has no value).
	Unit,
	/// The last statement is an expression without `;` — its value is the block's value.
	Expr(&'a ast::Expr),
	/// The last statement is a (semicolon-terminated) `return ...;` — it is
	/// already recorded by `scan_returns_in_block`, and the block itself
	/// returns nothing further (`return` diverges, no fall-through occurs).
	Diverges,
}

fn collect_tail_sites_block(
	block: &ast::Block,
	source: &str,
	registry: &SignatureRegistry,
	out: &mut Vec<ReturnSite>,
) {
	match block_tail(block) {
		BlockTail::Expr(expr) => collect_tail_sites_expr(expr, source, registry, out),
		BlockTail::Unit => out.push(ReturnSite::Unit),
		BlockTail::Diverges => {}
	}
}

fn block_tail(block: &ast::Block) -> BlockTail<'_> {
	match block.statements.last() {
		Some(ast::Stmt::Expr(expr)) => {
			if matches!(expr, ast::Expr::Return(_)) {
				BlockTail::Diverges
			} else {
				BlockTail::Expr(expr)
			}
		}
		Some(ast::Stmt::Semi(semi)) if matches!(semi.expr, ast::Expr::Return(_)) => {
			BlockTail::Diverges
		}
		_ => BlockTail::Unit,
	}
}

fn collect_tail_sites_expr(
	expr: &ast::Expr,
	source: &str,
	registry: &SignatureRegistry,
	out: &mut Vec<ReturnSite>,
) {
	match expr {
		ast::Expr::Block(b) => collect_tail_sites_block(&b.block, source, registry, out),
		ast::Expr::If(if_expr) => {
			collect_tail_sites_block(&if_expr.block, source, registry, out);
			for else_if in &if_expr.expr_else_ifs {
				collect_tail_sites_block(&else_if.block, source, registry, out);
			}
			match &if_expr.expr_else {
				Some(else_) => collect_tail_sites_block(&else_.block, source, registry, out),
				None => out.push(ReturnSite::Unit),
			}
		}
		ast::Expr::Match(m) => {
			for (branch, _) in &m.branches {
				collect_tail_sites_expr(&branch.body, source, registry, out);
			}
		}
		_ => out.push(classify_top(expr, source, registry)),
	}
}

// ---------------------------------------------------------------------------
// Classification of a single expression into LiteralValue / ReturnSite.
// ---------------------------------------------------------------------------

fn classify_top(expr: &ast::Expr, source: &str, registry: &SignatureRegistry) -> ReturnSite {
	match classify_value(expr, source, registry) {
		LiteralValue::Object(fields) => ReturnSite::ObjectLiteral(fields),
		LiteralValue::Enum { path, inner } => ReturnSite::EnumLiteral { path, inner },
		LiteralValue::Unit => ReturnSite::Unit,
		LiteralValue::ResolvedCall { name, type_def } => {
			ReturnSite::ResolvedCall { name, type_def }
		}
		LiteralValue::Dynamic(reason) => ReturnSite::Dynamic(reason),
		other => ReturnSite::PrimitiveLiteral(other),
	}
}

fn classify_value(expr: &ast::Expr, source: &str, registry: &SignatureRegistry) -> LiteralValue {
	match expr {
		ast::Expr::Lit(lit_expr) => classify_lit(&lit_expr.lit, source),
		ast::Expr::Object(obj) => classify_object(obj, source, registry),
		ast::Expr::Vec(v) => LiteralValue::List(
			v.items
				.iter()
				.map(|(e, _)| classify_value(e, source, registry))
				.collect(),
		),
		ast::Expr::Call(call) => classify_call(call, source, registry),
		ast::Expr::Path(path) => classify_bare_path(path, source),
		ast::Expr::Group(g) => classify_value(&g.expr, source, registry),
		ast::Expr::Tuple(t) if t.items.is_empty() => LiteralValue::Unit,
		ast::Expr::Try(t) => classify_try_value(&t.expr, source, registry),
		_ => LiteralValue::Dynamic(DynamicReason::Expression),
	}
}

// ---------------------------------------------------------------------------
// The `?` operator — propagation of Result::Err / Option::None.
// ---------------------------------------------------------------------------

/// Error variants that `expr?` propagates out of the function: `Result::Err`,
/// `Option::None`, or a bare `Result`/`Option` enum name (no variants
/// enumerated — anything of theirs may propagate).
fn try_error_variants(variants: &[EnumVariant]) -> Vec<EnumVariant> {
	variants
		.iter()
		.filter(|v| {
			if v.is_bare_enum_name() {
				matches!(v.path[0].as_str(), "Result" | "Option")
			} else {
				matches!(
					v.path.last().map(String::as_str),
					Some("Err") | Some("None")
				)
			}
		})
		.cloned()
		.collect()
}

/// The type `expr?` evaluates to when it does not propagate — the inner value
/// of `Result::Ok`/`Option::Some`. `None` = cannot be determined statically.
pub(crate) fn try_success_type(variants: &[EnumVariant]) -> Option<TypeDef> {
	let success = variants
		.iter()
		.find(|v| matches!(v.path.last().map(String::as_str), Some("Ok") | Some("Some")))?;
	Some(match &success.inner {
		Some(inner) => (**inner).clone(),
		None => TypeDef::Unit,
	})
}

/// The return site through which `expr?` may leave the function (the error
/// branch), or `None` when the declared/literal type implies there is nothing
/// to propagate.
fn try_early_return_site(
	try_expr: &ast::ExprTry,
	source: &str,
	registry: &SignatureRegistry,
) -> Option<ReturnSite> {
	match classify_value(&try_expr.expr, source, registry) {
		LiteralValue::ResolvedCall {
			name,
			type_def: TypeDef::Enum(variants),
		} => {
			let errors = try_error_variants(&variants);
			if errors.is_empty() {
				None
			} else {
				Some(ReturnSite::ResolvedCall {
					name,
					type_def: TypeDef::Enum(errors),
				})
			}
		}
		// A declared non-enum type — `?` has no error branch to propagate.
		LiteralValue::ResolvedCall { .. } => None,
		LiteralValue::Enum { path, inner } => {
			match path.last().map(String::as_str) {
				// `Err(e)?` / `None?` propagates the literal as-is.
				Some("Err") | Some("None") => Some(ReturnSite::EnumLiteral { path, inner }),
				_ => None,
			}
		}
		// Unknown type — anything may propagate; statically unverifiable.
		LiteralValue::Dynamic(_) => Some(ReturnSite::TryPropagation {
			line: line_of(source, try_expr.try_token.span),
		}),
		// Other literals (String, int, …) — `?` on them has nothing to propagate.
		_ => None,
	}
}

/// The value of `expr?` in the success branch (unwrapped `Ok`/`Some`).
fn classify_try_value(
	inner: &ast::Expr,
	source: &str,
	registry: &SignatureRegistry,
) -> LiteralValue {
	match classify_value(inner, source, registry) {
		LiteralValue::ResolvedCall {
			name,
			type_def: TypeDef::Enum(variants),
		} => match try_success_type(&variants) {
			Some(type_def) => LiteralValue::ResolvedCall { name, type_def },
			None => LiteralValue::Dynamic(DynamicReason::TryPropagation),
		},
		LiteralValue::Enum { path, inner }
			if matches!(path.last().map(String::as_str), Some("Ok") | Some("Some")) =>
		{
			match inner {
				Some(value) => *value,
				None => LiteralValue::Unit,
			}
		}
		_ => LiteralValue::Dynamic(DynamicReason::TryPropagation),
	}
}

fn classify_lit(lit: &ast::Lit, source: &str) -> LiteralValue {
	match lit {
		ast::Lit::Bool(b) => LiteralValue::Bool(b.value),
		ast::Lit::Str(s) => LiteralValue::String(lit_str_value(source, s.span, &s.source)),
		ast::Lit::Number(n) => classify_number(n, source),
		_ => LiteralValue::Dynamic(DynamicReason::Expression),
	}
}

fn classify_number(n: &ast::LitNumber, source: &str) -> LiteralValue {
	let ast::NumberSource::Text(text) = n.source else {
		return LiteralValue::Dynamic(DynamicReason::Expression);
	};

	let digits = slice(source, text.number);

	if text.is_fractional {
		match digits.parse::<f64>() {
			Ok(v) => LiteralValue::Float(v),
			Err(_) => LiteralValue::Dynamic(DynamicReason::Expression),
		}
	} else {
		let radix = match text.base {
			ast::NumberBase::Binary => 2,
			ast::NumberBase::Octal => 8,
			ast::NumberBase::Hex => 16,
			ast::NumberBase::Decimal => 10,
			_ => 10,
		};
		let digits = digits
			.trim_start_matches("0b")
			.trim_start_matches("0o")
			.trim_start_matches("0x");
		match i64::from_str_radix(digits, radix) {
			Ok(v) => LiteralValue::Int(v),
			Err(_) => LiteralValue::Dynamic(DynamicReason::Expression),
		}
	}
}

fn classify_object(
	obj: &ast::ExprObject,
	source: &str,
	registry: &SignatureRegistry,
) -> LiteralValue {
	if !matches!(obj.ident, ast::ObjectIdent::Anonymous(_)) {
		return LiteralValue::Dynamic(DynamicReason::Expression);
	}

	let mut fields = Vec::new();
	for (assign, _) in obj.assignments.iter() {
		let key = match &assign.key {
			ast::ObjectKey::Path(path) => match path_segments(path, source) {
				Some(segs) if segs.len() == 1 => segs.into_iter().next().unwrap(),
				_ => return LiteralValue::Dynamic(DynamicReason::Expression),
			},
			ast::ObjectKey::LitStr(s) => lit_str_value(source, s.span, &s.source),
			_ => return LiteralValue::Dynamic(DynamicReason::Expression),
		};

		let value = match &assign.assign {
			Some((_, value_expr)) => classify_value(value_expr, source, registry),
			// Shorthand `{ x }` == `{ x: x }` — a reference to a variable.
			None => LiteralValue::Dynamic(DynamicReason::Variable(key.clone())),
		};

		fields.push((key, value));
	}

	LiteralValue::Object(fields)
}

fn classify_bare_path(path: &ast::Path, source: &str) -> LiteralValue {
	let Some(segments) = path_segments(path, source) else {
		return LiteralValue::Dynamic(DynamicReason::Expression);
	};

	if segments.len() == 1 {
		if segments[0] == "None" {
			return LiteralValue::Enum {
				path: vec!["Option".to_string(), "None".to_string()],
				inner: None,
			};
		}
		return LiteralValue::Dynamic(DynamicReason::Variable(segments[0].clone()));
	}

	LiteralValue::Dynamic(DynamicReason::Expression)
}

fn classify_call(call: &ast::ExprCall, source: &str, registry: &SignatureRegistry) -> LiteralValue {
	// A method call, e.g. `value.compute()` — the callee is a field access, not a path.
	if let ast::Expr::FieldAccess(fa) = call.expr.as_ref() {
		let method = match &fa.expr_field {
			ast::ExprField::Path(p) => path_segments(p, source)
				.and_then(|s| s.into_iter().next())
				.unwrap_or_default(),
			ast::ExprField::LitNumber(n) => slice(source, n.span).to_string(),
			_ => String::new(),
		};
		return LiteralValue::Dynamic(DynamicReason::MethodCall(method));
	}

	let ast::Expr::Path(path) = call.expr.as_ref() else {
		// An indirect call, e.g. `f(x)` where `f` is any other expression.
		return LiteralValue::Dynamic(DynamicReason::IndirectCall);
	};

	let Some(segments) = path_segments(path, source) else {
		return LiteralValue::Dynamic(DynamicReason::IndirectCall);
	};

	let joined = segments.join("::");

	if let Some(origin) = registry.signatures.get(&joined) {
		return LiteralValue::ResolvedCall {
			name: joined,
			type_def: origin.type_def().clone(),
		};
	}

	let args: Vec<LiteralValue> = call
		.args
		.iter()
		.map(|(e, _)| classify_value(e, source, registry))
		.collect();

	if args.len() <= 1 {
		if let Some(canonical) = canonical_builtin_variant(&segments) {
			return LiteralValue::Enum {
				path: canonical,
				inner: args.into_iter().next().map(Box::new),
			};
		}

		let looks_like_variant = segments
			.last()
			.and_then(|s| s.chars().next())
			.is_some_and(|c| c.is_uppercase());

		if looks_like_variant {
			return LiteralValue::Enum {
				path: segments,
				inner: args.into_iter().next().map(Box::new),
			};
		}
	}

	LiteralValue::Dynamic(DynamicReason::UnannotatedCall(joined))
}

fn canonical_builtin_variant(segments: &[String]) -> Option<Vec<String>> {
	if segments.len() != 1 {
		return None;
	}

	match segments[0].as_str() {
		"Ok" => Some(vec!["Result".to_string(), "Ok".to_string()]),
		"Err" => Some(vec!["Result".to_string(), "Err".to_string()]),
		"Some" => Some(vec!["Option".to_string(), "Some".to_string()]),
		"None" => Some(vec!["Option".to_string(), "None".to_string()]),
		_ => None,
	}
}

/// Splits a `Path` into text segments when it consists purely of identifiers
/// (no `Self`/`super`/`crate`/generics) with no leading/trailing `::`.
pub(crate) fn path_segments(path: &ast::Path, source: &str) -> Option<Vec<String>> {
	if path.global.is_some() || path.trailing.is_some() {
		return None;
	}

	let mut segments = vec![segment_ident(&path.first, source)?];
	for (_, seg) in &path.rest {
		segments.push(segment_ident(seg, source)?);
	}

	Some(segments)
}

fn segment_ident(segment: &ast::PathSegment, source: &str) -> Option<String> {
	match segment {
		ast::PathSegment::Ident(ident) => Some(slice(source, ident.span).to_string()),
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::types::{SignatureOrigin, TypeDef};

	fn sites(source: &str, fn_name: &str, registry: &SignatureRegistry) -> Vec<ReturnSite> {
		let file = parse_file(source).expect("parse");
		let item_fn = find_function(&file, source, fn_name).expect("function found");
		find_return_sites(item_fn, source, registry)
	}

	#[test]
	fn primitive_literal() {
		let source = r#"
            fn process(name, age, active) {
                return "ok";
            }
        "#;
		let registry = SignatureRegistry::default();
		let result = sites(source, "process", &registry);
		assert_eq!(
			result,
			vec![ReturnSite::PrimitiveLiteral(LiteralValue::String(
				"ok".to_string()
			))]
		);
	}

	#[test]
	fn object_literal() {
		let source = r#"
            fn process(input) {
                return #{ status: "ok", code: 42, active: true };
            }
        "#;
		let registry = SignatureRegistry::default();
		let result = sites(source, "process", &registry);
		assert_eq!(
			result,
			vec![ReturnSite::ObjectLiteral(vec![
				("status".to_string(), LiteralValue::String("ok".to_string())),
				("code".to_string(), LiteralValue::Int(42)),
				("active".to_string(), LiteralValue::Bool(true)),
			])]
		);
	}

	#[test]
	fn nested_object_literal() {
		let source = r#"
            fn process(input) {
                return #{ status: "ok", data: #{ id: 1, name: "foo" } };
            }
        "#;
		let registry = SignatureRegistry::default();
		let result = sites(source, "process", &registry);
		assert_eq!(
			result,
			vec![ReturnSite::ObjectLiteral(vec![
				("status".to_string(), LiteralValue::String("ok".to_string())),
				(
					"data".to_string(),
					LiteralValue::Object(vec![
						("id".to_string(), LiteralValue::Int(1)),
						("name".to_string(), LiteralValue::String("foo".to_string())),
					])
				),
			])]
		);
	}

	#[test]
	fn list_literal() {
		let source = r#"
            fn process(input) {
                return ["a", "b", "c"];
            }
        "#;
		let registry = SignatureRegistry::default();
		let result = sites(source, "process", &registry);
		assert_eq!(
			result,
			vec![ReturnSite::PrimitiveLiteral(LiteralValue::List(vec![
				LiteralValue::String("a".to_string()),
				LiteralValue::String("b".to_string()),
				LiteralValue::String("c".to_string()),
			]))]
		);
	}

	#[test]
	fn enum_variant_ok_err_short_names() {
		let source = r#"
            fn process(input) {
                if input == "" {
                    return Err("empty input");
                }
                return Ok(42);
            }
        "#;
		let registry = SignatureRegistry::default();
		let result = sites(source, "process", &registry);
		assert_eq!(
			result,
			vec![
				ReturnSite::EnumLiteral {
					path: vec!["Result".to_string(), "Err".to_string()],
					inner: Some(Box::new(LiteralValue::String("empty input".to_string()))),
				},
				ReturnSite::EnumLiteral {
					path: vec!["Result".to_string(), "Ok".to_string()],
					inner: Some(Box::new(LiteralValue::Int(42))),
				},
			]
		);
	}

	#[test]
	fn nullable_unit_branch() {
		let source = r#"
            fn process(input) {
                if input == "" {
                    return ();
                }
                return "result";
            }
        "#;
		let registry = SignatureRegistry::default();
		let result = sites(source, "process", &registry);
		assert_eq!(
			result,
			vec![
				ReturnSite::Unit,
				ReturnSite::PrimitiveLiteral(LiteralValue::String("result".to_string())),
			]
		);
	}

	#[test]
	fn implicit_tail_return_without_explicit_return_keyword() {
		let source = r#"
            fn process(name, age, active) {
                "ok"
            }
        "#;
		let registry = SignatureRegistry::default();
		let result = sites(source, "process", &registry);
		assert_eq!(
			result,
			vec![ReturnSite::PrimitiveLiteral(LiteralValue::String(
				"ok".to_string()
			))]
		);
	}

	#[test]
	fn dynamic_variable() {
		let source = r#"
            fn process(input) {
                let result = compute(input);
                return result;
            }
        "#;
		let registry = SignatureRegistry::default();
		let result = sites(source, "process", &registry);
		assert_eq!(
			result,
			vec![ReturnSite::Dynamic(DynamicReason::Variable(
				"result".to_string()
			))]
		);
	}

	#[test]
	fn dynamic_unannotated_call() {
		let source = r#"
            fn process(input) {
                return helper(input);
            }
        "#;
		let registry = SignatureRegistry::default();
		let result = sites(source, "process", &registry);
		assert_eq!(
			result,
			vec![ReturnSite::Dynamic(DynamicReason::UnannotatedCall(
				"helper".to_string()
			))]
		);
	}

	#[test]
	fn dynamic_method_call() {
		let source = r#"
            fn process(input) {
                return input.compute();
            }
        "#;
		let registry = SignatureRegistry::default();
		let result = sites(source, "process", &registry);
		assert_eq!(
			result,
			vec![ReturnSite::Dynamic(DynamicReason::MethodCall(
				"compute".to_string()
			))]
		);
	}

	#[test]
	fn dynamic_expression() {
		let source = r#"
            fn process(a, b) {
                return a + b;
            }
        "#;
		let registry = SignatureRegistry::default();
		let result = sites(source, "process", &registry);
		assert_eq!(result, vec![ReturnSite::Dynamic(DynamicReason::Expression)]);
	}

	#[test]
	fn resolved_call_via_registry() {
		let source = r#"
            fn process(input) {
                return helper(input);
            }
        "#;
		let mut registry = SignatureRegistry::default();
		registry.signatures.insert(
			"helper".to_string(),
			SignatureOrigin::Helper(TypeDef::Primitive(crate::types::PrimitiveType::String)),
		);
		let result = sites(source, "process", &registry);
		assert_eq!(
			result,
			vec![ReturnSite::ResolvedCall {
				name: "helper".to_string(),
				type_def: TypeDef::Primitive(crate::types::PrimitiveType::String),
			}]
		);
	}

	#[test]
	fn doc_comment_extraction() {
		let source = "/// @param name: String\n/// @return String\nfn process(name) {\n    return name;\n}\n";
		let file = parse_file(source).unwrap();
		let item_fn = find_function(&file, source, "process").unwrap();
		let doc = doc_comment_before(source, item_fn).unwrap();
		assert_eq!(doc, "@param name: String\n@return String");
	}

	#[test]
	fn doc_comment_extraction_pub_fn() {
		let source = "/// @param name: String\n/// @return String\npub fn process(name) {\n    return name;\n}\n";
		let file = parse_file(source).unwrap();
		let item_fn = find_function(&file, source, "process").unwrap();
		let doc = doc_comment_before(source, item_fn).unwrap();
		assert_eq!(doc, "@param name: String\n@return String");
	}

	#[test]
	fn doc_comment_extraction_pub_async_fn() {
		let source = "/// @return String\npub async fn process() {\n    return \"x\";\n}\n";
		let file = parse_file(source).unwrap();
		let item_fn = find_function(&file, source, "process").unwrap();
		let doc = doc_comment_before(source, item_fn).unwrap();
		assert_eq!(doc, "@return String");
	}

	#[test]
	fn block_doc_comment_extraction() {
		let source = "/**\n * @param name: String\n * @return String\n */\npub fn process(name) {\n    return name;\n}\n";
		let file = parse_file(source).unwrap();
		let item_fn = find_function(&file, source, "process").unwrap();
		let doc = doc_comment_before(source, item_fn).unwrap();
		assert_eq!(doc, "@param name: String\n@return String");
	}

	#[test]
	fn block_doc_comment_without_stars() {
		let source = "/**\n@param name: String\n@return String\n*/\nfn process(name) {\n    return name;\n}\n";
		let file = parse_file(source).unwrap();
		let item_fn = find_function(&file, source, "process").unwrap();
		let doc = doc_comment_before(source, item_fn).unwrap();
		assert_eq!(doc, "@param name: String\n@return String");
	}

	#[test]
	fn single_line_block_doc_comment() {
		let source = "/** @return String */\nfn process() {\n    return \"x\";\n}\n";
		let file = parse_file(source).unwrap();
		let item_fn = find_function(&file, source, "process").unwrap();
		let doc = doc_comment_before(source, item_fn).unwrap();
		assert_eq!(doc, "@return String");
	}

	#[test]
	fn plain_block_comment_is_not_doc() {
		let source = "/* @return String */\nfn process() {\n    return \"x\";\n}\n";
		let file = parse_file(source).unwrap();
		let item_fn = find_function(&file, source, "process").unwrap();
		assert!(doc_comment_before(source, item_fn).is_none());
	}

	#[test]
	fn block_doc_of_earlier_function_does_not_leak() {
		let source = "/** @return int */\nfn other() {\n    1\n}\n\n/* a note */\nfn process() {\n    return \"x\";\n}\n";
		let file = parse_file(source).unwrap();
		let item_fn = find_function(&file, source, "process").unwrap();
		assert!(doc_comment_before(source, item_fn).is_none());
	}

	#[test]
	fn function_not_found_returns_none() {
		let source = "fn process() { 1 }";
		let file = parse_file(source).unwrap();
		assert!(find_function(&file, source, "missing").is_none());
	}
}
