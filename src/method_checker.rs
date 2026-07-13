//! Existence check for instance method calls.
//!
//! Rune resolves method calls dynamically by the receiver's type, so the
//! compiler cannot catch a call to a method that does not exist — it fails
//! only at runtime. This pass walks the function body with a simple type
//! environment seeded from the contract's `@param` types and, for every
//! `receiver.method(...)` whose receiver type is statically known *and*
//! described in the host-supplied [`MethodRegistry`], verifies the method
//! exists.
//!
//! The design errs on the side of silence: any expression whose type cannot
//! be established (or a type the host did not describe) is skipped, so the
//! pass produces no false positives at the cost of false negatives.

use std::collections::HashMap;

use rune::ast;

use crate::ast_analyzer::{path_segments, try_success_type};
use crate::source_text::line_of;
use crate::types::{
	Contract, EnumVariant, MethodRegistry, MethodViolation, PrimitiveType, SignatureRegistry,
	TypeDef,
};

/// Variable name -> statically known type. A name absent from the map has an
/// unknown type (calls on it are skipped).
type Env = HashMap<String, TypeDef>;

struct Checker<'a> {
	source: &'a str,
	registry: &'a SignatureRegistry,
	methods: &'a MethodRegistry,
	violations: Vec<MethodViolation>,
}

pub fn check(
	item_fn: &ast::ItemFn,
	source: &str,
	contract: &Contract,
	registry: &SignatureRegistry,
	methods: &MethodRegistry,
) -> Vec<MethodViolation> {
	if methods.is_empty() {
		return Vec::new();
	}

	let mut env = Env::new();
	for ((arg, _), param) in item_fn.args.iter().zip(&contract.params) {
		if let ast::FnArg::Pat(pat) = arg {
			if let Some(name) = pat_single_ident(pat, source) {
				env.insert(name, param.type_def.clone());
			}
		}
	}

	let mut checker = Checker {
		source,
		registry,
		methods,
		violations: Vec::new(),
	};
	checker.walk_block(&item_fn.body, &mut env);
	checker.violations
}

