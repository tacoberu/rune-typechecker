//! Rune resolves instance method calls dynamically by the receiver's type,
//! so the compiler cannot catch `value.does_not_exist()` — it fails only at
//! runtime. When the host describes its API in an [`Environment`] (a method
//! table of receiver types), the checker verifies every method call whose
//! receiver type it can establish statically: from the contract's `@param`
//! types, `let` bindings, `if let Some(x) = ...` unwraps and the return
//! types of chained methods.
//!
//! The check is deliberately silent where the type is unknown or the host
//! did not describe it — false negatives are preferred over false positives.
//!
//! The standard library tables in [`std_methods`] are grouped by rune module
//! (mirroring `rune::modules::*`): a host that installs only some modules
//! into its `rune::Context` assembles just the matching subset;
//! [`rune_std_methods`] is the aggregate for `Context::with_default_modules()`.

use rune_typechecker::{
	Environment, MethodRegistry, ScriptValidationReport, contract, methods, std_methods,
	validate_script,
};

/// The host describes what its rune context makes available to scripts.
fn host_environment() -> Environment {
	// Standard library — only the modules the host actually installs.
	let mut table = std_methods::string();
	table.extend(std_methods::vec());
	table.extend(std_methods::object());
	table.extend(std_methods::option());
	table.extend(std_methods::iter());

	// The host's own types, in the same notation contracts use. A return
	// type enables chained inference; an entry without `->` means the
	// method exists, but its return type is unknown.
	table.extend(methods![
		Sender::name() -> String;
		Sender::fullname() -> String;
		AppContext::lookup() -> Option::Some(ComponentContext) | Option::None;
		ComponentContext::set_value();
	]);

	Environment {
		builtins: Vec::new(),
		methods: MethodRegistry::new(table),
	}
}

fn main() {
	let env = host_environment();
	let expected = contract!(
		(sender: Sender, context: AppContext) -> Status::Solved | Status::Continue
	);

	missing_method_on_host_type(&expected, &env);
	typo_in_chained_std_method(&expected, &env);
	unwrapped_binding_is_tracked(&expected, &env);
	unknown_receiver_is_skipped(&expected, &env);
}

fn print_report(report: &ScriptValidationReport) {
	println!("is_valid = {}", report.is_valid);
	for violation in &report.main.method_violations {
		println!("  -> {violation}");
	}
	for (name, helper) in &report.helpers {
		for violation in &helper.method_violations {
			println!("  -> in helper `{name}`: {violation}");
		}
	}
}

fn missing_method_on_host_type(expected: &rune_typechecker::Contract, env: &Environment) {
	println!("== 1) A method that does not exist on a host type ==");

	let source = r#"
        /// @param sender: Sender
        /// @param context: AppContext
        /// @return Status::Solved | Status::Continue
        fn handler(sender, context) {
            sender.does_not_exist();
            Status::Solved
        }
    "#;

	let report = validate_script(source, "handler", Some(expected), env)
		.expect("validation failed");
	print_report(&report);
	println!();
}

fn typo_in_chained_std_method(expected: &rune_typechecker::Contract, env: &Environment) {
	println!("== 2) A typo in a chained std method: name() -> String ==");

	let source = r#"
        /// @param sender: Sender
        /// @param context: AppContext
        /// @return Status::Solved | Status::Continue
        fn handler(sender, context) {
            let shout = sender.name().to_uppercasee();
            Status::Solved
        }
    "#;

	let report = validate_script(source, "handler", Some(expected), env)
		.expect("validation failed");
	print_report(&report);
	println!();
}

fn unwrapped_binding_is_tracked(expected: &rune_typechecker::Contract, env: &Environment) {
	println!("== 3) `if let Some(x)` unwraps the declared return type ==");

	let source = r#"
        /// @param sender: Sender
        /// @param context: AppContext
        /// @return Status::Solved | Status::Continue
        fn handler(sender, context) {
            if let Some(note) = context.lookup("main/note") {
                note.set_value("ok");     // exists on ComponentContext
                note.set_valeu("typo");   // does not
                return Status::Solved;
            }
            Status::Continue
        }
    "#;

	let report = validate_script(source, "handler", Some(expected), env)
		.expect("validation failed");
	print_report(&report);
	println!();
}

fn unknown_receiver_is_skipped(expected: &rune_typechecker::Contract, env: &Environment) {
	println!("== 4) An unknown receiver type is skipped (no false positives) ==");

	let source = r#"
        /// @param sender: Sender
        /// @param context: AppContext
        /// @return Status::Solved | Status::Continue
        fn handler(sender, context) {
            let value = mystery();        // return type unknown
            value.whatever_this_is();     // not judged
            Status::Solved
        }
    "#;

	let report = validate_script(source, "handler", Some(expected), env)
		.expect("validation failed");
	print_report(&report);
}
