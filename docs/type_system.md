Balance Type System
===================

1\. Design goals
----------------

The type system must:

1.  Distinguish **values vs capabilities**

2.  Encode **authority explicitly**

3.  Prevent **capability forgery**

4.  Support **distributed serialization boundaries**

5.  Make **interaction semantics type-visible where needed**

6.  Stay **ergonomic for common cases**

* * * * *

2\. Kinds of types
------------------

Balance has three primary kinds:

2.1 Value types
---------------

String\
Int\
Float\
Bool\
Bytes\
List<T>\
Map<K, V>\
Result (Ok<T> | Err<E>)\
Closure\
None\
Unit\
UserDefined

Properties:

-   copyable

-   serializable (unless marked otherwise)

-   no authority

* * * * *

### 2.1.1 Type methods inventory

All value types support method calls via `value.method(args)` syntax. Methods are functional --- they return new values without mutating the receiver.

**String** --- 13 methods: `len`, `contains`, `split`, `trim`, `to_upper`, `to_lower`, `starts_with`, `ends_with`, `replace`, `substring`, `chars`, `index_of`, `to_bytes`

**Int** --- 3 methods: `to_bytes(width)`, `to_float`, `to_string`

**Float** --- 7 methods: `floor`, `ceil`, `round`, `abs`, `sqrt`, `to_int`, `to_string`

**List** --- 13 methods: `len`, `push`, `get`, `map`, `filter`, `fold`, `contains`, `concat`, `join`, `first`, `last`, `reverse`, `to_bytes`

**Map** --- 8 methods: `keys`, `values`, `entries`, `contains_key`, `get`, `remove`, `insert`, `len`

**Bytes** --- 8 methods: `len`, `at`, `slice`, `concat`, `hex`, `to_list`, `to_string`, `to_int`

**Result** --- 5 methods: `is_ok`, `is_err`, `unwrap`, `unwrap_err`, `unwrap_or`

**Bool** --- no methods

See `balance_language_specification_antlr_readme.md` for complete method signatures.

* * * * *

2.2 Capability types
--------------------

cap KV\
cap Stdout

A capability type is:

> a reference to a service implementing a `port`

Properties:

-   not constructible

-   not serializable by default

-   carries authority

-   tied to a resolved service instance

* * * * *

2.3 Interaction result types (conventional)
-------------------------------------------

Not a separate kind, but standardized shapes:

Ack\
Observed<T, F>

These are value types but semantically meaningful.

* * * * *

2.4 Bytes type
--------------

Bytes represents raw binary data.

### Literals

```
b"deadbeef"     // hex-encoded bytes literal
b""             // empty bytes
```

### Construction

```
42.to_bytes(4)          // Int to big-endian bytes (width 1-8)
[0x48, 0x65].to_bytes() // List<Int> to bytes (each 0-255)
"hello".to_bytes()      // String to UTF-8 bytes
```

### Conversion

```
bytes.to_int()          // big-endian to signed Int (max 8 bytes)
bytes.to_list()         // to List<Int> (byte values)
bytes.to_string()       // UTF-8 decode (lossy)
bytes.hex()             // to hex String
```

### Operations

```
bytes.len()             // byte count
bytes.at(i)             // single byte value (0-255)
bytes.slice(start, end) // sub-range
bytes.concat(other)     // concatenation
```

* * * * *

2.5 Float type
--------------

64-bit floating point numbers.

### Literals

```
3.14
0.5
```

### Math methods

```
f.floor()       // round down
f.ceil()        // round up
f.round()       // round nearest
f.abs()         // absolute value
f.sqrt()        // square root
```

### Conversion

```
f.to_int()      // truncate to Int
f.to_string()   // decimal string
42.to_float()   // Int to Float
```

* * * * *

2.6 Result type
---------------

Result represents success or failure. The two variants are `Ok(value)` and `Err(value)`.

### Constructors

```
ok(42)           // Ok variant
err("not found") // Err variant
```

### The `?` operator

The postfix `?` operator propagates errors. If the value is `Err`, it immediately returns the error from the enclosing function. If `Ok`, it unwraps the value.

