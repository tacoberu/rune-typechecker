//! The host system typically knows what signature it expects of the
//! validated function (how many parameters, what types, what return type).
//! `validate_script`, however, only verifies that the script honors *its own*
//! declared contract — it knows nothing about the host's expectation.
//!
//! `validate_script_against` additionally compares the script's contract with
//! the expected signature: mismatches end up in `report.contract_mismatches`
//! and drop `report.is_valid`. This also catches a silent typo like `@paran`,
//! which the doc-comment parser deliberately ignores (forward compatibility),
//! so the validation itself passes.
//!
//! The expected signature is written concisely with the `contract!` macro;
//! the type syntax is the same as in doc-comment contracts. For signatures
//! arriving at runtime (e.g. from configuration) there is `Contract::parse`,
//! which returns a `Result`.
//!
//! Parameter names are not compared — the script names them as it likes,
//! only the types at each position matter.

use rune_typechecker::{Contract, ScriptValidationReport, contract, validate_script_against};

fn main() {
	let expected = contract!(
		(sender: String, event: String, context: String) -> Status::Solved
	);

	matching_contract(&expected);
	typo_in_param_tag(&expected);
	wrong_param_type(&expected);
}

fn print_report(report: &ScriptValidationReport) {
	println!("is_valid = {}", report.is_valid);
	println!(
		"main.is_valid = {} (internal contract consistency)",
		report.main.is_valid
	);
	for mismatch in &report.contract_mismatches {
		println!("  -> {mismatch}");
	}
}

fn matching_contract(expected: &Contract) {
	println!("== 1) The script contract matches the expected signature ==");

	let source = r#"
        /// @param sender: String Who sent the message
        /// @param event: String
        /// @param context: String
        /// @return Status::Solved
        fn handler(sender, event, context) {
            Status::Solved
        }
    "#;

	let report =
		validate_script_against(source, "handler", expected, &[]).expect("validation failed");
	print_report(&report);
	println!();
}

fn typo_in_param_tag(expected: &Contract) {
	println!("== 2) Typo '@paran' — the contract itself passes, the signature does not match ==");

	let source = r#"
        /// @paran sender: String
        /// @paran event: String
        /// @paran context: String
        /// @return Status::Solved
        fn handler(sender, event, context) {
            Status::Solved
        }
    "#;

	let report =
		validate_script_against(source, "handler", expected, &[]).expect("validation failed");
	print_report(&report);
	println!();
}

fn wrong_param_type(expected: &Contract) {
	println!("== 3) The script declares a different param type than the host expects ==");

	let source = r#"
        /// @param sender: int
        /// @param event: String
        /// @param context: String
        /// @return Status::Solved
        fn handler(sender, event, context) {
            Status::Solved
        }
    "#;

	let report =
		validate_script_against(source, "handler", expected, &[]).expect("validation failed");
	print_report(&report);
}
