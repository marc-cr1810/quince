# Quince v0.8.1 — Grammar & Syntax Reference

This document provides the formal syntactic specification, lexical token definitions, EBNF grammar, and operator precedence matrix for Quince v0.8.1.

---

## 1. Lexical Structure

### 1.1 Source Encoding & Newlines

Quince source files must be encoded in UTF-8. Statements are primarily newline-terminated. A line break separates statements unless the line ends within an unclosed bracket (`()`, `[]`, `{}`), after a binary operator, or following a trailing comma. Optional semicolons (`;`) may separate multiple statements on a single line.

### 1.2 Comments & Documentation Blocks

- **Single-Line Comments**: Begin with `#` and extend to the end of the line.
  ```quince
  # This is a standard comment.
  let x = 10 # Inline comment
  ```
- **Documentation Comments**: Begin with `##` placed immediately above top-level declarations, classes, fields, or functions.
  ```quince
  ## Calculates the Euclidean length of a vector.
  ##
  ## @param x horizontal component
  ## @param y vertical component
  ## @return scalar distance
  fn distance(x: float, y: float): float {
      return math.sqrt(x * x + y * y)
  }
  ```

### 1.3 Keywords

Quince reserves 36 keywords. Keywords cannot be used as variable, function, class, or parameter identifiers:

| Reserved Keyword | Category           | Description                                                  |
| :--------------- | :----------------- | :----------------------------------------------------------- |
| `fn`             | Declaration        | Declares a named function or method                          |
| `op`             | Declaration        | Declares a special language operator slot on a class         |
| `class`          | Declaration        | Declares a class type                                        |
| `extends`        | Inheritance        | Specifies the superclass in a class header                   |
| `extend`         | Declaration        | Adds methods to an existing type                             |
| `self`           | Binding            | Refers to the receiver instance inside a method              |
| `super`          | Binding            | Invokes a parent class method                                |
| `let`            | Binding            | Binds a mutable local or class field                         |
| `final`          | Binding / Modifier | Single-assignment binding, final class modifier, or a member no subclass may replace |
| `complete`       | Modifier           | Prevents `extend` blocks on a class                          |
| `sealed`         | Modifier           | Combines `final` and `complete` on a class                   |
| `const`          | Modifier           | Freezes a binding or deep parameter/return boundary; before `fn`/`op`, marks the body pure |
| `override`       | Modifier           | Declares that a member replaces one a superclass declared    |
| `explicit`       | Modifier           | Before `op init`, refuses implicit constructor coercion      |
| `public`         | Visibility         | Member/module export visible everywhere                      |
| `private`        | Visibility         | Member visible only to declaring class; unexported top-level |
| `protected`      | Visibility         | Member visible to declaring class and subclasses             |
| `any`            | Type               | Non-nil top type annotation                                  |
| `is`             | Operator           | Type check and block smart-casting operator                  |
| `alias`          | Declaration        | Declares a type alias (`alias New = Existing`)               |
| `import`         | Module             | Imports a module or library                                  |
| `if`             | Control Flow       | Conditional branch                                           |
| `else`           | Control Flow       | Alternative branch for `if`                                  |
| `while`          | Control Flow       | Loop while condition is truthy                               |
| `for`            | Control Flow       | Iterates over a collection or iterable object                |
| `in`             | Operator / Keyword | Iteration target or membership query                         |
| `return`         | Control Flow       | Returns a value from a function                              |
| `try`            | Exception          | Begins a guarded exception block                             |
| `catch`          | Exception          | Handles a raised exception                                   |
| `throw`          | Exception          | Raises an error instance                                     |
| `true`           | Literal            | Boolean true                                                 |
| `false`          | Literal            | Boolean false                                                |
| `nil`            | Literal            | Absence of a value                                           |
| `and`            | Operator           | Short-circuit logical AND; `and=` is its assignment form      |
| `or`             | Operator           | Short-circuit logical OR; `or=` is its assignment form        |
| `not`            | Operator           | Logical NOT, and the first word of `not in`                  |

*Note*: `from` is a contextual keyword used in `from module import item`. It acts as an identifier in all other contexts.

### 1.4 Literals

- **Integer**: Decimal digit sequence (e.g. `42`, `-7`). Represented internally as 64-bit signed integers (`i64`).
- **Float**: Decimal digits with a fractional point (e.g. `3.14159`, `-0.5`). Represented internally as 64-bit double precision floats (`f64`).
- **String**: Double-quoted (`"..."`) or single-quoted (`'...'`) character sequences with standard UTF-8 escape sequences (`\n`, `\t`, `\r`, `\"`, `\'`, `\\`).
- **Boolean**: `true` or `false`.
- **Nil**: `nil`.

---

## 2. Operator Precedence & Associativity

Operators are listed below in order of precedence from highest (tightest binding) to lowest (loosest binding):

