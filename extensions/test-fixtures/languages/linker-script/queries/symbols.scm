; What Go to Symbol lists, and where Go to Definition goes. Each definition
; captures the whole thing as @definition.<kind> and its name as @name;
; @reference marks names that may refer to one.

(output_section
  name: (symbol) @name) @definition.module
(overlay_section
  name: (symbol) @name) @definition.module
(memory_region
  name: (symbol) @name) @definition.struct
(region_alias_command
  alias: (symbol) @name) @definition.struct
(program_header
  name: (symbol) @name) @definition.enum
(assignment
  left: (symbol) @name) @definition.variable
(provide_command
  name: (symbol) @name) @definition.variable

; Sections, regions, and program headers named after their definitions.
(region
  name: (symbol) @reference.struct)
(load_region
  name: (symbol) @reference.struct)
(region_alias_command
  region: (symbol) @reference.struct)
(phdr
  name: (symbol) @reference.enum)
(insert_command
  section: (symbol) @reference.module)
(nocrossrefs_command
  section: (symbol) @reference.module)
(call_expression
  function: (symbol) @_function
  arguments: (argument_list
    (symbol) @reference.module)
  (#match? @_function "^(ADDR|ALIGNOF|LOADADDR|SIZEOF)$"))
(call_expression
  function: (symbol) @_function
  arguments: (argument_list
    (symbol) @reference.struct)
  (#match? @_function "^(ORIGIN|LENGTH)$"))
(symbol) @reference
