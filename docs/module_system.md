Balance Module System
=====================

1\. Goals
---------

-   Deterministic builds

-   Explicit dependency graph

-   No global namespace

-   Clear separation:

    -   **code units (modules)**

    -   **published services (runtime)**

-   Support for:

    -   local development

    -   versioned distribution

* * * * *

2\. File = Module
-----------------

Each file defines exactly one module.

file: kv.bl

Implicit module name (default):

module kv

Optional explicit declaration:

module kv.storage

* * * * *

3\. Module contents
-------------------

A module may contain:

-   `port`

-   `service`

-   `entry`

-   `type`

-   `fn` (pure functions)

-   `substrate`

-   `guarantee`

* * * * *

4\. Visibility
--------------

Default visibility depends on the declaration kind:

-   `fn` — **exported by default** (for ergonomics; most functions are utility helpers)

-   `port`, `service`, `type`, `substrate` — **private by default**

Explicit export for non-fn declarations:

export port KV { ... }\
export service MainKV { ... }\
export type User { ... }

Functions are always importable from other modules without `export`.

* * * * *

5\. Imports
-----------

### Basic import

import kv.storage

### Selective import

import kv.storage.{KV, MainKV}

### Aliased import

import kv.storage as storage

### Aliased symbol

import kv.storage.{KV as KVPort}

* * * * *

### 5.5 Dynamic import

Balance supports runtime module loading via the `import()` expression:

```
import("path/to/module.bl")
```

Properties:

-   Evaluates at runtime, not compile time

-   The path argument is an expression (can be computed dynamically)

-   Returns `Unit` --- imported declarations are merged into the current scope

-   All functions from the imported module become available immediately after the import call

-   Useful for plugin architectures, conditional loading, and test helpers

Example:

```
// Conditionally load a module
if use_extended {
  import("helpers/extended.bl")
}
// Functions from extended.bl are now in scope (if loaded)
```

Note: the type checker treats `import()` as returning `Unit`. Dynamic imports bypass the static module dependency graph --- they are invisible to cycle detection and cannot export ports or services.

* * * * *

6\. Name resolution
-------------------

Within a module:

1.  Local declarations

2.  Imported symbols

3.  Qualified names

Example:

storage.KV

No implicit global namespace.

* * * * *

7\. Service identity vs module identity
---------------------------------------

Important distinction:

-   **Module** = compile-time unit

-   **Service** = runtime-published entity

module kv.storage

export service MainKV provides KV {\
  publish as "kv/main"\
}

-   `kv.storage` → module path

-   `"kv/main"` → runtime identity

* * * * *

8\. Packages
------------

A package is a directory with a manifest:

balance.toml

Example:

[package]\
name = "kv"\
version = "0.1.0"

[dependencies]\
net = "1.2.0"\
crypto = "0.5.1"

* * * * *

9\. Module path resolution
--------------------------

Given:

import kv.storage

Resolver:

1.  Look in local package

2.  Look in dependencies

3.  Match path to file:

    kv/storage.bl

* * * * *

10\. Dependency model
---------------------

-   Acyclic module graph required

-   Cycles allowed only via:

    -   `port` references (interfaces)

    -   not via `service` or `entry`

* * * * *

11\. Service wiring across modules
----------------------------------

Example:

// kv/port.bl\
export port KV { ... }

// kv/service.bl\
import kv.port.{KV}

export service MainKV provides KV { ... }

* * * * *

12\. Resolve across modules
---------------------------

import kv.port.{KV}

let kv = resolve KV["kv/main"]

Resolution is runtime; module only provides type + interface.

* * * * *

13\. Profiles across modules
----------------------------

Profiles are **global identifiers**, not module-scoped:

with profile public_strong

Modules may:

-   reference profiles

-   not redefine them (initially)

* * * * *

14\. Entry selection
--------------------

A module may define multiple entries:

entry cli(...) { ... }\
entry worker(...) { ... }

CLI:

balance run kv/service.bl --entry cli

Default:

-   if only one `entry`, use it

-   else require explicit selection

* * * * *

15\. Build output
-----------------

Compilation produces:

-   **IR / bytecode / executable bundle**

-   includes:

    -   module graph

    -   type info

    -   service definitions

    -   substrate bindings

* * * * *

16\. Service publication boundary
---------------------------------

Services are not active unless:

-   executed via runtime

-   or deployed

balance deploy kv.service:MainKV

* * * * *

17\. Versioning
---------------

Modules:

-   versioned via package (`balance.toml`)

Services:

-   versioned via:

    -   name convention (`kv/v1/main`)

    -   or metadata (future)

* * * * *

18\. Determinism constraints
----------------------------

-   No implicit imports

-   No ambient global services

-   All capabilities come from:

    -   `resolve`

    -   `entry` injection

* * * * *

19\. Minimal example
--------------------

// kv/port.bl\
export port KV {\
  get(Key) -> Value?\
  put(Key, Value) -> Ack\
}

// kv/service.bl\
module kv.service

import kv.port.{KV}

export service MainKV provides KV {\
  publish as "kv/main"

  query get(key) -> Value? {\
    return none\
  }\
}

// app.bl\
import kv.port.{KV}

entry() {\
  let kv = resolve KV["kv/main"]\
  kv.get("a")\
}

* * * * *

20\. Non-goals (v0.1)
---------------------

-   No circular service wiring

* * * * *

Final summary
=============

The module system enforces:

-   **compile-time structure is explicit and deterministic**

-   **runtime structure is separate and capability-driven**

-   **services are not implicitly linked --- only resolved**