|    Level    | Operator             | Description                                    |  Associativity  |
| :---------: | :------------------- | :--------------------------------------------- | :-------------: |
| 1 (Highest) | `.`, `?.`            | Primary member access & optional chaining      |  Left-to-right  |
|      2      | `()`                 | Function / method call                         |  Left-to-right  |
|      3      | `[]`                 | Indexing & container type arguments            |  Left-to-right  |
|      4      | `**`                 | Exponentiation                                 |  Right-to-left  |
|      5      | `-`, `~`             | Unary minus, bitwise NOT                       |  Right-to-left  |
|      6      | `*`, `/`, `//`, `%`  | Multiplication, Division, Floor Div, Remainder |  Left-to-right  |
|      7      | `+`, `-`             | Addition, Subtraction                          |  Left-to-right  |
|      8      | `<<`, `>>`           | Bitwise shift left, Bitwise shift right        |  Left-to-right  |
|      9      | `&`                  | Bitwise AND                                    |  Left-to-right  |
|     10      | `^`                  | Bitwise XOR                                    |  Left-to-right  |
|     11      | `\|`                 | Bitwise OR                                     |  Left-to-right  |
|     12      | `??`                 | Null coalescing                                |  Right-to-left  |
|     13      | `<`, `<=`, `>`, `>=`, `in`, `not in`, `is`, `is not` | Relational comparison, membership, runtime type check | Left-to-right |
|     14      | `==`, `!=`           | Equality, inequality                           |  Left-to-right  |
|     15      | `not`                | Logical NOT                                    |  Right-to-left  |
|     16      | `and`                | Short-circuit logical AND                      |  Left-to-right  |
|     17      | `or`                 | Short-circuit logical OR                       |  Left-to-right  |
| 18 (Lowest) | `=`, `+=`, `-=`, `*=`, `/=`, `//=`, `%=`, `**=`, `&=`, `\|=`, `^=`, `<<=`, `>>=`, `and=`, `or=`, `??=` | Assignment and compound assignment | Right-to-left |

`**` is the only binary operator in the language that associates to the right, and the only
one that binds tighter than unary minus: `2 ** 3 ** 2` is `2 ** (3 ** 2)`, and `-2 ** 2` is
`-(2 ** 2)`. Both follow Python and ordinary mathematical notation.

The three logical operators are words — `and`, `or`, `not` — and not `&&`, `||`, `!`. They
join `is` and `in`, which the language already read as words, and they leave `&` and `|`
meaning exactly one thing each: there is no pair to mistype one half of. `!` survives only
inside `!=`; written on its own it is refused with a pointer to `not`.

`not` is the one unary operator that binds *looser* than the comparisons, which is where
Python puts it and is the only placement that makes the word read as the word: `not a in b`
is `not (a in b)`, and `not a == b` asks whether the two differ. It still binds tighter than
`and`, so `not a and b` is `(not a) and b`. `-` and `~` are symbols and stay at level 5.

`not in` and `is not` are the negations of `in` and `is`, bind exactly where those bind, and
mean exactly `not (a in b)` and `not (a is T)` — the two spellings of each produce the same
tree. Note that `is not` does not narrow a name for the block it guards; only a positive `is`
does, because what a *failed* type test proves is not something the checker can express.

A compound assignment `a op= b` means `a = a op b` **with the target evaluated once**, so
`d[key()] += 1` calls `key` a single time. It reaches the same operator slot the binary form
does — a class defining `op add` gets `+=` for free, and there is no separate in-place slot.

`and=`, `or=`, and `??=` are written like compound assignments and are not one, because their
right side may never run: each reads the target, and only assigns if what it found does not
already answer. `count ??= expensive()` does not call `expensive` when `count` is set, and
does not write to `count` either. The target is still evaluated exactly once.

---

## 3. Formal EBNF Grammar

