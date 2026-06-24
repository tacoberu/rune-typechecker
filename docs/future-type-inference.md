# Type inference — deferred extension

This document describes a future extension of `AstAnalyzer` with type inference. The current [`spec-v0.1.md`](./spec-v0.1.md) does not include it — but `Dynamic(DynamicReason)` is designed so that it can be filled in **incrementally, case by case**, without changing the rest of the architecture (`StaticChecker` remains a purely structural comparison of `TypeDef` against `TypeDef`/literal and does not need to change at all).

---

## Why it is kept separate

Today `AstAnalyzer` can recognize only two expression shapes: a literal and a direct call by a name resolvable in the `SignatureRegistry`. Everything else falls into `Dynamic(DynamicReason)` — see `spec-v0.1.md`, the `AstAnalyzer` component, and Limitations. Type inference extends the set of recognized shapes; it adds no new step to the architecture, it only deepens what `AstAnalyzer` can return instead of `Dynamic`.

## What v0.1 already prepares for it

- **`DynamicReason`** distinguishes the individual unrecognized shapes (`Variable`, `IndirectCall`, `MethodCall`, `UnannotatedCall`, `Expression`) — each can be tackled independently, in any order, without having to solve them all at once.
- **`Contract.params: Vec<ParamDef>`** already carries the names and `TypeDef`s of the contracted and helper functions' parameters — exactly what inference would need as the initial typing environment (`name → TypeDef`) for a function body. No data model change is needed here.
- **`TypeDef`** is already the common currency for declared and statically derived types (`ResolvedCall.type_def` uses it just like `Contract.return_type`) — an inferred expression type would be expressed with the same structure; no new type system is introduced.
- **`StaticChecker`** compares a `TypeDef`/literal against the contract regardless of where the `TypeDef` came from — extending `AstAnalyzer` with inference does not touch it.

## Suggested extension order (cheapest first)

1. **`DynamicReason::Variable`** — tracking local variables (`let x = <expr>; ... return x;`) within a single function. Typing environment: `HashMap<String, TypeDef>`, initialized from `Contract.params`, extended on every `let` with the type of the right-hand side (recursively, with the same logic `AstAnalyzer` already uses for literals/`ResolvedCall`). Highest benefit/cost ratio — solves today's most common `Dynamic` case.
2. **`DynamicReason::Expression`** — typing rules for selected operators and field/index access (e.g. `input.name` where `input: { name: String, ... }` is in `Contract.params` → `String`). Requires a structural lookup into `TypeDef::Object`/`TypeDef::List`, not full inference.
3. **`DynamicReason::IndirectCall` / `MethodCall`** — requires knowing the type of the value being called (a variable holding a function, or a method receiver). Without value type tracking (step 1) there is no point starting here. Even with step 1 in place this is a substantially more expensive task (approaching a real type system) — weigh case by case whether it is worth it or stays permanently `Dynamic`.

## What this path does not solve

- **`DynamicReason::UnannotatedCall`** — that is not an inference problem but a missing annotation on the called function; the user fixes it by adding a `///` contract, not the checker.
- Anything that requires a runtime value (e.g. the result of an actual call with concrete arguments) — that remains the domain of [`docs/future-runtime-verifier.md`](./future-runtime-verifier.md); inference covers only statically derivable types.
