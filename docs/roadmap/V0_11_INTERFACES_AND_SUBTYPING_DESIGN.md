# Quince v0.11: Multiple Interfaces, Interface Subtyping & Object Hashing

## Executive Summary

Quince v0.11 introduces **Interface Contracts**, **Multiple Interface Implementation**, **Multiple Interface Inheritance**, and the built-in **`op hash(): int`** slot.

Key Rules:
1. **Single Class Inheritance**: A class can inherit from at most **1 base class** (`class Car extends Vehicle`).
2. **Multiple Interface Implementation**: A class can implement **multiple interfaces** (`class Car implements Printable, Serializable, Hashable`).
3. **Multiple Interface Inheritance**: An interface can extend **multiple parent interfaces** (`interface Serializable extends Printable, Hashable`).
4. **Interfaces Cannot Extend Classes**: Interfaces are pure behavior contracts and cannot hold state or inherit from concrete classes.
5. **Dynamic Interface Tables (`itables`)**: $O(1)$ dynamic interface method dispatch in compiled machine code via `class.itables`.

---

## 1. Syntax & Declaration Contracts

### 1.1 Interface Declarations
An `interface` defines a named contract of required method signatures:

```quince
pub interface Printable {
    fn print_summary(): string
}

pub interface Hashable {
    op hash(): int
    op eq(other: any): bool
}

# Multiple Interface Inheritance
pub interface Serializable extends Printable, Hashable {
    fn serialize(): bytes
    fn deserialize(data: bytes): self
}
```

### 1.2 Class Implementation
A class specifies implemented interfaces using the `implements` keyword:

```quince
pub class User extends Entity implements Printable, Hashable {
    let name: string
    let id: int

    pub fn print_summary(): string {
        return "User(id=" + string(self.id) + ", name=" + self.name + ")"
    }

    pub op hash(): int {
        return hash(self.id) ^ hash(self.name)
    }

    pub op eq(other: any): bool {
        if !(other is User) {
            return false
        }
        let u: User = other
        return self.id == u.id && self.name == u.name
    }
}
```

---

## 2. Dispatch Architecture & Interface Tables (`itables`)

### 2.1 Low-Level Layout
Every class descriptor contains an array of `ITable` structures:

```rust
pub struct ITable {
    pub interface_id: u32,
    pub method_pointers: Vec<*const u8>,
}
```

When calling an interface method (`OpCode::InvokeInterface`), the JIT compiler generates an inline lookup against the object's `itables` array to resolve the function pointer in $O(1)$ time, bypassing hash table lookups.

---

## 3. The `op hash(): int` Slot & Hashable Containers

`v0.11` introduces `op hash(): int` as a standard operation slot in `Class`.

- Any type implementing `Hashable` (defining `op hash(): int` and `op eq(other: any): bool`) can serve as keys in `dict[K, V]` and elements in `set[T]`.
- Built-in types (`int`, `float`, `string`, `bool`, `bytes`, `tuple[...]`) provide default $O(1)$ hardware hashing implementations.

---

## 4. Nullability-Aware Generic & Container Subtyping Matrix

Quince enforces non-nil safety guarantees across all compound types, generic classes, tuples, lists, and dicts:

| Source Type | Target Type | Allowed? | Subtyping Rationale |
| :--- | :--- | :--- | :--- |
| `list[int]` | `list[any]` | ✅ **Yes** | `int` is non-nil, satisfying non-nil `any`. |
| `list[int]` | `list[any?]` | ✅ **Yes** | `int` satisfies universal `any?`. |
| `list[int?]` | `list[any?]` | ✅ **Yes** | Both admit `nil` elements. |
| `list[int?]` | `list[any]` | ❌ **Refused** | `list[int?]` can yield `nil`, violating `list[any]`'s non-nil contract. |
| `dict[string, int]` | `dict[string, any]` | ✅ **Yes** | `int` value is non-nil, satisfying `any`. |
| `dict[string, int?]` | `dict[string, any]` | ❌ **Refused** | Dict values can be `nil`. |
| `tuple[int, string]` | `tuple[any, any]` | ✅ **Yes** | Both tuple elements are non-nil. |
| `tuple[int?, string]`| `tuple[any, any]` | ❌ **Refused** | Element 0 (`int?`) can be `nil`. |
| `Stack[int]` | `Stack[any]` | ✅ **Yes** | `int` satisfies non-nil `any`. |
| `Stack[int?]` | `Stack[any]` | ❌ **Refused** | `Stack[int?]` elements can be `nil`. |

