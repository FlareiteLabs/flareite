; Highlighting, adapted from tree-sitter-luau 1.2.0 queries/highlights.scm,
; which is written for Neovim, where later patterns win: here the earlier
; one does, so specific patterns come before general ones. MIT licensed:
; see the LICENSE files in ../grammars.

; Comments

(comment) @comment
(hash_bang_line) @keyword.directive

; Keywords

"return" @keyword.return

[
  "local"
  "type"
  "export"
] @keyword

(do_statement
  [
    "do"
    "end"
  ] @keyword)

(while_statement
  [
    "while"
    "do"
    "end"
  ] @keyword.repeat)

(repeat_statement
  [
    "repeat"
    "until"
  ] @keyword.repeat)

[
  (break_statement)
  (continue_statement)
] @keyword.repeat

(if_statement
  [
    "if"
    "elseif"
    "else"
    "then"
    "end"
  ] @keyword.conditional)

(if_expression
  [
    "if"
    "then"
  ] @keyword.conditional)

(elseif_statement
  [
    "elseif"
    "then"
    "end"
  ] @keyword.conditional)

(elseif_clause
  [
    "elseif"
    "then"
  ] @keyword.conditional)

(else_statement
  [
    "else"
    "end"
  ] @keyword.conditional)

(else_clause
  "else" @keyword.conditional)

(for_statement
  [
    "for"
    "do"
    "end"
  ] @keyword.repeat)

(function_declaration
  [
    "function"
    "end"
  ] @keyword.function)

(function_definition
  [
    "function"
    "end"
  ] @keyword.function)

[
  "and"
  "not"
  "or"
  "in"
  "typeof"
] @keyword.operator

; Strings: their parts, so that what's interpolated isn't colored as text.

(string
  start: _ @string)

(string
  end: _ @string)

(string
  "`" @string)

(string_content) @string

(escape_sequence) @string.escape

(interpolation
  [
    "{"
    "}"
  ] @punctuation.special)

; Literals

(number) @number

(nil) @constant.builtin

[
  (false)
  (true)
] @boolean

(vararg_expression) @variable.builtin

; Types

(builtin_type) @type.builtin

(type_definition
  name: (identifier) @type)

(type_definition
  "=" . (identifier) @type)

(generic_type
  (identifier) @type)

(generic_type_list
  (identifier) @type)

(generic_type
  [
    "<"
    ">"
  ] @punctuation.bracket)

(generic_type_list
  [
    "<"
    ">"
  ] @punctuation.bracket)

(union_type
  (identifier) @type)

(intersection_type
  (identifier) @type)

(optional_type
  (identifier) @type)

(tuple_type
  (identifier) @type)

(variadic_type
  (identifier) @type)

(field_type
  (identifier) @type)

; After a ':' (a variable's, a parameter's, or a function's return type),
; '::' (a cast), or '->' (a function type's result).
(variable_list
  ":" . (identifier) @type)

(parameter
  ":" . (identifier) @type)

(function_declaration
  ":" . (identifier) @type)

(function_definition
  ":" . (identifier) @type)

(cast_expression
  "::" . (identifier) @type)

(function_type
  ":" . (identifier) @type)

(function_type
  "->" . (identifier) @type)

(object_field_type
  (identifier) @type)

; Table types: keys, and their values' types.
(object_type
  (identifier) @property . ":")

(object_type
  ":" . (identifier) @type)

(object_type
  "{" . (identifier) @type . "}")

; Parameters

(parameter
  .
  (identifier) @variable.parameter)

(function_type
  (identifier) @variable.parameter . ":")

; Functions

(function_call
  name: (identifier) @function.builtin
  (#any-of? @function.builtin
    ; Luau's global functions
    "assert" "collectgarbage" "error" "gcinfo" "getfenv" "getmetatable" "ipairs" "loadstring"
    "newproxy" "next" "pairs" "pcall" "print" "rawequal" "rawget" "rawlen" "rawset" "require"
    "select" "setfenv" "setmetatable" "tonumber" "tostring" "type" "typeof" "unpack" "xpcall"))

(function_call
  name: (identifier) @function.call)

(function_call
  name: (dot_index_expression
    field: (identifier) @function.call))

(method_index_expression
  method: (identifier) @function.method.call)

(function_declaration
  name: (identifier) @function)

(function_declaration
  name: (dot_index_expression
    field: (identifier) @function))

(field
  name: (identifier) @function
  value: (function_definition))

; Variables

((identifier) @variable.builtin
  (#eq? @variable.builtin "self"))

((identifier) @module.builtin
  (#any-of? @module.builtin
    "_G" "bit32" "buffer" "coroutine" "debug" "math" "os" "string" "table" "utf8" "vector"))

((identifier) @constant.builtin
  (#eq? @constant.builtin "_VERSION"))

((identifier) @constant
  (#match? @constant "^[A-Z][A-Z_0-9]+$"))

(field
  name: (identifier) @property)

(dot_index_expression
  field: (identifier) @property)

; Attributes: `@native`, which this grammar doesn't know, so it's an error.
((identifier) @attribute
  (#match? @attribute "^@"))

(variable_list
  attribute: (attribute
    ([
      "<"
      ">"
    ] @punctuation.bracket
      (identifier) @attribute)))

; Operators and punctuation

[
  "+"
  "-"
  "*"
  "/"
  "%"
  "^"
  "#"
  "=="
  "~="
  "<="
  ">="
  "<"
  ">"
  "="
  "&"
  "|"
  "?"
  "//"
  ".."
  "+="
  "-="
  "*="
  "/="
  "%="
  "^="
  "..="
  "//="
] @operator

[
  ";"
  ":"
  "::"
  ","
  "."
  "->"
] @punctuation.delimiter

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

(identifier) @variable