impl Checker<'_> {
	// -----------------------------------------------------------------------
	// Walking. Every nested scope (branch, loop body, closure) works on a
	// clone of the environment; variables the scope re-assigns are afterwards
	// poisoned (dropped) in the outer environment, because we do not know
	// which branch ran.
	// -----------------------------------------------------------------------

	fn walk_block(&mut self, block: &ast::Block, env: &mut Env) {
		for stmt in &block.statements {
			match stmt {
				ast::Stmt::Local(local) => {
					self.walk_expr(&local.expr, env);
					let ty = self.infer(&local.expr, env);
					bind_pattern_static(&local.pat, ty, env, self.source);
				}
				ast::Stmt::Expr(expr) => self.walk_expr(expr, env),
				ast::Stmt::Semi(semi) => self.walk_expr(&semi.expr, env),
				_ => {}
			}
		}
	}

	/// Walks a nested scope on a cloned environment and poisons re-assigned
	/// outer variables afterwards.
	fn walk_scope_block(&mut self, block: &ast::Block, env: &mut Env, extra: impl FnOnce(&mut Env)) {
		let mut scope = env.clone();
		extra(&mut scope);
		self.walk_block(block, &mut scope);
		poison_assigned_in_block(block, self.source, env);
	}

	fn walk_expr(&mut self, expr: &ast::Expr, env: &mut Env) {
		match expr {
			ast::Expr::Call(call) => {
				for (arg, _) in call.args.iter() {
					self.walk_expr(arg, env);
				}
				match call.expr.as_ref() {
					ast::Expr::FieldAccess(fa) => {
						self.walk_expr(&fa.expr, env);
						self.check_method_call(call, fa, env);
					}
					other => self.walk_expr(other, env),
				}
			}
			ast::Expr::Block(b) => self.walk_scope_block(&b.block, env, |_| {}),
			ast::Expr::If(if_expr) => {
				self.walk_condition(&if_expr.condition, env);
				let cond_binding = self.condition_binding(&if_expr.condition, env);
				self.walk_scope_block(&if_expr.block, env, |scope| {
					if let Some((pat, ty)) = cond_binding {
						bind_pattern_static(pat, ty, scope, self.source);
					}
				});
				for else_if in &if_expr.expr_else_ifs {
					self.walk_condition(&else_if.condition, env);
					let binding = self.condition_binding(&else_if.condition, env);
					self.walk_scope_block(&else_if.block, env, |scope| {
						if let Some((pat, ty)) = binding {
							bind_pattern_static(pat, ty, scope, self.source);
						}
					});
				}
				if let Some(else_) = &if_expr.expr_else {
					self.walk_scope_block(&else_.block, env, |_| {});
				}
			}
			ast::Expr::Match(m) => {
				self.walk_expr(&m.expr, env);
				let scrutinee = self.infer(&m.expr, env);
				for (branch, _) in &m.branches {
					let mut scope = env.clone();
					bind_pattern_static(&branch.pat, scrutinee.clone(), &mut scope, self.source);
					if let Some((_, cond)) = &branch.condition {
						self.walk_expr(cond, &mut scope);
					}
					self.walk_expr(&branch.body, &mut scope);
					poison_assigned_in_expr(&branch.body, self.source, env);
				}
			}
			ast::Expr::Loop(l) => {
				poison_assigned_in_block(&l.body, self.source, env);
				self.walk_scope_block(&l.body, env, |_| {});
			}
			ast::Expr::While(w) => {
				self.walk_condition(&w.condition, env);
				poison_assigned_in_block(&w.body, self.source, env);
				let binding = self.condition_binding(&w.condition, env);
				self.walk_scope_block(&w.body, env, |scope| {
					if let Some((pat, ty)) = binding {
						bind_pattern_static(pat, ty, scope, self.source);
					}
				});
			}
			ast::Expr::For(f) => {
				self.walk_expr(&f.iter, env);
				poison_assigned_in_block(&f.body, self.source, env);
				let source = self.source;
				let binding = &f.binding;
				self.walk_scope_block(&f.body, env, |scope| {
					bind_pattern_static(binding, None, scope, source);
				});
			}
			ast::Expr::Closure(c) => {
				let mut scope = env.clone();
				if let ast::ExprClosureArgs::List { args, .. } = &c.args {
					for (arg, _) in args {
						if let ast::FnArg::Pat(pat) = arg {
							bind_pattern_static(pat, None, &mut scope, self.source);
						}
					}
				}
				self.walk_expr(&c.body, &mut scope);
				poison_assigned_in_expr(&c.body, self.source, env);
			}
			ast::Expr::Assign(a) => {
				self.walk_expr(&a.rhs, env);
				self.walk_expr(&a.lhs, env);
				if let ast::Expr::Path(p) = a.lhs.as_ref() {
					if let Some(name) = single_segment(p, self.source) {
						let ty = self.infer(&a.rhs, env);
						match ty {
							Some(ty) => env.insert(name, ty),
							None => env.remove(&name),
						};
					}
				}
			}
			ast::Expr::Return(ret) => {
				if let Some(inner) = &ret.expr {
					self.walk_expr(inner, env);
				}
			}
			ast::Expr::Binary(b) => {
				self.walk_expr(&b.lhs, env);
				self.walk_expr(&b.rhs, env);
			}
			ast::Expr::Unary(u) => self.walk_expr(&u.expr, env),
			ast::Expr::Group(g) => self.walk_expr(&g.expr, env),
			ast::Expr::Try(t) => self.walk_expr(&t.expr, env),
			ast::Expr::Await(a) => self.walk_expr(&a.expr, env),
			ast::Expr::FieldAccess(fa) => self.walk_expr(&fa.expr, env),
			ast::Expr::Index(idx) => {
				self.walk_expr(&idx.target, env);
				self.walk_expr(&idx.index, env);
			}
			ast::Expr::Object(obj) => {
				for (assign, _) in obj.assignments.iter() {
					if let Some((_, value)) = &assign.assign {
						self.walk_expr(value, env);
					}
				}
			}
			ast::Expr::Vec(v) => {
				for (item, _) in v.items.iter() {
					self.walk_expr(item, env);
				}
			}
			ast::Expr::Tuple(t) => {
				for (item, _) in t.items.iter() {
					self.walk_expr(item, env);
				}
			}
			ast::Expr::Break(b) => {
				if let Some(inner) = &b.expr {
					self.walk_expr(inner, env);
				}
			}
			ast::Expr::Yield(y) => {
				if let Some(inner) = &y.expr {
					self.walk_expr(inner, env);
				}
			}
			ast::Expr::Range(r) => {
				if let Some(from) = &r.start {
					self.walk_expr(from, env);
				}
				if let Some(to) = &r.end {
					self.walk_expr(to, env);
				}
			}
			_ => {}
		}
	}

	fn walk_condition(&mut self, condition: &ast::Condition, env: &mut Env) {
		match condition {
			ast::Condition::Expr(e) => self.walk_expr(e, env),
			ast::Condition::ExprLet(l) => self.walk_expr(&l.expr, env),
			_ => {}
		}
	}

	/// For `if let PAT = expr` returns the pattern together with the inferred
	/// type of `expr`, so the branch scope can bind it.
	fn condition_binding<'c>(
		&mut self,
		condition: &'c ast::Condition,
		env: &Env,
	) -> Option<(&'c ast::Pat, Option<TypeDef>)> {
		match condition {
			ast::Condition::ExprLet(l) => {
				let ty = self.infer(&l.expr, &mut env.clone());
				Some((&l.pat, ty))
			}
			_ => None,
		}
	}

	// -----------------------------------------------------------------------
	// The check itself.
	// -----------------------------------------------------------------------

	fn check_method_call(&mut self, call: &ast::ExprCall, fa: &ast::ExprFieldAccess, env: &mut Env) {
		/// Protocol-backed methods rune provides across types — never reported.
		const UNIVERSAL: [&str; 5] = ["clone", "eq", "ne", "cmp", "partial_cmp"];

		let ast::ExprField::Path(p) = &fa.expr_field else {
			return;
		};
		let Some(method) = single_segment(p, self.source) else {
			return;
		};
		if UNIVERSAL.contains(&method.as_str()) {
			return;
		}

		let Some(receiver_ty) = self.infer(&fa.expr, env) else {
			return;
		};
		let names = type_names(&receiver_ty);
		if names.is_empty() {
			return;
		}
		// Only judge when every candidate type is described by the host.
		if !names.iter().all(|n| self.methods.has_type(n)) {
			return;
		}
		if names.iter().any(|n| self.methods.lookup(n, &method).is_some()) {
			return;
		}
		self.violations.push(MethodViolation {
			receiver: names.join(" | "),
			method,
			line: line_of(self.source, call_span(call)),
		});
	}

	// -----------------------------------------------------------------------
	// Type inference for expressions. `None` = unknown, skip.
	// -----------------------------------------------------------------------

	fn infer(&mut self, expr: &ast::Expr, env: &mut Env) -> Option<TypeDef> {
		match expr {
			ast::Expr::Path(p) => {
				let segments = path_segments(p, self.source)?;
				match segments.as_slice() {
					[single] if single == "None" => Some(enum_of(vec![
						"Option".to_string(),
						"None".to_string(),
					])),
					[single] => env.get(single).cloned(),
					// `Status::Solved` — a bare variant path.
					_ if starts_uppercase(segments.last()) => Some(enum_of(segments)),
					_ => None,
				}
			}
			ast::Expr::Lit(lit_expr) => match &lit_expr.lit {
				ast::Lit::Str(_) => Some(TypeDef::Primitive(PrimitiveType::String)),
				ast::Lit::Bool(_) => Some(TypeDef::Primitive(PrimitiveType::Bool)),
				ast::Lit::Number(n) => match n.source {
					ast::NumberSource::Text(text) if text.is_fractional => {
						Some(TypeDef::Primitive(PrimitiveType::Float))
					}
					_ => Some(TypeDef::Primitive(PrimitiveType::Int)),
				},
				ast::Lit::ByteStr(_) => Some(TypeDef::Primitive(PrimitiveType::Bytes)),
				_ => None,
			},
			ast::Expr::Object(_) => Some(TypeDef::Object(Vec::new())),
			ast::Expr::Vec(_) => Some(TypeDef::List(Box::new(TypeDef::Unit))),
			ast::Expr::Group(g) => self.infer(&g.expr, env),
			ast::Expr::Try(t) => {
				let inner = self.infer(&t.expr, env)?;
				match inner {
					TypeDef::Enum(variants) => try_success_type(&variants),
					_ => None,
				}
			}
			ast::Expr::Call(call) => self.infer_call(call, env),
			_ => None,
		}
	}

	fn infer_call(&mut self, call: &ast::ExprCall, env: &mut Env) -> Option<TypeDef> {
		// Method call — return type from the method table.
		if let ast::Expr::FieldAccess(fa) = call.expr.as_ref() {
			let ast::ExprField::Path(p) = &fa.expr_field else {
				return None;
			};
			let method = single_segment(p, self.source)?;
			let receiver_ty = self.infer(&fa.expr, env)?;
			let names = type_names(&receiver_ty);
			// An unambiguous return type only when there is a single candidate.
			let [name] = names.as_slice() else {
				return None;
			};
			return self.methods.lookup(name, &method)?.clone();
		}

		let ast::Expr::Path(path) = call.expr.as_ref() else {
			return None;
		};
		let segments = path_segments(path, self.source)?;
		let joined = segments.join("::");

		// A helper or a host builtin with a known return type.
		if let Some(origin) = self.registry.signatures.get(&joined) {
			return Some(origin.type_def().clone());
		}

		match segments.as_slice() {
			[single] => match single.as_str() {
				"Ok" | "Err" => Some(enum_of(vec!["Result".to_string(), single.clone()])),
				"Some" | "None" => Some(enum_of(vec!["Option".to_string(), single.clone()])),
				_ => None,
			},
			// `Status::Solved(...)` — an enum variant; `Style::parse(...)` (a
			// lowercase tail) is an associated function with an unknown return.
			_ if starts_uppercase(segments.last()) => Some(enum_of(segments)),
			_ => None,
		}
	}
}

