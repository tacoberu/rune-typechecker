//! Demonstrates the public `rune_typechecker` API on three scenarios:
//! 1) a script that honors its contract (incl. a helper function),
//! 2) a script that breaks the contract directly (the README motivation),
//! 3) a script where only the called helper breaks the contract.

use rune_typechecker::{BuiltinSignature, Environment, PrimitiveType, TypeDef, validate_script};

fn main() {
	honest_script_with_helper();
	broken_contract();
	helper_breaks_its_own_contract();
	trusted_builtin();
	my_test_1();
}

fn honest_script_with_helper() {
	println!("== 1) Honest script with a helper function ==");

	let source = r#"
        /// @param name: String
        /// @return String
        fn greeting(name) {
            return format_greeting(name);
        }

        /// @param name: String
        /// @return String
        fn format_greeting(name) {
            return "Hello!";
        }
    "#;

	let report = validate_script(source, "greeting", None, &Environment::default()).expect("validation failed");

	println!("is_valid = {}", report.is_valid);
	println!(
		"main: verified={}, unverifiable={}, violations={}",
		report.main.static_result.verified.len(),
		report.main.static_result.unverifiable.len(),
		report.main.static_result.violations.len()
	);
	for (name, helper) in &report.helpers {
		println!("helper '{name}': is_valid={}", helper.is_valid);
	}
	println!();
}

fn broken_contract() {
	println!("== 2) Script breaking its own contract ==");

	let source = r#"
        /// @return String
        fn process(input) {
            return 42;
        }
    "#;

	let report = validate_script(source, "process", None, &Environment::default()).expect("validation failed");

	println!("is_valid = {}", report.is_valid);
	for violation in &report.main.static_result.violations {
		println!("violation: {}", violation.actual);
	}
	println!();
}

fn helper_breaks_its_own_contract() {
	println!("== 3) Helper function breaks its own contract ==");

	let source = r#"
        /// @return int
        fn parse_amount() {
            return "not a number";
        }

        /// @return int
        fn process() {
            return parse_amount();
        }
    "#;

	let report = validate_script(source, "process", None, &Environment::default()).expect("validation failed");

	println!("is_valid = {} (whole script)", report.is_valid);
	println!(
		"main.is_valid = {} (the call itself is type-correct)",
		report.main.is_valid
	);
	let helper = &report.helpers["parse_amount"];
	println!("helpers['parse_amount'].is_valid = {}", helper.is_valid);
	for violation in &helper.static_result.violations {
		println!("  -> {}", violation.actual);
	}
	println!();
}

fn trusted_builtin() {
	println!("== 4) Builtin function — the declared signature is trusted ==");

	let source = r#"
        /// @return String
        fn fetch_title() {
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

	let report = validate_script(source, "fetch_title", None, &env).expect("validation failed");

	println!("is_valid = {}", report.is_valid);
	println!(
		"helpers checked = {} (builtins are not re-verified)",
		report.helpers.len()
	);
}

fn my_test_1() {
	println!("== 5)  ==");

	let source = r#"
/// @paran sender String
/// @paran event String
/// @paran context String
/// @return Status::Solved
fn handler(sender, event, context) {

	println!("Hi");

	Status::Solved
}

	"#;

	//~ let report = validate_script(source, "fetch_title", None, &env).expect("validation failed");
	let report = validate_script(source, "handler", None, &Environment::default()).expect("validation failed");

	println!("is_valid = {}", report.is_valid);
	println!(
		"helpers checked = {} (builtins are not re-verified)",
		report.helpers.len()
	);
}
