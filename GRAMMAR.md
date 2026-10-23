# ACL Grammar — Formal Definition

This document defines the formal grammar of the ACL language (`.aclp`
protocol programs and `.acl` vocabulary IDLs) in Extended Backus-Naur
Form (EBNF). The grammar is the single source of truth for syntax;
`src/protocol.rs` implements a recursive-descent parser for it.

## 1. Lexical tokens

```ebnf
identifier    = ( letter | "_" ), { letter | digit | "_" } ;
letter        = "a" .. "z" | "A" .. "Z" ;
digit         = "0" .. "9" ;
int_literal   = [ "-" ], digit, { digit } ;
str_literal   = '"', { any_char - '"' | '\\"' }, '"' ;
bool_literal  = "true" | "false" ;
dot_expr      = identifier, ".", identifier ;        (* arg.x, q.price *)
comment       = "//", { any_char }, newline ;
whitespace    = space | tab | newline ;
```

**Keywords (reserved):**
`protocol`, `roles`, `state`, `type`, `fn`, `on`, `every`, `while`,
`as`, `from`, `to`, `when`, `timeout`, `handle`, `send`, `begin`,
`fulfil`, `retract`, `set`, `push`, `put`, `for`, `in`, `abort`,
`return`, `end`, `scenario`, `expect`, `include`, `message`, `transport`,
`capacity`, `use`, `and`, `or`, `not`, `true`, `false`, `null`.

**Performatives (reserved, 17):**
`request`, `inform`, `query`, `propose`, `accept`, `reject`, `cancel`,
`progress`, `input_required`, `failure`, `delegate`, `error`, `hello`,
`bye`, `retract`, `subscribe`, `unsubscribe`.

## 2. Program structure

```ebnf
program       = { protocol_decl | role_decl | state_decl | type_decl
                | fn_decl | handler | timer_handler | scenario_block
                | include_stmt | import_stmt | export_stmt
                | handler_set_decl | message_stmt | transport_stmt
                | capacity_stmt | use_stmt | abort_when_decl
                | end_when_decl | comment | whitespace } ;

protocol_decl = "protocol", identifier, "v", digit,
                [ "extends", identifier ], ";" ;
              (* inheritance chain (§41): multiple protocols per file MUST
                 form an extends chain *)
role_decl     = "roles", identifier, { ",", identifier }, ";" ;
state_decl    = "state", identifier, [ ":", type ], ";" ;
type_decl     = "type", identifier, "=", "{", field_list, "}", ";" ;
field_list    = field_decl, { ",", field_decl } ;
field_decl    = identifier, ":", type ;

type          = "bool" | "int" | "str" | "float" | "list" | "map"
              | "list", "<", type, ">"                    (* typed list *)
              | "map", "<", type, ",", type, ">"        (* typed map *)
              | identifier ;                            (* user record *)

fn_decl       = "fn", identifier, "(", param_list, ")", [ "->", type ],
                "{", fn_body, "}" ;
param_list    = [ param_decl, { ",", param_decl } ] ;
param_decl    = identifier, ":", type ;
fn_body       = { fn_stmt }, return_stmt ;
fn_stmt       = "set", identifier, "=", expr, ";" ;
return_stmt   = "return", [ expr ], ";" ;
```

## 3. Handlers

```ebnf
handler       = "on", trigger, [ "when", guard ], "->", action_stmt,
                [ "handle", "error", "->", action_stmt ], ";" ;
              (* `handle error` is sugar for try/catch around the action *)
              | "on", trigger, [ "when", guard ], "{", { action_stmt }, "}" ;
              | "on", trigger, [ "when", guard ],
                "handle", "error", "->", action_stmt, ";" ;

trigger       = performative_alt, [ pattern ], [ "from", identifier ],
                [ "as", identifier ], [ "timeout", duration ] ;
              (* `as <role>` (n-role protocols, §42): the executor is that
                 ONE role instead of every role other than the sender *)
              | "breach" | "expired" ;

performative_alt = performative
                 | "(", performative, { "|", performative }, ")" ;
              (* alternation expands at parse time into one handler per
                 alternative with identical guard and actions — only valid
                 in the one-line and inline-block forms *)

(* an unattributed `on error` handler (no `from`) additionally receives
   internal failures: set/push/put evaluation errors and send retry
   exhaustion arrive as synthetic Error events with the failure text in
   the `$reason` capture (§39) *)

timer_handler = "every", duration, [ "while", guard ], [ "as", identifier ],
                "->", action_stmt, ";" ;
              | "every", duration, [ "while", guard ], [ "as", identifier ],
                "{", { action_stmt }, "}" ;

pattern       = "(", str_literal, ")" ;                   (* literal pattern *)
              | "{", json_field_list, "}" ;                (* content pattern *)

json_field_list = json_field, { ",", json_field } ;
json_field    = str_literal, ":", json_value ;
json_value    = str_literal | "$", identifier ;             (* capture var *)
              (* wire payloads are TYPED (str|int|float|bool, §45); a
                 bare $var capture converts natively, strings infer *)
              (* a bare $var value is normalized to the string "$var" at
                 parse time, so the pattern stays valid JSON *)
```

