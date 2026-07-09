# Rune Contract Checker — Specification

## Overview

A system for static verification of user-written Rune scripts before they are saved. The user declares a contract using doc-comment annotations above a function. The checker verifies that the implementation satisfies the contract wherever that is statically possible.

Runtime verification with mock inputs (running the function in a sandbox) is out of scope in this version — see [`docs/future-runtime-verifier.md`](./future-runtime-verifier.md). Type inference (variable tracking, expression typing) is out of scope as well — `AstAnalyzer` is however designed so that this extension can be added incrementally, see [`docs/future-type-inference.md`](./future-type-inference.md).

---

## Contract — doc-comment syntax

The contract is written as a doc-comment above the contracted function — either line style (`///`) or block style (`/** ... */`, decorative `*` at line starts are ignored). Inspired by PHPStan/JSDoc.

```rune
/**
 * @param name: String
 * @return String
 */
fn process(name) {
    return "ok";
}
```

The type may be followed on the line by an optional human description — it does not affect the contract:

```rune
/// @param sender: String Who sent the message
/// @return Status::Solved Processing outcome
```

### Primitive types

```rune
/// @param name: String
/// @param age: int
/// @param active: bool
/// @return String
fn process(name, age, active) {
    return "ok";
}
```

### Struct / object shape

```rune
/// @return { status: String, code: int, active: bool }
fn process(input) {
    return #{ status: "ok", code: 42, active: true };
}
```

### Enum variant

```rune
/// @return Result::Ok(int) | Result::Err(String)
fn process(input) {
    if input == "" {
        return Err("empty input");
    }
    return Ok(42);
}
```

A bare enum name (with no enumerated variants) matches any of its variants —
`@return Status` accepts `Status::Solved` as well as the union
`Status::Solved | Status::Continue`. Once variants are enumerated, they are
compared exactly.

```rune
/// @return Status
fn process(input) {
    Status::Solved
}
```

### Nested types

```rune
/// @return { status: String, data: { id: int, name: String } }
fn process(input) {
    return #{ status: "ok", data: #{ id: 1, name: "foo" } };
}
```

### List

```rune
/// @return [String]
fn process(input) {
    return ["a", "b", "c"];
}
```

Can be combined with the other types, e.g. `{ items: [int] }` or `[{ id: int, name: String }]`.

### Nullable / optional

```rune
/// @return String | ()
fn process(input) {
    if input == "" {
        return ();
    }
    return "result";
}
```

### The `?` operator

`expr?` is a hidden early return of the error variant (`Result::Err` /
`Option::None`). The checker captures it as a separate return site:

- If the expression's type is declared (a call to a helper/builtin with a
  contract), the propagated error variants are verified against the calling
  function's contract — the contract must therefore admit `Result::Err(...)`
  (resp. `Option::None`).
- The value of `expr?` in the success branch is the unwrapped content of
  `Result::Ok` / `Option::Some`.
- On an expression of unknown type, `?` always propagates `None` or
  `Err(...)` — if the contract admits neither (no `Err`/`None` variant nor a
  whole `Result`/`Option`), it is a definite contract violation. If the
  contract admits it, it is a statically unverifiable site
  (`ReturnSite::TryPropagation { line }` — with the line number for reporting).

```rune
/// @param input: String
/// @return Result::Ok(int) | Result::Err(String)
fn process(input) {
    let value = parse(input)?;   // may propagate Result::Err(String)
    return Ok(value);
}
```

---

## Function groups

Static checking distinguishes three groups of functions, based on where their return type is known from:

1. **The contracted function** — the function passed to `validate_script` as `function_name`. Its contract is mandatory; if the doc-comment is missing, `CheckerError::NoDocComment` is returned.
2. **Helper functions** — the other `fn`s defined by the user in the same script. Annotations (`///`) are optional for them — the user decorates them if they want the checker to be able to rely on them when resolving calls from the contracted function.
3. **Builtin functions** — native functions available to the user script (registered in the Rune `Context` by the host system) that are not written in Rune and therefore have no doc-comment. Their signatures are supplied to the checker externally by the host system, not parsed from the script.

When `AstAnalyzer` encounters a return value that is a direct function call (`return helper(x)`, or an object field `code: helper()`), it tries to look up the function name in the **`SignatureRegistry`** (the merge of groups 2 and 3 — see the component below). If found, the called function's return type is statically known and is compared against the contract just like a literal. If not found (an unannotated helper), the return site stays `Dynamic`.

`Dynamic` is not only a consequence of a missing annotation, though — `AstAnalyzer` attempts the registry lookup only for a direct call by name. A local variable, an indirect/computed call, a method on a value, or any other expression stays `Dynamic` no matter how thoroughly the other functions are annotated. Each of these cases has its own `DynamicReason` (see `AstAnalyzer` below) — this version does nothing further with it, but it is groundwork for future type inference, see [`docs/future-type-inference.md`](./future-type-inference.md) and Limitations. In this version `Dynamic` means the site stays unconfirmed (`unverifiable`), with no further verification.