// ---------------------------------------------------------------------------
// Patterns.
// ---------------------------------------------------------------------------

fn bind_pattern_static(pat: &ast::Pat, ty: Option<TypeDef>, env: &mut Env, source: &str) {
	match pat {
		ast::Pat::Path(p) => {
			if let Some(name) = single_segment(&p.path, source) {
				match ty {
					Some(ty) => env.insert(name, ty),
					None => env.remove(&name),
				};
			}
		}
		ast::Pat::Ignore(_) | ast::Pat::Lit(_) | ast::Pat::Rest(_) => {}
		// `Some(x)` / `Ok(x)` — bind the unwrapped success type.
		ast::Pat::Tuple(t) if is_unwrap_tuple(t, source) => {
			let inner_ty = match &ty {
				Some(TypeDef::Enum(variants)) => try_success_type(variants),
				_ => None,
			};
			let items: Vec<_> = t.items.iter().collect();
			match items.as_slice() {
				[(inner_pat, _)] => bind_pattern_static(inner_pat, inner_ty, env, source),
				_ => {
					for (inner_pat, _) in items {
						bind_pattern_static(inner_pat, None, env, source);
					}
				}
			}
		}
		// Anything else — bind all contained identifiers as unknown.
		other => {
			for name in pat_idents(other, source) {
				env.remove(&name);
			}
		}
	}
}

