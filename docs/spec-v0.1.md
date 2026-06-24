# Rune Contract Checker — Specifikace

## Přehled

Systém pro statickou verifikaci uživatelských Rune skriptů před jejich uložením. Uživatel deklaruje kontrakt pomocí doc-comment anotací nad funkcí. Checker ověří, že implementace kontrakt splňuje, pokud je to staticky možné.

Runtime verifikace mock vstupy (spouštění funkce v sandboxu) je v této verzi mimo rozsah — viz [`docs/future-runtime-verifier.md`](./future-runtime-verifier.md). Stejně tak je mimo rozsah type inference (sledování proměnných, typování výrazů) — `AstAnalyzer` je ale navržen tak, aby šlo toto rozšíření doplnit postupně, viz [`docs/future-type-inference.md`](./future-type-inference.md).

---

## Kontrakt — syntaxe doc-commentů

Kontrakt se zapisuje jako doc-comment nad kontraktovanou funkcí — buď řádkový (`///`), nebo blokový (`/** ... */`, dekorační `*` na začátcích řádků se ignorují). Inspirace PHPStan/JSDoc.

```rune
/**
 * @param name: String
 * @return String
 */
fn process(name) {
    return "ok";
}
```

Za typem může na řádku následovat volitelný lidský popisek — do kontraktu se nepromítá:

```rune
/// @param sender: String Kdo zprávu poslal
/// @return Status::Solved Výsledek zpracování
```

### Primitivní typy

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

### Enum varianta

```rune
/// @return Result::Ok(int) | Result::Err(String)
fn process(input) {
    if input == "" {
        return Err("empty input");
    }
    return Ok(42);
}
```

Samotné jméno enumu (bez vyjmenovaných variant) matchuje libovolnou jeho
variantu — `@return Status` přijme `Status::Solved` i unii
`Status::Solved | Status::Continue`. Jakmile jsou varianty vyjmenované,
porovnávají se přesně.

```rune
/// @return Status
fn process(input) {
    Status::Solved
}
```

### Vnořené typy

```rune
/// @return { status: String, data: { id: int, name: String } }
fn process(input) {
    return #{ status: "ok", data: #{ id: 1, name: "foo" } };
}
```

### Seznam (List)

```rune
/// @return [String]
fn process(input) {
    return ["a", "b", "c"];
}
```

Lze kombinovat s ostatními typy, např. `{ items: [int] }` nebo `[{ id: int, name: String }]`.

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

---

## Skupiny funkcí

Při statické kontrole se rozlišují tři skupiny funkcí, podle toho, odkud je známý jejich návratový typ:

1. **Kontraktovaná funkce** — funkce předaná do `validate_script` jako `function_name`. Kontrakt je u ní povinný; chybí-li doc-comment, vrací se `CheckerError::NoDocComment`.
2. **Pomocné funkce** — ostatní `fn` definované uživatelem ve stejném skriptu. Anotace (`///`) jsou u nich dobrovolné — uživatel si je odekoruje sám, pokud chce, aby na ně checker mohl spoléhat při řešení volání z kontraktované funkce.
3. **Vestavěné funkce** — nativní funkce dostupné uživatelskému skriptu (registrované v Rune `Context` hostitelským systémem), které nejsou napsané v Rune a doc-comment tedy nemají. Jejich signatury dodává hostitelský systém checkeru zvenčí, ne parsováním skriptu.

Když `AstAnalyzer` narazí na návratovou hodnotu, která je přímým voláním funkce (`return helper(x)`, nebo pole objektu `code: helper()`), pokusí se název funkce dohledat ve **`SignatureRegistry`** (sloučení skupiny 2 a 3 — viz komponenta níže). Pokud je nalezen, návratový typ volané funkce je staticky znám a porovná se s kontraktem stejně jako u literálu. Pokud není nalezen (neanotovaná pomocná funkce), zůstává návratové místo `Dynamic`.