**Helper verification:** when `AstAnalyzer` encounters a `ResolvedCall` pointing at a helper function (group 2), the checker **recursively verifies** it — with the same procedure (`AstAnalyzer` + `StaticChecker`) as the contracted function, against its own declared `@return`. The result is attached to the `ValidationReport` (see below) and a broken helper contract fails the validation of the function that calls it too. Builtin functions (group 3) cannot be verified this way — they have no Rune body, so their supplied signature is accepted as-is, without verification.

To keep (mutual) recursion between helpers from ending in an infinite loop, each named function is verified at most once per validation.

**Name collisions:** if the same name exists both as a script helper and as a builtin, the script definition wins.

**Invalid helper annotation:** if a helper has a doc-comment but its syntax is invalid, `CheckerError::InvalidContractSyntax` is returned for the whole validation (the error is not silenced). A missing helper doc-comment, on the other hand, is not an error — calls to it stay `Dynamic`.

---

## Architecture

```
user script (String)                builtin functions (&[BuiltinSignature])
        │                                          │
        ▼                                          │
┌───────────────────┐                               │
│   DocCommentParser │  — extracts @param, @return from every fn in the script
└────────┬──────────┘                               │
         │  Contract (target fn) + Contract (helper fns)
         ▼                                          │
┌────────────────────┐                              │
│  SignatureRegistry │ <─────────────────────────────┘
└────────┬───────────┘  — HashMap<String, TypeDef>: script helpers + builtins
         │
         ▼
┌───────────────────┐
│    AstAnalyzer    │  — parses the Rune AST, finds return expressions, resolves calls in the SignatureRegistry
└────────┬──────────┘
         │  Vec<ReturnSite>
         ▼
┌───────────────────┐
│  StaticChecker    │  — compares return sites (incl. ResolvedCall) with the contract
└────────┬──────────┘
         │  StaticCheckResult { verified, unverifiable, violations }
         ▼
  ValidationReport
         │
         ▼
  ScriptValidationReport { main, helpers, is_valid }
```

For every `ResolvedCall` to a helper function (`SignatureOrigin::Helper`), `AstAnalyzer` → `StaticChecker` runs again, this time on the helper's body — the resulting `ValidationReport` is stored in `ScriptValidationReport.helpers`. Each named function is verified at most once this way (see "Helper verification").

---

## Components

### 1. `DocCommentParser`

Parses a doc-comment string and returns a structured contract. Used for the contracted function as well as any helper function in the script.

**Input:** `&str` — the doc-comment content
**Output:** `Contract`

```rust
pub struct Contract {
    pub params: Vec<ParamDef>,
    pub return_type: TypeDef,
}

pub struct ParamDef {
    pub name: String,
    pub type_def: TypeDef,
}

pub enum TypeDef {
    Primitive(PrimitiveType),
    Object(Vec<(String, TypeDef)>),      // { field: Type, ... }
    Enum(Vec<EnumVariant>),              // Variant | Variant
    Nullable(Box<TypeDef>),              // Type | ()
    List(Box<TypeDef>),                  // [Type]
    Unit,                                // ()
}

pub enum PrimitiveType {
    String,
    Int,
    Float,
    Bool,
    Bytes,
}

pub struct EnumVariant {
    pub path: Vec<String>,               // ["Result", "Ok"]
    pub inner: Option<Box<TypeDef>>,
}
```

In this version `params` is purely declarative — it describes the parameter types, but the function body is not statically typed against them (out of scope; without a `RuntimeVerifier` they are moreover not used anywhere to derive mock inputs).

**Behavior:**

- Ignores lines without an `@` prefix
- Ignores unknown annotations (forward compatibility)
- Returns `Err(ParseError)` on invalid type syntax

---

### 2. `SignatureRegistry`

Merges the knowledge of helper and builtin return types (groups 2 and 3) into a single table, which `AstAnalyzer` then uses to recognize calls with a statically known return type.

**Input:**
- the source code as `&str` — every `fn` (except the contracted one) with a doc-comment is found in it and handed to `DocCommentParser`
- `&[BuiltinSignature]` — builtin signatures supplied by the host system

**Output:** `SignatureRegistry`

```rust
pub struct BuiltinSignature {
    pub name: String,        // how the function is called in the script, e.g. "http::get"
    pub return_type: TypeDef,
}

pub enum SignatureOrigin {
    /// script helper — has a body, recursively verified on ResolvedCall
    Helper(TypeDef),
    /// builtin — has no body, accepted as supplied
    Builtin(TypeDef),
}

pub struct SignatureRegistry {
    pub signatures: HashMap<String, SignatureOrigin>,   // function name → origin + return type
}
```