```ebnf
Program         ::= Statement* EOF ;

Statement       ::= ImportStmt
                  | ClassDecl
                  | ExtendDecl
                  | AliasDecl
                  | FnDecl
                  | VarDecl
                  | IfStmt
                  | WhileStmt
                  | ForStmt
                  | TryStmt
                  | ReturnStmt
                  | ThrowStmt
                  | IncrStmt
                  | ExprStmt ;

ImportStmt      ::= "import" IDENT
                  | "from" IDENT "import" IDENT ("," IDENT)* ;

ClassDecl       ::= ClassModifier? "class" IDENT ("extends" IDENT)? "{" ClassMember* "}" ;
ClassModifier   ::= "final" | "complete" | "sealed" ;
ClassMember     ::= Visibility? ( VarDecl | MemberModifier* ( FnDecl | OpDecl ) ) ;
Visibility      ::= "public" | "private" | "protected" ;
(* Any order, normalized by the parser. `override` and `final` are refused
   outside a class body; `explicit` is refused on anything but a
   one-parameter `op init`; each may be written at most once. *)
MemberModifier  ::= "const" | "override" | "final" | "explicit" | Visibility ;

ExtendDecl      ::= "extend" TypeExpr "{" MethodDecl* "}" ;
MethodDecl      ::= Visibility? "const"? ( FnDecl | OpDecl ) ;

AliasDecl       ::= "alias" IDENT "=" TypeExpr ;

FnDecl          ::= "fn" IDENT "(" ParamList? ")" (":" TypeExpr)? Block ;
OpDecl          ::= "op" IDENT "(" ParamList? ")" (":" TypeExpr)? Block ;
ParamList       ::= Param ("," Param)* ;
(* A parameter with no default may not follow one that has a default. *)
Param           ::= BindingKind? IDENT (":" TypeExpr)? ("=" Expr)? ;

VarDecl         ::= BindingKind IDENT (":" TypeExpr)? ("=" Expr)? ;
BindingKind     ::= "let" | "final" | "const" ;

Block           ::= "{" Statement* "}" ;

IfStmt          ::= "if" Expr Block ("else" ( IfStmt | Block ))? ;
WhileStmt       ::= "while" Expr Block ;
ForStmt         ::= "for" IDENT "in" Expr Block ;

TryStmt         ::= "try" Block "catch" IDENT Block ;
ReturnStmt      ::= "return" Expr? ;
ThrowStmt       ::= "throw" Expr ;

(* `++` and `--` are statements and produce no value, so the prefix and postfix
   forms mean the same thing: `n += 1`. Neither is reachable inside an
   expression — `x = i++` is a syntax error rather than a puzzle. *)
IncrStmt        ::= AssignTarget ("++" | "--")
                  | ("++" | "--") AssignTarget ;

ExprStmt        ::= Expr ;

Expr            ::= Assignment ;
Assignment      ::= AssignTarget ( "=" | CompoundOp | ShortAssignOp ) Assignment
                  | LogicalOr ;
AssignTarget    ::= IDENT
                  | Primary ("." | "?.") IDENT
                  | Primary "[" Expr "]" ;
CompoundOp      ::= "+=" | "-=" | "*=" | "/=" | "//=" | "%=" | "**="
                  | "&=" | "|=" | "^=" | "<<=" | ">>=" ;

(* Written like a compound assignment; not one. The right side may not run. *)
ShortAssignOp   ::= "and=" | "or=" | "??=" ;

LogicalOr       ::= LogicalAnd ("or" LogicalAnd)* ;
LogicalAnd      ::= Not ("and" Not)* ;

(* The one unary operator looser than a comparison, so `not a in b` groups as
   `not (a in b)`. *)
Not             ::= "not" Not | Equality ;
Equality        ::= Relational ( ("==" | "!=") Relational )* ;
Relational      ::= NullCoalescing ( RelationalOp NullCoalescing )*
                  | NullCoalescing ("is" "not"? Type) ;
RelationalOp    ::= "<" | "<=" | ">" | ">=" | "in" | "not" "in" ;
NullCoalescing  ::= BitwiseOr ("??" NullCoalescing)? ;
BitwiseOr       ::= BitwiseXor ("|" BitwiseXor)* ;
BitwiseXor      ::= BitwiseAnd ("^" BitwiseAnd)* ;
BitwiseAnd      ::= Shift ("&" Shift)* ;
Shift           ::= Additive ( ("<<" | ">>") Additive )* ;
Additive        ::= Multiplicative ( ("+" | "-") Multiplicative )* ;
Multiplicative  ::= Unary ( ("*" | "/" | "//" | "%") Unary )* ;

(* `**` binds tighter than unary minus and associates to the right. *)
Unary           ::= ("-" | "~") Unary | Power ;
Power           ::= Primary ("**" Unary)? ;

Primary         ::= Postfix ;
Postfix         ::= Atom ( Call | Index | Access | OptionalAccess )* ;

Atom            ::= INT_LITERAL
                  | FLOAT_LITERAL
                  | STRING_LITERAL
                  | "true" | "false" | "nil"
                  | IDENT
                  | "self" | "super"
                  | ListLiteral
                  | DictLiteral
                  | "(" Expr ")" ;

ListLiteral     ::= "[" ( Expr ("," Expr)* ","? )? "]" ;
DictLiteral     ::= "{" ( DictEntry ("," DictEntry)* ","? )? "}" ;
DictEntry       ::= Expr ":" Expr ;

(* Positional arguments first; a positional one after a named one is refused. *)
Call            ::= "(" ( Argument ("," Argument)* ","? )? ")" ;
Argument        ::= (IDENT ":")? Expr ;
Index           ::= "[" Expr "]" ;
Access          ::= "." IDENT ;
OptionalAccess  ::= "?." IDENT ;

TypeExpr        ::= "const"? BaseType "?"? ;
BaseType        ::= "string" | "int" | "float" | "bool" | "any" | "_"
                  | "list" ("[" TypeExpr "]")?
                  | "dict" ("[" TypeExpr ("," TypeExpr)? "]")?
                  | IDENT ;
```