`Dynamic` ale není jen důsledek chybějící anotace — `AstAnalyzer` se o vyhledání v registru pokouší jen u přímého volání jménem. Lokální proměnná, nepřímé/computed volání, metoda na hodnotě nebo libovolný jiný výraz (operátor, field/index access) zůstávají `Dynamic` bez ohledu na to, jak důkladně jsou ostatní funkce anotované. Každý z těchto případů má svoji `DynamicReason` (viz `AstAnalyzer` níže) — v této verzi se s ní dál nic nedělá, ale je to připravený základ pro budoucí type inference, viz [`docs/future-type-inference.md`](./future-type-inference.md) a Omezení. V této verzi `Dynamic` znamená, že místo zůstává nepotvrzené (`unverifiable`), bez dalšího ověření.

**Verifikace pomocných funkcí:** narazí-li `AstAnalyzer` na `ResolvedCall` směřující na pomocnou funkci (skupina 2), checker ji **rekurzivně ověří** — stejným postupem (`AstAnalyzer` + `StaticChecker`) jako kontraktovanou funkci, proti jejímu vlastnímu deklarovanému `@return`. Výsledek se připojí k `ValidationReport` (viz níže) a porušený kontrakt pomocné funkce shodí i validaci té, která ji volá. Vestavěné funkce (skupina 3) takto ověřit nelze — nemají tělo napsané v Rune, takže se jejich dodaná signatura přijímá tak, jak je, bez verifikace.

Aby (vzájemná) rekurze mezi pomocnými funkcemi neskončila v nekonečné smyčce, ověří se každá jmenovaná funkce v rámci jedné validace nejvýše jednou.

**Kolize jmen:** pokud stejné jméno existuje jako pomocná funkce ve skriptu i jako vestavěná funkce, má přednost definice ze skriptu.

**Neplatná anotace pomocné funkce:** pokud pomocná funkce doc-comment má, ale jeho syntaxe je nevalidní, vrací se `CheckerError::InvalidContractSyntax` pro celou validaci (chyba se nepromlčuje). Chybějící doc-comment u pomocné funkce naopak není chyba — volání na ni zůstává `Dynamic`.

---

## Architektura

```
uživatelský skript (String)         vestavěné funkce (&[BuiltinSignature])
        │                                          │
        ▼                                          │
┌───────────────────┐                               │
│   DocCommentParser │  — extrahuje @param, @return ze všech fn ve skriptu
└────────┬──────────┘                               │
         │  Contract (cílová fn) + Contract (pomocné fn)
         ▼                                          │
┌────────────────────┐                              │
│  SignatureRegistry │ <─────────────────────────────┘
└────────┬───────────┘  — HashMap<String, TypeDef>: pomocné fn ze skriptu + vestavěné fn
         │
         ▼
┌───────────────────┐
│    AstAnalyzer    │  — parsuje Rune AST, hledá return výrazy, volání dohledává v SignatureRegistry
└────────┬──────────┘
         │  Vec<ReturnSite>
         ▼
┌───────────────────┐
│  StaticChecker    │  — porovná return sites (vč. ResolvedCall) s kontraktem
└────────┬──────────┘
         │  StaticCheckResult { verified, unverifiable, violations }
         ▼
  ValidationReport
         │
         ▼
  ScriptValidationReport { main, helpers, is_valid }
```