**Behavior:**

- A helper without a doc-comment is not included in the registry (calls to it stay `Dynamic`)
- A helper with an invalid doc-comment → `Err(InvalidContractSyntax)` for the whole validation
- On a name collision between a helper and a builtin, the helper (from the script) wins
- The `SignatureOrigin` on each entry signals whether a `ResolvedCall` to that function should additionally trigger recursive verification of its body (`Helper`), or the declared type should just be accepted unchanged (`Builtin`) — see "Helper verification" above

---

### 3. `AstAnalyzer`

Traverses the Rune AST and finds all sites where the function returns a value.

**Input:** the source code as `&str`, the function name as `&str`, `SignatureRegistry`
**Output:** `Vec<ReturnSite>`

```rust
pub enum ReturnSite {
    /// return #{ field: value, ... }
    ObjectLiteral(Vec<(String, LiteralValue)>),
    /// return "string" / 42 / true
    PrimitiveLiteral(LiteralValue),
    /// return SomeEnum::Variant(value)
    EnumLiteral { path: Vec<String>, inner: Option<Box<LiteralValue>> },
    /// return () or the implicit end of the function
    Unit,
    /// return helper(x) — name found in SignatureRegistry, return type statically known
    ResolvedCall { name: String, type_def: TypeDef },
    /// cannot be determined statically — see DynamicReason
    Dynamic(DynamicReason),
}

pub enum LiteralValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Object(Vec<(String, LiteralValue)>),
    Enum { path: Vec<String>, inner: Option<Box<LiteralValue>> },
    List(Vec<LiteralValue>),
    Unit,
    /// an object/list field is a call found in the SignatureRegistry
    ResolvedCall { name: String, type_def: TypeDef },
    Dynamic(DynamicReason),
}

/// Why a particular expression could not be evaluated statically. Serves as the
/// interface for future incremental type-inference extensions (see
/// docs/future-type-inference.md) — each variant is a separate, independently
/// conquerable case.
pub enum DynamicReason {
    /// return x; — a local variable, no dataflow tracking
    Variable(String),
    /// return helper(x); helper not found in the SignatureRegistry (no contract)
    UnannotatedCall(String),
    /// return f(x); where f is an expression/variable, not a function name
    IndirectCall,
    /// return value.compute(); — a method call on a value
    MethodCall(String),
    /// an operator, field/index access, any other expression
    Expression,
}
```

**Behavior:**

- If no function with the given name exists → `Err(FunctionNotFound)`
- Traverses the function body recursively including nested blocks (if/match/loop)
- The implicit return (the last expression without `;`) also counts as a `ReturnSite`
- A direct function call (`name(...)`) in return position, or as an object/list field value, is first looked up in the `SignatureRegistry`; found → `ResolvedCall`, otherwise → `Dynamic(DynamicReason::UnannotatedCall(name))`
- Other expression shapes are classified into the appropriate `DynamicReason` variant (see above) — even without type inference this gives more precise diagnostics than a flat `Dynamic`

---

### 4. `StaticChecker`

Compares the `Vec<ReturnSite>` from `AstAnalyzer` with the `Contract` from `DocCommentParser`.

**Output:**

```rust
pub struct StaticCheckResult {
    /// Return sites that were successfully verified
    pub verified: Vec<ReturnSite>,
    /// Return sites that are Dynamic — cannot be verified statically
    pub unverifiable: Vec<ReturnSite>,
    /// Return sites that VIOLATE the contract
    pub violations: Vec<Violation>,
}

pub struct Violation {
    pub site: ReturnSite,
    pub expected: TypeDef,
    pub actual: String,   // a description of what was found
}
```

**Rules:**

- A `Dynamic(_)` site → goes to `unverifiable`, not `violations` (regardless of the specific `DynamicReason`)
- An object literal must contain **all** fields from the contract (extra fields are allowed)
- Primitive types must match exactly
- An enum variant must be one of the variants allowed by the contract
- List: every literal element must match the inner `TypeDef`; an empty list (`[]`) is always valid
- An object literal with a field whose value is `LiteralValue::Dynamic(_)` (e.g. the result of an unannotated function call): if no other static field produces a violation, the whole return site goes to `unverifiable` (not `verified`, because the dynamic field cannot be confirmed statically). If, on the other hand, some statically known field has the wrong type/is missing, it is a `violation` regardless of the dynamic fields present.
- An enum variant with more than one inner value (`Variant(int, String)`) is not supported in v1 — both `EnumVariant.inner` and `LiteralValue::Enum.inner` assume 0 or 1 values. Multi-parameter variants are a known limitation (see below).
- `ResolvedCall { type_def, .. }` (at the return-site level or as an object/list field) is compared with the same structural rules as a literal, but `TypeDef` against `TypeDef`: the object must contain all contract fields with a compatible type, primitives must be identical, the enum variant must be a subset of the allowed variants, list/nullable recursively by the inner type. Mismatch → a `violation` with a message like `Function 'helper' returns int, expected String`. Match → `verified`.