## 4. Actions

```ebnf
action_stmt   = send_stmt | broadcast_stmt | await_stmt | try_stmt
              | begin_stmt | fulfil_stmt | retract_stmt | set_stmt
              | push_stmt | put_stmt | for_stmt | abort_stmt ;

await_stmt    = "await", int_literal, performative,
                "into", identifier, "within", duration, "as", identifier, ";" ;
              (* declarative collection (§43): matching events are consumed
                 into the list state; the bool flag completes or times out *)

try_stmt      = "try", "{", [ action_stmt, { action_stmt } ], "}",
                "catch", "{", [ action_stmt, { action_stmt } ], "}", ";" ;
              (* local error flow (§44): a failing body action cuts the
                 body and runs the catch actions *)

broadcast_stmt = "broadcast", performative, [ pattern ],
                 "to", identifier, ";" ;
              (* fan-out: one send per role named in the list state;
                 dialogue legality is checked like an ordinary send *)

send_stmt     = "send", performative,
                [ pattern ],                              (* fields *)
                [ "to", identifier ],
                [ "retry", int_literal, "backoff", duration, [ "exp" ] ] ;

begin_stmt    = "begin" ;
fulfil_stmt   = "fulfil" ;
retract_stmt  = "retract", "(", identifier, ")" ;
set_stmt      = "set", target, "=", expr, ";" ;
target        = identifier | identifier, ".", identifier ;
push_stmt     = "push", identifier, ",", expr, ";" ;
put_stmt      = "put", identifier, ",", str_literal, ",", expr, ";" ;
for_stmt      = "for", identifier, "in", identifier,
                [ "{", { action_stmt }, "}" ] ;
              | "for", identifier, "in", identifier, "->", action_stmt, ";" ;
abort_stmt    = "abort" ;
```

## 5. Expressions

```ebnf
expr          = or_expr ;
or_expr       = and_expr, { "or", and_expr } ;
and_expr      = cmp_expr, { "and", cmp_expr } ;
cmp_expr      = add_expr, [ cmp_op, add_expr ] ;
cmp_op        = "<" | "<=" | ">" | ">=" | "==" | "!=" ;
add_expr      = mul_expr, { add_op, mul_expr } ;
add_op        = "+" | "-" ;
mul_expr      = unary_expr, { mul_op, unary_expr } ;
mul_op        = "*" | "/" ;
unary_expr    = [ "-" ], atom ;

atom          = int_literal
              | str_literal
              | bool_literal
              | identifier                                (* state ref *)
              | dot_expr                                  (* arg.x, q.price *)
              | identifier, "(", arg_list, ")"            (* function call *)
              | identifier, "{", field_init_list, "}"     (* record literal *)
              | "(", expr, ")" ;

field_init_list = field_init, { ",", field_init } ;
field_init      = identifier, ":", expr ;

arg_list      = [ expr, { ",", expr } ] ;

(* builtin functions *)
builtin_fn    = "add" | "sub" | "mul" | "div" | "min" | "max" | "abs"
              | "len" | "sum" | "count" | "concat" | "upper" | "lower"
              | "str" | "int" | "exists" | "first" | "last" | "fmt"
              | "prefer" ;
              (* prefer(list, key) — contract-net bid selection: the map or
                 record in `list` with the LOWEST numeric value for `key`;
                 empty list yields unset *)
```

## 6. Guards

```ebnf
guard         = or_guard | expr_guard ;

or_guard      = and_guard, { "or", and_guard } ;
and_guard     = guard_atom, { "and", guard_atom } ;
guard_atom    = [ "not" ], identifier ;

expr_guard    = expr ;
              (* any guard containing a capture reference or a comparison
                 operator is parsed as a full expression and evaluated by
                 the runtime with the trigger's captures in scope —
                 captures shadow session states. Example:
                 `on inform {"price": $p} when $p > 100 -> ...` *)
```

## 7. Scenarios (§35.12)

