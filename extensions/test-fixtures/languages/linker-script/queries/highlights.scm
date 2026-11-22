; Linker scripts. Narrower captures win, and earlier patterns win for the
; same node, so specific names come before the generic `symbol`.

(comment) @comment
(string) @string
(number) @number

; Output sections, where they're defined and where they're named.
(output_section
  name: (symbol) @module)
(overlay_section
  name: (symbol) @module)
(discard) @keyword
(insert_command
  section: (symbol) @module)
(nocrossrefs_command
  section: (symbol) @module)

; Input sections: the files, then the sections taken from them.
((pattern) @constant.builtin
  (#eq? @constant.builtin "COMMON"))
(input_section
  file: (pattern) @string.special)
(input_section
  section: (pattern) @module)
(sort
  (pattern) @module)
(exclude_file
  (pattern) @string.special)

; Memory regions.
(memory_region
  name: (symbol) @type)
(attributes) @attribute
(region
  name: (symbol) @type)
(load_region
  name: (symbol) @type)
(region_alias_command
  alias: _ @type
  region: (symbol) @type)

; Program headers.
(program_header
  name: (symbol) @label)
(program_header
  type: (symbol) @constant.builtin)
(phdr
  name: (symbol) @label)

; Built-in functions and constants.
(call_expression
  function: (symbol) @function.builtin
  (#match? @function.builtin "^(ABSOLUTE|ADDR|ALIGN|ALIGNOF|ASSERT|BLOCK|CONSTANT|DATA_SEGMENT_ALIGN|DATA_SEGMENT_END|DATA_SEGMENT_RELRO_END|DEFINED|LENGTH|LOADADDR|LOG2CEIL|MAX|MIN|NEXT|ORIGIN|SEGMENT_START|SIZEOF)$"))
(call_expression
  function: (symbol) @function)
((symbol) @constant.builtin
  (#match? @constant.builtin "^(SIZEOF_HEADERS|MAXPAGESIZE|COMMONPAGESIZE)$"))
(location_counter) @variable.builtin

; Names in the file and format commands.
(format_name) @string
(filename) @string.special
(version_node
  name: (symbol) @label)
(version_node
  parent: (symbol) @label)

[
  (flag_command)
  (section_constraint)
  (section_keyword)
] @keyword

[
  "ENTRY"
  "INCLUDE"
  "INPUT"
  "GROUP"
  "AS_NEEDED"
  "OUTPUT"
  "SEARCH_DIR"
  "STARTUP"
  "OUTPUT_FORMAT"
  "OUTPUT_ARCH"
  "TARGET"
  "EXTERN"
  "NOCROSSREFS"
  "NOCROSSREFS_TO"
  "REGION_ALIAS"
  "LD_FEATURE"
  "INSERT"
  "AFTER"
  "BEFORE"
  "PROVIDE"
  "PROVIDE_HIDDEN"
  "HIDDEN"
  "ASSERT"
  "SECTIONS"
  "NOLOAD"
  "DSECT"
  "COPY"
  "INFO"
  "OVERLAY"
  "READONLY"
  "TYPE"
  "AT"
  "ALIGN"
  "SUBALIGN"
  "INPUT_SECTION_FLAGS"
  "SORT"
  "SORT_BY_NAME"
  "SORT_BY_ALIGNMENT"
  "SORT_BY_INIT_PRIORITY"
  "SORT_NONE"
  "REVERSE"
  "EXCLUDE_FILE"
  "KEEP"
  "BYTE"
  "SHORT"
  "LONG"
  "QUAD"
  "SQUAD"
  "FILL"
  "ASCIZ"
  "MEMORY"
  "ORIGIN"
  "org"
  "o"
  "LENGTH"
  "len"
  "l"
  "PHDRS"
  "FILEHDR"
  "FLAGS"
  "VERSION"
  "global"
  "local"
  "extern"
] @keyword

[
  "="
  "+="
  "-="
  "*="
  "/="
  "<<="
  ">>="
  "&="
  "|="
  "^="
  "+"
  "-"
  "*"
  "/"
  "%"
  "||"
  "&&"
  "|"
  "&"
  "=="
  "!="
  ">"
  ">="
  "<="
  "<"
  "<<"
  ">>"
  "!"
  "~"
  "?"
] @operator

[
  "("
  ")"
  "{"
  "}"
] @punctuation.bracket

[
  ";"
  ","
  ":"
] @punctuation.delimiter
