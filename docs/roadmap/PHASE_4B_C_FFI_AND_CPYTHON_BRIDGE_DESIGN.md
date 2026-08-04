# Phase 4B: Foreign Function Interfaces (Transparent C FFI & CPython Bridge)

## Executive Summary

Phase 4B implements the **Unified Transparent Import Resolution Mechanism**, supporting system C dynamic libraries (`import raylib`), CPython modules (`import py:numpy`), selective symbol imports, and symbol aliasing (`as`).

---

## 1. Transparent C FFI (`import <lib>`)

```quince
import raylib as rl
from raylib import InitWindow, CloseWindow, SetTargetFPS
```

- When `import raylib` resolves to `libraylib.so` / `raylib.dll`, Quince dynamically binds C-ABI exported symbols using libffi / Cranelift function wrappers.

---

## 2. CPython Bridge (`import py:<module>`)

```quince
import py:numpy as np
from py:numpy import array as np_array
```

- The `py:` prefix triggers the CPython C-API loader (`libpython3`).
- **Dynamic Marshaling**: Transparently marshals Quince primitives (`int`, `float`, `string`, `list`, `dict`) to Python `PyObject*` values and back.