```ebnf
scenario_block = "scenario", str_literal, "{", { scenario_step }, "}" ;

scenario_step = send_step | expect_send | expect_state
              | expect_commitment ;

send_step     = identifier, "sends", performative,
                [ pattern ], "to", identifier, ";" ;
expect_send   = "expect", identifier, "sends", performative, ";" ;
expect_state  = "expect", "state", identifier, ".", identifier,
                [ "==", bool_literal ], ";" ;
expect_commitment = "expect", "commitment", identifier, status_word, ";" ;
status_word   = "proposed" | "active" | "fulfilled" | "breached" ;
```

## 8. Top-level declarations

```ebnf
use_stmt      = "use", qualified_name, [ "as", identifier ], ";" ;
qualified_name = identifier, ".", identifier ;

transport_stmt = "transport", ("stdio" | "http" | "ws" | "relay"), ";" ;
capacity_stmt  = "capacity", int_literal, ";" ;
message_stmt   = "message", identifier, "=", str_literal, ";" ;

include_stmt  = "include", str_literal,
                [ "with", "roles", "(", role_mapping, ")" ], ";" ;
role_mapping  = role_map, { ",", role_map } ;
role_map      = identifier, "=", identifier ;

include_set_stmt = "include", "handler_set", qualified_name, ";" ;
              (* splices a declared handler set into the current protocol;
                 names may be namespaced (module.set) by an import *)

import_stmt   = "import", str_literal, [ "as", identifier ], ";" ;
              (* loads a module file and splices ONLY its exported handler
                 sets, namespaced as <alias>.<set>; without an alias the
                 original names are used and collisions are compile errors *)

export_stmt   = ( "export", "handler_set", identifier
                | "export", "type", identifier
                | "export", "fn", identifier ), ";" ;
              (* marks the item importable by other modules; types and
                 functions splice under their OWN names on import *)

handler_set_decl = "handler_set", identifier, "{", { handler }, "}" ;
              (* a reusable handler fragment; roles it references are
                 checked against the INCLUDING protocol at include time *)

abort_when_decl = "abort", "when", guard, ";" ;
end_when_decl   = "end", "when", ( "fulfilled" | "expired" | "breached"
                                  | identifier ), ";" ;

duration      = int_literal, ( "ms" | "s" | "m" ) ;
```

## 9. Scopes and resolution

- **Global scope**: session states (declared via `state`), roles,
  transport, capacity, templates.
- **Function scope**: parameters (from `fn_decl`) + locals (via `set`).
  Locals shadow globals within the function body.
- **Handler scope**: session states + pattern captures (`$var`).
  Captures shadow session states for the duration of the handler.
- **Loop scope**: the loop variable (`for x in ...`) is added to the
  handler scope for the duration of the loop body, then removed.

## 10. Semantic rules (non-EBNF)

1. **Dialogue matrix**: every `send <perf>` must be legal after the
   trigger's performative (§16). Violation = compile error E014.
2. **Linearity**: every `send` must have a receiver handler in the
   target role's projection. Violation = compile error E015.
3. **Liveness**: every handler must reach a terminal action
   (`fulfil`, `bye`, `reject`, `error`). Violation = warning W201.
4. **Guard types**: guards and `end when` only accept `bool` states.
   Violation = compile error E020.
5. **Type checking**: `set <state: int> = "str"` is a compile error
   E021 (state type vs expression type).
6. **Function return**: every `fn` body MUST have a `return` statement.
   Violation = compile error E030.
7. **Capability gate**: `use <cap>` requires the host to grant the
   capability before session open. Violation = runtime error E040.
8. **Static expression types**: a `set` to a declared typed state whose
   expression type is statically known and incompatible is a compile
   error (state `int` accepts `int`; `float` accepts `int` and `float`;
   captures and `arg.*` are statically unknown and stay runtime-checked).
   Record field access `q.price` and record literals are validated
   against the declared `type` — unknown fields are compile errors.
9. **Handler-set visibility**: a `handler_set` may be declared once per
   file (duplicate = compile error); `export handler_set x` requires a
   declared set with that name; `include handler_set ns.x` requires an
   imported set — violations are compile errors.
10. **Internal error routing**: internal action failures route to the
   unattributed `on error` handler with `$reason`; failures inside the
   error handler are logged only (no error loops).
11. **Module imports**: `import` splices only exported handler sets;
   module nesting deeper than 8 levels (import cycles) is an error.
12. **Function recursion**: a function that calls itself (directly or
   through another function) is a compile error — the runtime has no
   call stacks.
13. **Await targets**: `await ... into <list>` requires a list state and
   `as <flag>` a declared bool state; the collector consumes matching
   events before handlers see them.