/// `Some(...)` or `Ok(...)` tuple pattern.
fn is_unwrap_tuple(t: &ast::PatTuple, source: &str) -> bool {
	t.path
		.as_ref()
		.and_then(|p| path_segments(p, source))
		.is_some_and(|segs| matches!(segs.last().map(String::as_str), Some("Some") | Some("Ok")))
}

fn pat_single_ident(pat: &ast::Pat, source: &str) -> Option<String> {
	match pat {
		ast::Pat::Path(p) => single_segment(&p.path, source),
		_ => None,
	}
}

fn pat_idents(pat: &ast::Pat, source: &str) -> Vec<String> {
	let mut out = Vec::new();
	collect_pat_idents(pat, source, &mut out);
	out
}

fn collect_pat_idents(pat: &ast::Pat, source: &str, out: &mut Vec<String>) {
	match pat {
		ast::Pat::Path(p) => {
			if let Some(name) = single_segment(&p.path, source) {
				out.push(name);
			}
		}
		ast::Pat::Tuple(t) => {
			for (inner, _) in t.items.iter() {
				collect_pat_idents(inner, source, out);
			}
		}
		ast::Pat::Vec(v) => {
			for (inner, _) in v.items.iter() {
				collect_pat_idents(inner, source, out);
			}
		}
		ast::Pat::Object(o) => {
			for (inner, _) in o.items.iter() {
				collect_pat_idents(inner, source, out);
			}
		}
		ast::Pat::Binding(b) => collect_pat_idents(&b.pat, source, out),
		ast::Pat::Ignore(_) | ast::Pat::Lit(_) | ast::Pat::Rest(_) => {}
		_ => {}
	}
}

