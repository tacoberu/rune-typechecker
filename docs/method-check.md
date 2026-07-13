# Method existence check

Rune resolves instance method calls (`receiver.method(...)`) dynamically by
the receiver's runtime type. The compiler therefore cannot catch a call to a
method that does not exist — unlike a free function or a path
(`missing_fn()`, `Status::Missing`), which fail at build time with
`Missing item`. A typo in a method name used to surface only at runtime,
as `missing instance function`.

The checker closes this gap statically: the host describes the API its rune
`Context` registers (an [`Environment`](#environment)), and the checker
walks the function body with a simple type environment. Every method call
whose receiver type it can establish — *and* whose type the host described —
is verified to exist. Failures are reported as
`ValidationReport::method_violations` and drop `is_valid`.

## Design principle: no false positives

The pass errs on the side of silence. A call is **skipped** (never
reported) when:

- the receiver's type cannot be established statically,
- the receiver's type is not described in the host's method table
  (`MethodRegistry::has_type` is false for any candidate),
- the method is one of the protocol-backed universals rune provides across
  types (`clone`, `eq`, `ne`, `cmp`, `partial_cmp`).

The price is false negatives: what the checker cannot see, it does not
judge. It never reports a call that would work at runtime.

## Type environment and inference

The environment maps variable names to declared types
(the contract [`TypeDef`]s). It is seeded from the `@param` declarations of
the checked function and updated while walking the body:

| construct | effect |
|---|---|
| `let x = expr;` | binds `x` to the inferred type of `expr` (or unknown) |
| `x = expr;` | rebinds; inside a branch/loop/closure it *poisons* `x` to unknown in the outer scope (we do not know whether the branch ran) |
| `if let Some(x) = expr` / `Ok(x)` | binds `x` to the unwrapped success type of `expr` |
| `match expr { Some(x) => ... }` | the same, per arm |
| `expr?` | evaluates to the unwrapped `Ok`/`Some` type of `expr` |
| `for x in ...`, closure params | bind as unknown (shadowing a typed outer name) |

Expression types are inferred from: parameter types, literals
(`"s"` → `String`, `#{}` → `Object`, `[..]` → `Vec`, numbers, bools),
enum variant paths and constructors (`Status::Solved`, `Some(x)`),
helper/builtin calls with known signatures, and — crucially for chaining —
the **declared return types of methods** in the table
(`sender.name()` → `String`, so `sender.name().to_uppercasee()` is checked
against `String`).

A declared union (`WindowContext | ComponentContext`) is judged as a whole:
the call is reported only when the method exists on **no** member — if any
member has it, the call may be valid and stays silent.

## Environment

```rust
use rune_typechecker::{
	Environment, MethodRegistry, contract, methods,
	std_methods, validate_script_against_env,
};

// Standard library — pick exactly the modules the host installs into its
// rune::Context (tables mirror `rune::modules::*`); or take everything at
// once with `rune_std_methods()` when using `Context::with_default_modules()`.
let mut table = std_methods::string();
table.extend(std_methods::vec());
table.extend(std_methods::iter());

// The host's own types, written with the `methods!` macro in the same
// notation contracts use. A return type enables chained inference; an
// entry without `->` means the method exists, return type unknown.
table.extend(methods![
	Sender::name() -> String;
	AppContext::lookup() -> Option::Some(ComponentContext) | Option::None;
	ComponentContext::set_value();
]);

let env = Environment {
	builtins: Vec::new(),
	methods: MethodRegistry::new(table),
};

let expected = contract!((sender: Sender, context: AppContext) -> Status::Solved);
let report = validate_script_against_env(source, "handler", &expected, &env)?;
for v in &report.main.method_violations {
	println!("{v}");   // unknown method `does_not_exist` on `Sender` (line 6)
}
```

Entries in `methods!` are separated by `;`, because return types may
contain commas (`{ count: int, label: String }`). For tables assembled at
runtime there are `MethodSignature::new` and `TypeDef::parse`, which return
a `Result` instead of panicking.

A runnable version with more scenarios: `cargo run --example method_check`.

### `std_methods` — the standard library, per module

The tables are **generated** from an installed
`Context::with_default_modules()` via the `rune doc` CLI (rune 0.14.2) and
grouped by the module that registers the items: `string()`, `bytes()`,
`vec()`, `object()`, `tuple()`, `hash_map()`, `hash_set()`, `vec_deque()`,
`char()`, `i64()`, `u64()`, `f64()`, `option()`, `result()`, `iter()`,
`ops()`, `generator()`, `mem()`, `test()`, `stream()`. Modules with no
instance methods (`fmt`, `io`, `cmp`, …) have no table.

Two placement decisions mirror runtime semantics:

- **`iter()` carries the `Iterator`/`DoubleEndedIterator`/
  `ExactSizeIterator` trait methods** for all iterator-like types, whichever
  module defines them (`Chars` from `std::string`, `Iter` from `std::vec`,
  …) — without `std::iter` installed, `.map()` does not exist at runtime
  either.
- **`std::slice` is folded into `vec()`** — its only type (`slice::Iter`)
  is produced by `vec.iter()`.

## Limitations

- Only method **calls** are checked; field access (`value.prop`) and index
  operations are not.
- `?` and `if let Some(x)` unwrap the **first** `Ok`/`Some` variant of the
  declared type; a bare `Option`/`Result` (no variants) unwraps to unknown.
- Values with undeclarable types (e.g. a component's `value()`) are
  unknown — calls on them are not judged.
- The host tables must be kept in sync with what the host actually
  registers; a forgotten entry causes calls on that type to be skipped
  (missing type) or falsely reported (missing method on a described type).
  Keep an integration test that validates real scripts against the real
  environment.
