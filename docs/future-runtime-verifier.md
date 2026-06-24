# RuntimeVerifier — deferred extension

This document describes the `RuntimeVerifier` component, which the current [`spec-v0.1.md`](./spec-v0.1.md) does not include. If static checking alone (`StaticChecker`) turns out to be insufficient in practice — typically for functions with a large share of `Dynamic`/`unverifiable` return sites — it will be added as an **optional (opt-in)** feature on top of the existing API, not as a mandatory step.

---

## Purpose

Runs the contracted function in an isolated Rune VM with mock inputs and verifies the return value where static checking could not (`StaticCheckResult.unverifiable`).

**It would run only if:**

- `StaticCheckResult.violations` is empty
- `StaticCheckResult.unverifiable` is non-empty (Dynamic sites exist)

---

## Proposed data structures

```rust
pub struct RuntimeVerifierConfig {
    /// Maximum run time in ms (default: 500)
    pub timeout_ms: u64,
    /// Maximum number of Rune instructions (budget)
    pub instruction_budget: u64,
    /// Mock values for the individual parameters based on their TypeDef
    pub mock_inputs: Vec<MockInput>,
}

pub enum RuntimeCheckResult {
    Passed,
    Failed(Violation),
    Timeout,
    ScriptError(String),
    Skipped,   // there were no Dynamic sites
}
```

**Generating mock inputs from `Contract.params`:**

- `String` → `"__contract_test__"`
- `int` → `0`
- `bool` → `false`
- `float` → `0.0`
- Object → recursively generated mock object
- List → empty list `[]`

---

## Behavior

- The sandbox VM would register no additional host modules (no I/O, files, network, databases) — only the Rune language core would be available. Builtin functions (group 3, see `spec-v0.1.md`) would therefore not be available during runtime verification; if the script hit one while running with mock inputs, it would be a `ScriptError`.
- `timeout_ms` would be enforced by running the VM on a separate thread; if the run did not finish within the limit, `Timeout` would be returned.
- `instruction_budget` would be enforced independently of the timeout (the number of executed VM instructions) as a safeguard against loops/recursion.
- Panic/error in the script → `RuntimeCheckResult::ScriptError(String)`
- Success → the return value is verified against the `TypeDef` from the contract

---

## Impact on `ValidationReport` and the public API

Once enabled, `ValidationReport` would gain a field:

```rust
pub runtime_result: Option<RuntimeCheckResult>,
```

and `is_valid` would additionally require `runtime_result` to be `None`, `Some(Passed)`, or `Some(Skipped)`.

The public API would extend `validate_script` with an optional parameter/config to enable this component (the exact shape to be designed during implementation — opt-in, with no change to the default/current behavior).

---

## Additional error messages (for the table in `spec-v0.1.md`)

| Situation | Message |
|:---|:-------|
| Contract violation (runtime) | `Runtime check failed: expected int, got String` |
| Timeout | `Script exceeded time limit (500ms) during contract verification` |
| Script error | `Script error during contract verification: {rune error}` |

---

## Known limitations (would apply even when enabled)

- **Mock inputs are not real data:** runtime verification only checks what the script does with the default values. Under different conditions the script may return a different type.
- **Recursive functions and loops:** runtime verification may hit the instruction budget before finishing.
- **Builtin functions are not available in the sandbox** (no host modules) — a runtime path that hits one ends as a `ScriptError`, not as contract verification.