// ---------------------------------------------------------------------------
// Poisoning of re-assigned variables.
// ---------------------------------------------------------------------------

fn poison_assigned_in_block(block: &ast::Block, source: &str, env: &mut Env) {
	let mut names = Vec::new();
	collect_assigned_block(block, source, &mut names);
	for name in names {
		env.remove(&name);
	}
}

fn poison_assigned_in_expr(expr: &ast::Expr, source: &str, env: &mut Env) {
	let mut names = Vec::new();
	collect_assigned_expr(expr, source, &mut names);
	for name in names {
		env.remove(&name);
	}
}

fn collect_assigned_block(block: &ast::Block, source: &str, out: &mut Vec<String>) {
	for stmt in &block.statements {
		match stmt {
			ast::Stmt::Local(local) => collect_assigned_expr(&local.expr, source, out),
			ast::Stmt::Expr(expr) => collect_assigned_expr(expr, source, out),
			ast::Stmt::Semi(semi) => collect_assigned_expr(&semi.expr, source, out),
			_ => {}
		}
	}
}

fn collect_assigned_expr(expr: &ast::Expr, source: &str, out: &mut Vec<String>) {
	match expr {
		ast::Expr::Assign(a) => {
			if let ast::Expr::Path(p) = a.lhs.as_ref() {
				if let Some(name) = single_segment(p, source) {
					out.push(name);
				}
			}
			collect_assigned_expr(&a.rhs, source, out);
		}
		ast::Expr::Block(b) => collect_assigned_block(&b.block, source, out),
		ast::Expr::If(i) => {
			collect_assigned_block(&i.block, source, out);
			for else_if in &i.expr_else_ifs {
				collect_assigned_block(&else_if.block, source, out);
			}
			if let Some(else_) = &i.expr_else {
				collect_assigned_block(&else_.block, source, out);
			}
		}
		ast::Expr::Match(m) => {
			for (branch, _) in &m.branches {
				collect_assigned_expr(&branch.body, source, out);
			}
		}
		ast::Expr::Loop(l) => collect_assigned_block(&l.body, source, out),
		ast::Expr::While(w) => collect_assigned_block(&w.body, source, out),
		ast::Expr::For(f) => collect_assigned_block(&f.body, source, out),
		ast::Expr::Closure(c) => collect_assigned_expr(&c.body, source, out),
		ast::Expr::Group(g) => collect_assigned_expr(&g.expr, source, out),
		ast::Expr::Try(t) => collect_assigned_expr(&t.expr, source, out),
		ast::Expr::Binary(b) => {
			collect_assigned_expr(&b.lhs, source, out);
			collect_assigned_expr(&b.rhs, source, out);
		}
		ast::Expr::Call(call) => {
			collect_assigned_expr(&call.expr, source, out);
			for (arg, _) in call.args.iter() {
				collect_assigned_expr(arg, source, out);
			}
		}
		_ => {}
	}
}

