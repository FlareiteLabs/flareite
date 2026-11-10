; What folds: blocks (with the line that opens them), tables, table types,
; and long comments.

[
  (do_statement)
  (while_statement)
  (repeat_statement)
  (if_statement)
  (for_statement)
  (function_declaration)
  (function_definition)
  (table_constructor)
  (object_type)
  (arguments)
] @fold

((comment) @fold
  (#match? @fold "^--\\[=*\\["))