```
let value = fallible_operation()?
// equivalent to:
// match fallible_operation() {
//   Ok(v) => v
//   Err(e) => return err(e)
// }
```

### Pattern matching

```
match result {
  Ok(value) => { value + 1 }
  Err(msg) => { err(msg) }
}
```

### Methods

```
result.is_ok()          // true if Ok
result.is_err()         // true if Err
result.unwrap()         // extract Ok value (errors on Err)
result.unwrap_err()     // extract Err value (errors on Ok)
result.unwrap_or(default) // Ok value or default
```

* * * * *

2.7 Closure type
----------------

Closures are anonymous functions that capture their lexical environment.

### Syntax

```
|x: Int, y: Int| { x + y }    // typed parameters
|x| { x + 1 }                 // inferred types
|| { 42 }                      // no parameters
```

Note: closures do not support return type annotations. The syntax is `|params| { body }`, never `|params| -> Type { body }`.

### Properties

-   First-class values (can be stored in variables, passed as arguments, returned)

-   Capture variables from enclosing scope by value

-   Represented internally as `Value::ClosureRef(id)` with associated `ClosureData`

-   Commonly used with list methods (`map`, `filter`, `fold`)

```
let double = |x: Int| { x * 2 }
[1, 2, 3].map(double)  // [2, 4, 6]
```

* * * * *

3\. Capability typing rules
---------------------------

3.1 Introduction
----------------

Capabilities can only be introduced by:

### 1\. Resolution

let kv: cap KV = resolve KV["kv/main"]

### 2\. Entry injection

entry(kv: cap KV) { ... }

### 3\. Parameter passing

fn use(kv: cap KV) { ... }

* * * * *

3.2 No construction
-------------------

Illegal:

let kv = KV()        // ❌\
let kv: cap KV = ... // ❌ arbitrary assignment

* * * * *

3.3 No implicit conversion
--------------------------

cap KV ≠ KV

Ports are not values; capabilities are not data.

* * * * *

4\. Ownership & authority
-------------------------

We avoid Rust-level ownership, but we still need **authority tracking**.

### 4.1 Default: shareable

let kv = resolve KV["kv/main"]\
let kv2 = kv

Capabilities are:

-   copyable references

-   authority is shared

* * * * *

4.2 Authority qualifiers --- IMPLEMENTED AND ENFORCED
-----------------------------------------------------

Capabilities support three authority qualifiers, enforced at runtime via HMAC-SHA256 capability signing:

cap KV @consume\
cap KV @borrow\
cap KV @delegate

Meaning:

-   `@consume` → transfer ownership; the capability is invalidated after use (single-use authority)

-   `@borrow` → temporary use; authority cannot be passed onward to other services

-   `@delegate` → explicitly pass authority to another service; enables capability forwarding

### Enforcement

Authority qualifiers are cryptographically enforced:

-   Each `CapabilityRef` carries a `signature` (HMAC-SHA256) and an `authority` field

-   The runtime verifies authority constraints at dispatch time

-   `@consume` capabilities are invalidated after their first interaction completes

-   `@borrow` capabilities cannot be used as arguments to `@delegate` parameters

-   Violations produce runtime errors

### Static warnings

The type checker emits warnings at the serialization boundary when capabilities with `@consume` or `@borrow` qualifiers are passed to service methods, since distributed enforcement requires additional protocol support.

* * * * *

4.3 No capability serialization (default)
------------------------------------------

Capabilities don't accidentally cross process/network boundaries. Only value types may cross boundaries unless explicitly allowed via `@delegate`.

* * * * *

4.4 Mutable bindings
--------------------

By default, bindings are immutable. The `mut` qualifier enables reassignment:

```
let mut counter = 0
counter = counter + 1    // OK: counter is mutable

let x = 42
x = 43                   // ❌ error: x is not mutable
```

Rules:

-   `let mut` creates a mutable binding

-   Reassignment (`=`) only permitted on mutable bindings

-   The type of a mutable binding is fixed at declaration

-   Mutable bindings are tracked in the evaluator's environment (`mutable_vars` set)

* * * * *

5\. Serialization boundary
--------------------------

Critical for distributed semantics.