Pro každý `ResolvedCall` na pomocnou funkci (`SignatureOrigin::Helper`) se `AstAnalyzer` → `StaticChecker` spustí znovu, tentokrát na těle této pomocné funkce — výsledný `ValidationReport` se uloží do `ScriptValidationReport.helpers`. Každá jmenovaná funkce se takto ověří nejvýš jednou (viz „Verifikace pomocných funkcí").

---

## Komponenty

### 1. `DocCommentParser`

Parsuje doc-comment string a vrátí strukturovaný kontrakt. Používá se pro kontraktovanou funkci i pro libovolnou pomocnou funkci ve skriptu.

**Vstup:** `&str` — obsah doc-commentu
**Výstup:** `Contract`

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

`params` je v této verzi čistě deklarativní — popisuje typy parametrů, ale tělo funkce se vůči nim staticky netypuje (mimo rozsah; bez `RuntimeVerifier` se navíc nikde nepoužívají k odvození mock vstupů).

**Chování:**

- Ignoruje řádky bez `@` prefixu
- Neznámé anotace ignoruje (forward compatibility)
- Vrátí `Err(ParseError)` při nevalidní syntaxi typu

---

### 2. `SignatureRegistry`

Slučuje znalost návratových typů pomocných a vestavěných funkcí (skupiny 2 a 3) do jedné tabulky, kterou pak používá `AstAnalyzer` k rozpoznání volání se staticky známým návratovým typem.

**Vstup:**
- zdrojový kód jako `&str` — nalezne se v něm každá `fn` (mimo kontraktovanou) s doc-commentem a ten se předá `DocCommentParser`
- `&[BuiltinSignature]` — signatury vestavěných funkcí, dodané hostitelským systémem

**Výstup:** `SignatureRegistry`

```rust
pub struct BuiltinSignature {
    pub name: String,        // jak se funkce volá ve skriptu, např. "http::get"
    pub return_type: TypeDef,
}

pub enum SignatureOrigin {
    /// pomocná funkce ze skriptu — má tělo, bude rekurzivně ověřena při ResolvedCall
    Helper(TypeDef),
    /// vestavěná funkce — bez těla, přijímá se tak, jak je dodaná
    Builtin(TypeDef),
}

pub struct SignatureRegistry {
    pub signatures: HashMap<String, SignatureOrigin>,   // jméno funkce → původ + návratový typ
}
```

**Chování:**

- Pomocná funkce bez doc-commentu se do registru nezahrnuje (zůstává `Dynamic` při volání)
- Pomocná funkce s nevalidním doc-commentem → `Err(InvalidContractSyntax)` pro celou validaci
- Při kolizi jména mezi pomocnou a vestavěnou funkcí vyhrává pomocná (ze skriptu)
- `SignatureOrigin` u každého záznamu signalizuje, jestli se má při `ResolvedCall` na danou funkci navíc rekurzivně spustit ověření jejího těla (`Helper`), nebo jen přijmout deklarovaný typ beze změny (`Builtin`) — viz „Verifikace pomocných funkcí" výše

---

### 3. `AstAnalyzer`

Traversuje Rune AST a nalezne všechna místa kde funkce vrací hodnotu.

**Vstup:** zdrojový kód jako `&str`, název funkce jako `&str`, `SignatureRegistry`
**Výstup:** `Vec<ReturnSite>`

```rust
pub enum ReturnSite {
    /// return #{ field: value, ... }
    ObjectLiteral(Vec<(String, LiteralValue)>),
    /// return "string" / 42 / true
    PrimitiveLiteral(LiteralValue),
    /// return SomeEnum::Variant(value)
    EnumLiteral { path: Vec<String>, inner: Option<Box<LiteralValue>> },
    /// return () nebo implicitní konec funkce
    Unit,
    /// return helper(x) — jméno nalezeno v SignatureRegistry, návratový typ znám staticky
    ResolvedCall { name: String, type_def: TypeDef },
    /// nelze staticky určit — viz DynamicReason
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
    /// pole objektu/seznamu je volání nalezené v SignatureRegistry
    ResolvedCall { name: String, type_def: TypeDef },
    Dynamic(DynamicReason),
}

/// Proč konkrétní výraz nešel staticky vyhodnotit. Sloužilo by jako rozhraní pro
/// budoucí postupné rozšiřování o type inference (viz docs/future-type-inference.md) —
/// každá varianta je samostatný, nezávisle dobyvatelný případ.
pub enum DynamicReason {
    /// return x; — lokální proměnná, bez sledování dataflow
    Variable(String),
    /// return helper(x); helper nenalezen v SignatureRegistry (bez kontraktu)
    UnannotatedCall(String),
    /// return f(x); kde f je výraz/proměnná, ne přímo jméno funkce
    IndirectCall,
    /// return value.compute(); — metoda na hodnotě
    MethodCall(String),
    /// operátor, field/index access, libovolný jiný výraz
    Expression,
}
```

**Chování:**

- Pokud funkce se zadaným názvem neexistuje → `Err(FunctionNotFound)`
- Traversuje tělo funkce rekurzivně včetně vnořených bloků (if/match/loop)
- Implicitní návrat (poslední výraz bez `;`) se také považuje za `ReturnSite`
- Přímé volání funkce (`name(...)`) v návratové pozici nebo jako hodnota pole objektu/seznamu se nejprve hledá v `SignatureRegistry`; nalezení → `ResolvedCall`, jinak → `Dynamic(DynamicReason::UnannotatedCall(name))`
- Ostatní tvary výrazů se klasifikují do příslušné `DynamicReason` varianty (viz výše) — i bez type inference to dává přesnější diagnostiku než plochý `Dynamic`

---

### 4. `StaticChecker`

Porovná `Vec<ReturnSite>` z `AstAnalyzer` s `Contract` z `DocCommentParser`.

**Výstup:**

```rust
pub struct StaticCheckResult {
    /// Return sites které byly úspěšně ověřeny
    pub verified: Vec<ReturnSite>,
    /// Return sites které jsou Dynamic — nelze staticky ověřit
    pub unverifiable: Vec<ReturnSite>,
    /// Return sites které NESPLŇUJÍ kontrakt
    pub violations: Vec<Violation>,
}

pub struct Violation {
    pub site: ReturnSite,
    pub expected: TypeDef,
    pub actual: String,   // popis co bylo nalezeno
}
```

**Pravidla:**

- `Dynamic(_)` site → přesun do `unverifiable`, ne do `violations` (bez ohledu na konkrétní `DynamicReason`)
- Object literal musí obsahovat **všechna** pole z kontraktu (extra pole jsou povolena)
- Typy primitivů musí přesně odpovídat
- Enum varianta musí být jednou z povolených variant v kontraktu
- List: každý prvek literálu musí odpovídat vnitřnímu `TypeDef`; prázdný seznam (`[]`) je vždy platný
- Object literal s polem, jehož hodnota je `LiteralValue::Dynamic(_)` (např. výsledek volání neanotované funkce): pokud žádné jiné statické pole nepřináší violation, celý return site jde do `unverifiable` (ne do `verified`, protože dynamické pole nelze potvrdit staticky). Pokud je naopak nějaké staticky známé pole špatného typu/chybí, jde o `violation` bez ohledu na přítomnost dynamických polí.
- Enum varianta s více než jednou vnitřní hodnotou (`Variant(int, String)`) není v1 podporovaná — `EnumVariant.inner` i `LiteralValue::Enum.inner` počítají jen s 0 nebo 1 hodnotou. Víceparametrické varianty jsou known limitation (viz níže).
- `ResolvedCall { type_def, .. }` (na úrovni return site i jako pole objektu/seznamu) se porovná stejnými strukturálními pravidly jako literál, ale `TypeDef` proti `TypeDef`: object musí obsahovat všechna pole kontraktu se slučitelným typem, primitiva musí být shodná, enum varianta musí být podmnožinou povolených variant, list/nullable rekurzivně podle vnitřního typu. Neshoda → `violation` se zprávou typu `Function 'helper' returns int, expected String`. Shoda → `verified`.

---

### 5. `ValidationReport` — výsledek validace jedné funkce

```rust
pub struct ValidationReport {
    pub function_name: String,
    pub contract: Contract,
    pub static_result: StaticCheckResult,
    pub is_valid: bool,
}
```

`is_valid` je `true` pouze pokud `static_result.violations` je prázdné.

Přítomnost `static_result.unverifiable` (Dynamic sites) `is_valid` **neovlivňuje** — v této verzi nemá checker jak je dál ověřit, takže se u nich kontrakt jen nepotvrdí, ale ani neporuší. Funkce s neprázdným `unverifiable` tedy může být `is_valid == true`; `ValidationReport` to zůstává transparentně vidět pro případné zobrazení uživateli.

Tento tvar je společný pro kontraktovanou funkci i pro každou pomocnou funkci, kterou checker rekurzivně ověřil (viz „Verifikace pomocných funkcí").

### 6. `ScriptValidationReport` — výsledek celé validace

```rust
pub struct ScriptValidationReport {
    /// Report kontraktované funkce (function_name z validate_script)
    pub main: ValidationReport,
    /// Reporty pomocných funkcí, na které se narazilo přes ResolvedCall a které se rekurzivně ověřily
    pub helpers: HashMap<String, ValidationReport>,
    pub is_valid: bool,
}
```

`is_valid` je `true` pouze pokud `main.is_valid` a zároveň všechny `helpers` mají `is_valid == true` — porušený kontrakt kdekoliv v řetězci volání shodí celou validaci.

Pomocná funkce, na kterou se z kontraktované funkce (přímo ani transitivně) nikdo neodkazuje, se neověřuje, i kdyby měla vlastní kontrakt — viz Omezení.

---

## Veřejné API

```rust
/// Hlavní entry point — validuje skript před uložením (staticky), vč. rekurzivní
/// verifikace pomocných funkcí dosažených přes ResolvedCall
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

## Chybové hlášky pro uživatele

Checker by měl produkovat srozumitelné chyby zobrazitelné uživateli:

| Situace | Zpráva |
|:---|:-------|
| Funkce neexistuje | `Function 'process' not found in script` |
| Chybí doc-comment | `Function 'process' has no contract doc-comment` |
| Nevalidní syntaxe kontraktu | `Invalid @return type: unexpected token 'xyz'` |
| Porušení kontraktu (staticky) | `Return value missing field 'status' (expected String)` |
| Porušení kontraktu voláním jiné funkce | `Function 'helper' returns int, expected String` |
| Pomocná funkce neplní svůj vlastní kontrakt | `Helper function 'helper' does not satisfy its own contract: return value missing field 'status'` |

---

## Omezení a known limitations

- **Dynamic return sites:** `Dynamic` nevzniká jen z chybějící anotace u volané funkce (`DynamicReason::UnannotatedCall`) — `AstAnalyzer` rozpoznává pouze literály a přímé volání jménem dohledatelné v `SignatureRegistry`. I se 100% anotačním pokrytím všech pomocných i vestavěných funkcí proto `Dynamic` zůstává u:
  - **`DynamicReason::Variable`** — lokální proměnná (`let x = helper(); return x;`) — bez dataflow analýzy se nesleduje, čím byla `x` přiřazena, ani kdyby `helper` měl kontrakt
  - **`DynamicReason::IndirectCall` / `MethodCall`** — nepřímé/computed volání (`let f = get_handler(); return f(x);`) nebo metoda na hodnotě (`return value.compute();`) — v AST není statické jméno k vyhledání v registru
  - **`DynamicReason::Expression`** — libovolný jiný výraz, operátor, field/index access (`return a + b;`, `return input.name;`)

  Takové návratové místo zůstává nepotvrzené (`unverifiable`) a v této verzi se dál neověřuje — `is_valid` na něj nereaguje (viz `ValidationReport`). Runtime verifikace mock vstupy, která by tuto mezeru zacelila, je odložena — viz [`docs/future-runtime-verifier.md`](./future-runtime-verifier.md). Statické zacelení (type inference) je rovněž odložené, ale `DynamicReason` je k tomu připravený základ — viz [`docs/future-type-inference.md`](./future-type-inference.md).
- **Rune `Any` typ:** external typy registrované v Rune kontextu nejsou v kontraktu popsatelné přes primitivní typový systém — nutno rozšířit `TypeDef` o `Any(String)` s názvem typu.
- **Enum varianty s více vnitřními hodnotami** (`Variant(int, String)`) nejsou v1 podporované — `inner` je vždy 0 nebo 1 hodnota.
- **Deklarovaná signatura vestavěné funkce se nereverifikuje** (nemá tělo v Rune) — `SignatureRegistry` ji přijímá tak, jak ji dodal hostitelský systém. Pomocné funkce (skupina 2) se naopak rekurzivně ověřují, viz „Verifikace pomocných funkcí".
- **Nepoužitá pomocná funkce se neověří:** pokud na pomocnou funkci s kontraktem nevede žádné `ResolvedCall` z kontraktované funkce (přímo ani transitivně přes jiné pomocné funkce), `validate_script` ji nezahrne do `ScriptValidationReport.helpers` a její tělo se nezkontroluje.
