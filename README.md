Rune TypeChecker
================

I have Rune in my system.
Users write scripts that I then process.
The problem is that a user can completely ignore the contract: the function they wrote returns values it should not,
values we never agreed on, and I have no way to check that what they wrote is correct.
Naturally, I want to check it before I allow the script to be saved.


I'm considering writing a checker that verifies a user-written function satisfies its contract.
I would use typed doc-comments for it —
the way it is done in PHP with phpstan, for example.


What it checks
--------------

- **The function honors its own contract** — a doc-comment with
  `@param`/`@return` (line `///` or block `/** */` style), verified against
  every return site of the body, including the hidden early returns of the
  `?` operator. See [`docs/spec-v0.1.md`](docs/spec-v0.1.md).
- **Helpers with contracts, recursively** — a helper reached from the
  contracted function is verified against its own `@return`; a broken
  helper fails the whole script.
- **The contract matches the host's expected signature** (the `expected`
  parameter) — catches silent typos like `@paran` that the doc-comment
  parser deliberately ignores.
- **Called instance methods exist** — rune dispatches
  `receiver.method(...)` dynamically, so a typo fails only at runtime;
  when the host describes its API in an `Environment`, the checker
  verifies the call statically wherever the receiver type is known.
  See [`docs/method-check.md`](docs/method-check.md).


Usage
-----

```rust
use rune_typechecker::{
	Environment, MethodRegistry, contract, methods, std_methods,
	validate_script,
};

let source = r#"
    /// @param sender: Sender
    /// @return Status::Solved | Status::Continue
    fn handler(sender) {
        let shout = sender.name().to_uppercasee();  // typo!
        Status::Solved
    }
"#;

// What the host's rune context provides: std modules it installs
// (tables mirror `rune::modules::*`; `rune_std_methods()` = all of them)
// plus the host's own types, in the same notation contracts use.
let mut table = std_methods::string();
table.extend(std_methods::iter());
table.extend(methods![
	Sender::name() -> String;
	Sender::fullname() -> String;
]);

let env = Environment {
	builtins: Vec::new(),
	methods: MethodRegistry::new(table),
};

// What the host expects of the entry function. `None` would skip the
// signature comparison; `&Environment::default()` an environment with no
// builtins and no method table.
let expected = contract!((sender: Sender) -> Status::Solved | Status::Continue);

let report = validate_script(source, "handler", Some(&expected), &env).unwrap();
assert!(!report.is_valid);
for m in &report.contract_mismatches {
	println!("{m}");
}
for v in &report.main.method_violations {
	println!("{v}");   // unknown method `to_uppercasee` on `String` (line 5)
}
```

Runnable examples with more scenarios:

```sh
cargo run --example basic
cargo run --example expected_signature
cargo run --example method_check
```


Documentation
-------------

- [`docs/spec-v0.1.md`](docs/spec-v0.1.md) — contract syntax, architecture,
  function groups, public API.
- [`docs/method-check.md`](docs/method-check.md) — the method existence
  check: type inference rules, `Environment`, per-module `std_methods`
  tables, limitations.
- [`docs/future-type-inference.md`](docs/future-type-inference.md),
  [`docs/future-runtime-verifier.md`](docs/future-runtime-verifier.md) —
  deferred extensions.