5.1 Rule
--------

> Only value types may cross process/network boundaries unless explicitly allowed.

So:

fn foo(x: String)        // ✔ OK\
fn bar(kv: cap KV)       // ❌ unless local

* * * * *

5.2 Explicit capability passing
--------------------------------

Delegated capabilities may cross boundaries:

fn bar(kv: cap KV @delegate)

Default is **no capability serialization**.

* * * * *

6\. Port typing
---------------

Ports define typed operations.

port KV {\
  get(Key) -> Value?\
  put(Key, Value) -> Ack\
}

This induces:

cap KV {\
  get(Key) -> Interaction<Value?>\
  put(Key, Value) -> Interaction<Ack>\
}

* * * * *

7\. Interaction typing
----------------------

When you call:

kv.put("a", 1)

Type is:

Interaction<Ack>

But surface syntax usually treats it as:

Ack   // with implicit settlement semantics

* * * * *

8\. Effect typing (minimal model)
---------------------------------

We do NOT introduce a full effect system yet.

But we do distinguish:

-   pure expressions

-   capability interactions

So conceptually:

pure:     T\
effectful: cap KV -> T

* * * * *

9\. Query vs Command typing
---------------------------

From port:

get(Key) -> Value? [query]\
put(Key, Value) -> Ack [command]

We derive:

| Kind | Type meaning |
| --- | --- |
| query | returns `Observed<T, F>` internally |
| command | returns `Ack` with settlement obligation |

* * * * *

10\. Observed typing
--------------------

type Observed<T, F> {\
  value: T\
  frontier: F\
}

Typing rule:

index.get(key) : Observed<Value?, Offset>

Then:

observe log.entry_available by res.frontier

discharges the obligation.

* * * * *

11\. Settlement typing
----------------------

Command:

put(...) -> Ack

Typing obligation:

> must be paired with a `settle` clause in provider context

This is not a type error at call site, but at **provider implementation site**.

* * * * *

12\. Provider typing rules
--------------------------

Inside a provider/service:

### Command rule

command put(...) -> Ack

Must include:

settle ...

Otherwise: compile error.

* * * * *

### Query rule

If returning `Observed`:

observe ...

Otherwise:

-   either explicit observation

-   or must prove visibility via annotations

* * * * *

13\. Entry typing
-----------------

entry(out: cap Stdout) -> Ack

Rules:

-   parameters must be capabilities or values

-   return type is any value type

-   interactions inside must type-check normally

* * * * *

14\. Subtyping (minimal)
------------------------

We keep this simple for now:

### Value subtyping

-   structural (records)

-   parametric (generics later)

### Capability subtyping

Not supported initially:

cap KV ≠ cap Any

We can introduce:

cap AnyPort

later if needed.

* * * * *

15\. Generics (minimal form)
----------------------------

We support:

type List<T>\
type Observed<T, F>

No higher-kinded types yet.

* * * * *

16\. Nullability / optional
---------------------------

Value?

Means:

Option<Value>

Pattern matching with `Some` and `None`:

```
match optional_value {
  Some(v) => { v }
  None => { "default" }
}
```

* * * * *

17\. Type inference
-------------------

-   local inference allowed

-   capability types must usually be explicit at boundaries

let kv = resolve KV["kv/main"]  // inferred: cap KV

* * * * *

18\. Core invariants
--------------------

The type system enforces:

### 1\. No capability forgery

You cannot create `cap T` manually.

### 2\. Authority is explicit

Capabilities must be passed explicitly. Authority qualifiers (`@consume`, `@borrow`, `@delegate`) are enforced via HMAC-SHA256 signing.

### 3\. Serialization safety

Capabilities don't accidentally cross boundaries.

### 4\. Semantic obligations are localized

-   `settle` enforced at provider

-   `observe` enforced at query implementation

* * * * *

Final intuition
===============

The Balance type system is not trying to be:

-   a full theorem prover

-   a maximal effect system

-   a borrow-checker clone

It is trying to ensure:

> **you cannot accidentally lose track of authority or distributed semantics**

while still writing code that feels like:

kv.put("a", 1)\
let x = kv.get("a")
