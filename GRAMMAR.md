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
                | include_stmt | message_stmt | transport_stmt
                | capacity_stmt | use_stmt | abort_when_decl
                | end_when_decl | comment | whitespace } ;

protocol_decl = "protocol", identifier, "v", digit, ";" ;
role_decl     = "roles", identifier, { ",", identifier }, ";" ;
state_decl    = "state", identifier, [ ":", type ], ";" ;
type_decl     = "type", identifier, "=", "{", field_list, "}", ";" ;
field_list    = field_decl, { ",", field_decl } ;
field_decl    = identifier, ":", type ;

type          = "bool" | "int" | "str" | "list" | "map"
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
handler       = "on", trigger, [ "when", guard ], "->", action_stmt, ";" ;
              | "on", trigger, [ "when", guard ], "{", { action_stmt }, "}" ;
              | "on", trigger, [ "when", guard ],
                "handle", "error", "->", action_stmt, ";" ;

trigger       = performative, [ pattern ], [ "from", identifier ],
                [ "timeout", duration ] ;
              | "breach" | "expired" ;

timer_handler = "every", duration, [ "while", guard ], [ "as", identifier ],
                "->", action_stmt, ";" ;
              | "every", duration, [ "while", guard ], [ "as", identifier ],
                "{", { action_stmt }, "}" ;

pattern       = "(", str_literal, ")" ;                   (* literal pattern *)
              | "{", json_field_list, "}" ;                (* content pattern *)

json_field_list = json_field, { ",", json_field } ;
json_field    = str_literal, ":", json_value ;
json_value    = str_literal | "$", identifier ;             (* capture var *)
```

## 4. Actions

```ebnf
action_stmt   = send_stmt | begin_stmt | fulfil_stmt | retract_stmt
              | set_stmt | push_stmt | put_stmt | for_stmt
              | abort_stmt ;

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
              | "(", expr, ")" ;

arg_list      = [ expr, { ",", expr } ] ;

(* builtin functions *)
builtin_fn    = "add" | "sub" | "mul" | "div" | "min" | "max" | "abs"
              | "len" | "sum" | "count" | "concat" | "upper" | "lower"
              | "str" | "int" | "exists" | "first" | "last" | "fmt" ;
```

## 6. Guards

```ebnf
guard         = or_guard ;
or_guard      = and_guard, { "or", and_guard } ;
and_guard     = guard_atom, { "and", guard_atom } ;
guard_atom    = [ "not" ], identifier ;
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