---

### 5. `ValidationReport` — the result of validating one function

```rust
pub struct ValidationReport {
    pub function_name: String,
    pub contract: Contract,
    pub static_result: StaticCheckResult,
    pub is_valid: bool,
}
```

`is_valid` is `true` only if `static_result.violations` is empty.

The presence of `static_result.unverifiable` (Dynamic sites) does **not** affect `is_valid` — in this version the checker has no way to verify them further, so the contract is merely unconfirmed for them, not violated. A function with a non-empty `unverifiable` can therefore be `is_valid == true`; the `ValidationReport` keeps this transparently visible for potential display to the user.

This shape is shared by the contracted function and every helper the checker verified recursively (see "Helper verification").

### 6. `ScriptValidationReport` — the result of the whole validation

```rust
pub struct ScriptValidationReport {
    /// Report of the contracted function (function_name from validate_script)
    pub main: ValidationReport,
    /// Reports of the helpers encountered via ResolvedCall and verified recursively
    pub helpers: HashMap<String, ValidationReport>,
    pub is_valid: bool,
}
```

`is_valid` is `true` only if `main.is_valid` and all `helpers` have `is_valid == true` — a broken contract anywhere in the call chain fails the whole validation.

A helper that nothing in the contracted function references (directly or transitively) is not verified, even if it has its own contract — see Limitations.

---

## Public API

```rust
/// Main entry point — validates a script before saving (statically), incl.
/// recursive verification of helpers reached via ResolvedCall
pub fn validate_script(
    source: &str,
    function_name: &str,
    builtins: &[BuiltinSignature],
) -> Result<ScriptValidationReport, CheckerError>;

pub enum CheckerError {
    FunctionNotFound(String),
    NoDocComment,
    InvalidContractSyntax(String),
    RuneParseError(String),
}
```

---

## User-facing error messages

The checker should produce understandable errors displayable to the user:

| Situation | Message |
|:---|:-------|
| Function does not exist | `Function 'process' not found in script` |
| Missing doc-comment | `Function 'process' has no contract doc-comment` |
| Invalid contract syntax | `Invalid @return type: unexpected token 'xyz'` |
| Contract violation (static) | `Return value missing field 'status' (expected String)` |
| Contract violation via another function | `Function 'helper' returns int, expected String` |
| Helper does not satisfy its own contract | `Helper function 'helper' does not satisfy its own contract: return value missing field 'status'` |

---

## Limitations and known limitations

- **Dynamic return sites:** `Dynamic` does not arise only from a missing annotation on the called function (`DynamicReason::UnannotatedCall`) — `AstAnalyzer` recognizes only literals and direct calls by a name resolvable in the `SignatureRegistry`. Even with 100% annotation coverage of all helpers and builtins, `Dynamic` therefore remains for:
  - **`DynamicReason::Variable`** — a local variable (`let x = helper(); return x;`) — without dataflow analysis, what `x` was assigned is not tracked, even if `helper` had a contract
  - **`DynamicReason::IndirectCall` / `MethodCall`** — an indirect/computed call (`let f = get_handler(); return f(x);`) or a method on a value (`return value.compute();`) — there is no static name in the AST to look up in the registry
  - **`DynamicReason::Expression`** — any other expression, an operator, field/index access (`return a + b;`, `return input.name;`)

  Such a return site stays unconfirmed (`unverifiable`) and is not verified further in this version — `is_valid` does not react to it (see `ValidationReport`). Runtime verification with mock inputs, which would close this gap, is deferred — see [`docs/future-runtime-verifier.md`](./future-runtime-verifier.md). The static closure (type inference) is deferred as well, but `DynamicReason` is groundwork prepared for it — see [`docs/future-type-inference.md`](./future-type-inference.md).
- **The Rune `Any` type:** external types registered in the Rune context are not describable in the contract via the primitive type system — `TypeDef` would need extending with `Any(String)` carrying the type name.
- **Enum variants with multiple inner values** (`Variant(int, String)`) are not supported in v1 — `inner` is always 0 or 1 values.
- **A builtin's declared signature is not re-verified** (it has no Rune body) — the `SignatureRegistry` accepts it as supplied by the host system. Helpers (group 2) are, in contrast, verified recursively, see "Helper verification".
- **An unused helper is not verified:** if no `ResolvedCall` from the contracted function (directly or transitively through other helpers) leads to a helper with a contract, `validate_script` does not include it in `ScriptValidationReport.helpers` and its body is not checked.
