# Quince v0.7 — Grammar & Syntax Reference

This document provides the formal syntactic specification, lexical token definitions, EBNF grammar, and operator precedence matrix for Quince v0.7.

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

Quince reserves 31 keywords. Keywords cannot be used as variable, function, class, or parameter identifiers:

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
| `final`          | Binding / Modifier | Single-assignment binding or final class modifier            |
| `complete`       | Modifier           | Prevents `extend` blocks on a class                          |
| `sealed`         | Modifier           | Combines `final` and `complete` on a class                   |
| `const`          | Modifier           | Freezes a binding or deep parameter/return boundary          |
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
|      4      | `-`, `!`, `~`        | Unary minus, logical NOT, bitwise NOT          |  Right-to-left  |
|      5      | `*`, `/`, `//`, `%`  | Multiplication, Division, Floor Div, Remainder |  Left-to-right  |
|      6      | `+`, `-`             | Addition, Subtraction                          |  Left-to-right  |
|      7      | `<<`, `>>`           | Bitwise shift left, Bitwise shift right        |  Left-to-right  |
|      8      | `&`                  | Bitwise AND                                    |  Left-to-right  |
|      9      | `^`                  | Bitwise XOR                                    |  Left-to-right  |
|     10      | `\|`                 | Bitwise OR                                     |  Left-to-right  |
|     11      | `is`                 | Runtime type check                             | Non-associative |
|     12      | `<`, `<=`, `>`, `>=` | Relational comparison                          |  Left-to-right  |
|     13      | `==`, `!=`, `in`     | Equality, inequality, membership               |  Left-to-right  |
|     14      | `&&`                 | Short-circuit logical AND                      |  Left-to-right  |
|     15      | `\|\|`               | Short-circuit logical OR                       |  Left-to-right  |
|     16      | `??`                 | Null coalescing operator                       |  Right-to-left  |
| 17 (Lowest) | `=`                  | Assignment                                     |  Right-to-left  |

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
                  | ExprStmt ;

ImportStmt      ::= "import" IDENT
                  | "from" IDENT "import" IDENT ("," IDENT)* ;

ClassDecl       ::= ClassModifier? "class" IDENT ("extends" IDENT)? "{" ClassMember* "}" ;
ClassModifier   ::= "final" | "complete" | "sealed" ;
ClassMember     ::= Visibility? ( VarDecl | FnDecl | OpDecl ) ;
Visibility      ::= "public" | "private" | "protected" ;

ExtendDecl      ::= "extend" TypeExpr "{" MethodDecl* "}" ;
MethodDecl      ::= Visibility? FnDecl ;

AliasDecl       ::= "alias" IDENT "=" TypeExpr ;

FnDecl          ::= "fn" IDENT "(" ParamList? ")" (":" TypeExpr)? Block ;
OpDecl          ::= "op" IDENT "(" ParamList? ")" (":" TypeExpr)? Block ;
ParamList       ::= Param ("," Param)* ;
Param           ::= IDENT (":" TypeExpr)? ("=" Expr)? ;

VarDecl         ::= BindingKind IDENT (":" TypeExpr)? ("=" Expr)? ;
BindingKind     ::= "let" | "final" | "const" ;

Block           ::= "{" Statement* "}" ;

IfStmt          ::= "if" Expr Block ("else" ( IfStmt | Block ))? ;
WhileStmt       ::= "while" Expr Block ;
ForStmt         ::= "for" IDENT "in" Expr Block ;

TryStmt         ::= "try" Block "catch" IDENT Block ;
ReturnStmt      ::= "return" Expr? ;
ThrowStmt       ::= "throw" Expr ;

ExprStmt        ::= Expr ;

Expr            ::= Assignment ;
Assignment      ::= ( Primary ("." | "?.") IDENT | Primary "[" Expr "]" ) "=" Assignment
                  | NullCoalescing ;

NullCoalescing  ::= LogicalOr ("??" NullCoalescing)? ;
LogicalOr       ::= LogicalAnd ("||" LogicalAnd)* ;
LogicalAnd      ::= BitwiseOr ("&&" BitwiseOr)* ;
BitwiseOr       ::= BitwiseXor ("|" BitwiseXor)* ;
BitwiseXor      ::= BitwiseAnd ("^" BitwiseAnd)* ;
BitwiseAnd      ::= Shift ("&" Shift)* ;
Shift           ::= Relational ( ("<<" | ">>") Relational )* ;
Relational      ::= Equality ( ("<" | "<=" | ">" | ">=" | "is") Equality )* ;
Equality        ::= Additive ( ("==" | "!=" | "in") Additive )* ;
Additive        ::= Multiplicative ( ("+" | "-") Multiplicative )* ;
Multiplicative  ::= Unary ( ("*" | "/" | "//" | "%") Unary )* ;

Unary           ::= ("-" | "!" | "~") Unary | Primary ;

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

Call            ::= "(" ( Expr ("," Expr)* ","? )? ")" ;
Index           ::= "[" Expr "]" ;
Access          ::= "." IDENT ;
OptionalAccess  ::= "?." IDENT ;

TypeExpr        ::= "const"? BaseType "?"? ;
BaseType        ::= "string" | "int" | "float" | "bool" | "any" | "_"
                  | "list" ("[" TypeExpr "]")?
                  | "dict" ("[" TypeExpr ("," TypeExpr)? "]")?
                  | IDENT ;
```