// ---------------------------------------------------------------------------
// Type names.
// ---------------------------------------------------------------------------

/// Candidate receiver-type names of a declared type, as the method table keys
/// them. An empty result = nothing to check on (e.g. unit).
fn type_names(ty: &TypeDef) -> Vec<String> {
	match ty {
		TypeDef::Primitive(PrimitiveType::String) => vec!["String".to_string()],
		TypeDef::Primitive(PrimitiveType::Int) => vec!["i64".to_string()],
		TypeDef::Primitive(PrimitiveType::Float) => vec!["f64".to_string()],
		TypeDef::Primitive(PrimitiveType::Bool) => vec!["bool".to_string()],
		TypeDef::Primitive(PrimitiveType::Bytes) => vec!["Bytes".to_string()],
		TypeDef::Object(_) => vec!["Object".to_string()],
		TypeDef::List(_) => vec!["Vec".to_string()],
		TypeDef::Unit => Vec::new(),
		TypeDef::Nullable(inner) => type_names(inner),
		TypeDef::Enum(variants) => {
			let mut names: Vec<String> = Vec::new();
			for v in variants {
				let name = canonical_enum_name(&v.path);
				if !names.contains(&name) {
					names.push(name);
				}
			}
			names
		}
	}
}

/// `Ok`/`Err` belong to `Result`, `Some`/`None` to `Option`; anything else is
/// named by the first path segment (`Status::Solved` -> `Status`).
fn canonical_enum_name(path: &[String]) -> String {
	match path.first().map(String::as_str) {
		Some("Ok") | Some("Err") => "Result".to_string(),
		Some("Some") | Some("None") => "Option".to_string(),
		Some(first) => first.to_string(),
		None => String::new(),
	}
}

fn enum_of(path: Vec<String>) -> TypeDef {
	TypeDef::Enum(vec![EnumVariant { path, inner: None }])
}

fn starts_uppercase(segment: Option<&String>) -> bool {
	segment
		.and_then(|s| s.chars().next())
		.is_some_and(|c| c.is_uppercase())
}

fn single_segment(path: &ast::Path, source: &str) -> Option<String> {
	let segments = path_segments(path, source)?;
	match segments.as_slice() {
		[single] => Some(single.clone()),
		_ => None,
	}
}

fn call_span(call: &ast::ExprCall) -> rune::ast::Span {
	use rune::ast::Spanned;
	call.span()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ast_analyzer::{find_function, parse_file};
	use crate::types::MethodSignature;

	fn registry() -> MethodRegistry {
		MethodRegistry::new([
			MethodSignature::new(
				"Sender",
				"name",
				Some(TypeDef::Primitive(PrimitiveType::String)),
			),
			MethodSignature::new("Sender", "fullname", None),
			MethodSignature::new("String", "to_uppercase", None),
			MethodSignature::new(
				"AppContext",
				"lookup",
				Some(TypeDef::parse("Option::Some(ComponentContext) | Option::None").unwrap()),
			),
			MethodSignature::new("ComponentContext", "set_value", None),
			MethodSignature::new("Option", "unwrap_or", None),
			MethodSignature::new("Status", "dummy", None),
		])
	}

	fn violations(source: &str) -> Vec<MethodViolation> {
		let file = parse_file(source).expect("parse");
		let item_fn = find_function(&file, source, "handler").expect("fn");
		let doc = crate::ast_analyzer::doc_comment_before(source, item_fn).expect("doc");
		let contract = crate::doc_comment::parse(&doc).expect("contract");
		let sig_registry = SignatureRegistry::default();
		check(item_fn, source, &contract, &sig_registry, &registry())
	}

	#[test]
	fn missing_method_on_param_is_reported() {
		let out = violations(
			"/// @param sender: Sender\n/// @return String\nfn handler(sender) {\n\tsender.neexistuje()\n}\n",
		);
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].receiver, "Sender");
		assert_eq!(out[0].method, "neexistuje");
		assert_eq!(out[0].line, 4);
	}

	#[test]
	fn existing_method_passes() {
		let out = violations(
			"/// @param sender: Sender\n/// @return String\nfn handler(sender) {\n\tsender.name()\n}\n",
		);
		assert!(out.is_empty());
	}

	#[test]
	fn chained_call_is_checked() {
		// name() -> String; String nemá `to_uppercasee`.
		let out = violations(
			"/// @param sender: Sender\n/// @return String\nfn handler(sender) {\n\tsender.name().to_uppercasee()\n}\n",
		);
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].receiver, "String");
		assert_eq!(out[0].method, "to_uppercasee");
	}

	#[test]
	fn let_binding_carries_type() {
		let out = violations(
			"/// @param sender: Sender\n/// @return String\nfn handler(sender) {\n\tlet n = sender.name();\n\tn.wrong()\n}\n",
		);
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].receiver, "String");
	}

	#[test]
	fn if_let_unwraps_success_type() {
		let src = "/// @param context: AppContext\n/// @return ()\nfn handler(context) {\n\tif let Some(c) = context.lookup(\"x\") {\n\t\tc.wrong();\n\t\tc.set_value(1);\n\t}\n}\n";
		let out = violations(src);
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].receiver, "ComponentContext");
		assert_eq!(out[0].method, "wrong");
	}

	#[test]
	fn union_needs_method_on_no_member_to_report() {
		// Návrat helperu není v registru — tady simulujeme unii kontraktem parametru.
		let src = "/// @param x: Status::Solved | Option::None\n/// @return ()\nfn handler(x) {\n\tx.unwrap_or(1);\n\tx.wrong()\n}\n";
		let out = violations(src);
		// unwrap_or existuje na Option -> ok; wrong neexistuje na Status ani Option.
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].method, "wrong");
	}

	#[test]
	fn unknown_receiver_type_is_skipped() {
		let out = violations(
			"/// @param x: UnknownType\n/// @return ()\nfn handler(x) {\n\tx.whatever()\n}\n",
		);
		assert!(out.is_empty());
	}

	#[test]
	fn reassigned_in_branch_poisons_outer_type() {
		// Ve větvi se `n` přepíše na neznámý typ — po větvi už nesmíme soudit.
		let src = "/// @param sender: Sender\n/// @param c: bool\n/// @return ()\nfn handler(sender, c) {\n\tlet n = sender.name();\n\tif c {\n\t\tn = unknown();\n\t}\n\tn.wrong()\n}\n";
		let out = violations(src);
		assert!(out.is_empty(), "po přiřazení ve větvi je typ neznámý: {:?}", out);
	}

	#[test]
	fn closure_param_shadows_type() {
		let src = "/// @param sender: Sender\n/// @return ()\nfn handler(sender) {\n\tlet f = |sender| sender.wrong();\n\tf(1)\n}\n";
		let out = violations(src);
		assert!(out.is_empty(), "parametr closury stíní typ: {:?}", out);
	}

	#[test]
	fn method_inside_closure_is_checked() {
		let src = "/// @param sender: Sender\n/// @return ()\nfn handler(sender) {\n\tlet f = || sender.wrong();\n\tf()\n}\n";
		let out = violations(src);
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].method, "wrong");
	}

	#[test]
	fn universal_protocol_methods_pass() {
		let out = violations(
			"/// @param sender: Sender\n/// @return ()\nfn handler(sender) {\n\tsender.clone();\n\tsender.eq(sender)\n}\n",
		);
		assert!(out.is_empty());
	}

	#[test]
	fn try_unwrap_carries_inner_type() {
		let src = "/// @param context: AppContext\n/// @return Option\nfn handler(context) {\n\tlet c = context.lookup(\"x\")?;\n\tc.wrong();\n\tSome(1)\n}\n";
		let out = violations(src);
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].receiver, "ComponentContext");
	}

	#[test]
	fn match_arm_unwrap_binding() {
		let src = "/// @param context: AppContext\n/// @return ()\nfn handler(context) {\n\tmatch context.lookup(\"x\") {\n\t\tSome(c) => c.wrong(),\n\t\t_ => {},\n\t}\n}\n";
		let out = violations(src);
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].receiver, "ComponentContext");
	}
}
