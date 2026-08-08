use super::*;

/// Infers over a program, which must compile — the pass is allowed to be
/// handed a broken tree by the language server, but a test asserting an
/// answer should not be quietly asserting it about a parse error.
fn types(src: &str) -> Types {
    let program = crate::compile(src).expect("the test program compiles");
    infer(&program)
}

/// What `name` holds at the end of the file, which is where an editor asks
/// from — so a test that passes here is a test about the thing that ships.
fn of(src: &str, name: &str) -> Type {
    types(src).of_name(name, src.len() as u32)
}

fn class_of(src: &str, name: &str) -> Option<String> {
    of(src, name).class_name().map(str::to_string)
}

/// The offset of `needle` in `src`, for asking a question from inside a
/// scope rather than at the end of the file.
fn at(src: &str, needle: &str) -> u32 {
    src.find(needle).expect("the marker is in the program") as u32
}

#[test]
fn a_literal_is_its_own_type() {
    assert_eq!(class_of("let a = 1", "a").as_deref(), Some("int"));
    assert_eq!(class_of("let a = 1.5", "a").as_deref(), Some("float"));
    assert_eq!(class_of("let a = \"hi\"", "a").as_deref(), Some("string"));
    assert_eq!(class_of("let a = true", "a").as_deref(), Some("bool"));
    assert_eq!(class_of("let a = nil", "a").as_deref(), Some("nil"));
    assert_eq!(class_of("let a = [1, 2]", "a").as_deref(), Some("list"));
    assert_eq!(class_of("let a = {\"k\": 1}", "a").as_deref(), Some("dict"));
}

#[test]
fn a_constructor_call_makes_one_of_the_class_it_names() {
    let src = "class Point {\n  op init(x) { self.x = x }\n}\nlet p = Point(1)\n";
    assert_eq!(class_of(src, "p").as_deref(), Some("Point"));
}

#[test]
fn a_class_name_holds_a_class_and_not_an_instance() {
    // The capital-letter heuristic answers `Point` here and is wrong. The
    // distinction is a reason the pass exists: `Point` is a value of type
    // `class`, and only `Point()` is a `Point`.
    let src = "class Point {\n  op init() { self.x = 1 }\n}\n";
    assert_eq!(class_of(src, "Point").as_deref(), Some("class"));
}

#[test]
fn a_lowercase_class_is_still_a_class() {
    // The other half of the same point: the heuristic decides by spelling,
    // and this decides by declaration.
    let src = "class point {\n  op init() { self.x = 1 }\n}\nlet p = point()\n";
    assert_eq!(class_of(src, "p").as_deref(), Some("point"));
}

#[test]
fn a_conversion_produces_the_type_it_names() {
    assert_eq!(class_of("let a = int(\"4\")", "a").as_deref(), Some("int"));
    assert_eq!(class_of("let a = string(4)", "a").as_deref(), Some("string"));
    assert_eq!(class_of("let a = list(\"ab\")", "a").as_deref(), Some("list"));
}

#[test]
fn a_function_is_what_its_returns_agree_on() {
    let src = "fn pick(c) {\n  if c { return 1 }\n  return 2\n}\nlet a = pick(true)\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("int"));
}

#[test]
fn a_function_whose_returns_disagree_is_unknown() {
    let src = "fn pick(c) {\n  if c { return 1 }\n  return \"two\"\n}\nlet a = pick(true)\n";
    assert_eq!(of(src, "a"), Type::Unknown);
}

#[test]
fn a_function_that_returns_nothing_returns_nil() {
    let src = "fn shout(x) { print(x) }\nlet a = shout(1)\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("nil"));
}

#[test]
fn a_bare_return_is_a_nil_that_joins() {
    // What makes `return` with no value worth handling rather than skipping:
    // skipping it would call this function an int.
    let src = "fn maybe(c) {\n  if c { return }\n  return 1\n}\nlet a = maybe(true)\n";
    assert_eq!(of(src, "a"), Type::Unknown);
}

#[test]
fn a_recursive_function_does_not_hang_the_pass() {
    let src =
        "fn down(n) {\n  if n <= 0 { return 0 }\n  return down(n - 1)\n}\nlet a = down(3)\n";
    // The recursive arm carries no information, so the two returns
    // disagree. Answering `Unknown` is the point; not answering at all
    // would be the bug.
    assert_eq!(of(src, "a"), Type::Unknown);
}

#[test]
fn a_method_return_is_read_through_the_receiver() {
    let src = "class Box {\n  op init() { self.n = 1 }\n  fn size() { return 2 }\n}\nlet b = Box()\nlet a = b.size()\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("int"));
}

#[test]
fn a_method_named_but_not_called_is_a_function() {
    let src = "class Box {\n  op init() { self.n = 1 }\n  fn size() { return 2 }\n}\nlet b = Box()\nlet a = b.size\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("function"));
}

#[test]
fn a_field_is_what_the_class_assigns_to_it() {
    let src = "class Point {\n  op init() { self.x = 1 }\n}\nlet p = Point()\nlet a = p.x\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("int"));
}

#[test]
fn a_field_assigned_two_types_is_unknown() {
    let src = "class Wobble {\n  op init() { self.v = 1 }\n  fn reset() { self.v = \"\" }\n}\nlet w = Wobble()\nlet a = w.v\n";
    assert_eq!(of(src, "a"), Type::Unknown);
}

#[test]
fn a_field_is_found_on_the_parent() {
    let src = "class Base {\n  op init() { self.tag = \"b\" }\n}\nclass Kid extends Base {\n  op init() { super.init() }\n}\nlet k = Kid()\nlet a = k.tag\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("string"));
}

#[test]
fn a_self_referential_field_does_not_hang_the_pass() {
    let src = "class Node {\n  op init() { self.next = Node() }\n}\nlet n = Node()\n";
    assert_eq!(class_of(src, "n").as_deref(), Some("Node"));
}

#[test]
fn self_is_the_class_whose_body_it_is_in() {
    let src = "class Point {\n  op init() { self.x = 1 }\n  fn me() { return self }\n}\nlet p = Point()\nlet a = p.me()\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("Point"));
}

#[test]
fn super_reaches_the_parent_class() {
    let src = "class Base {\n  op init() { self.n = 1 }\n  fn tag() { return \"b\" }\n}\nclass Kid extends Base {\n  op init() { super.init() }\n  fn mine() { return super.tag() }\n}\nlet k = Kid()\nlet a = k.mine()\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("string"));
}

#[test]
fn a_parameter_carries_no_information() {
    let src = "fn f(x) { return x }\nlet a = f(1)\n";
    assert_eq!(of(src, "a"), Type::Unknown);
}

#[test]
fn the_operators_the_language_decides_are_decided() {
    assert_eq!(class_of("let a = 1 + 2", "a").as_deref(), Some("int"));
    assert_eq!(class_of("let a = 1 / 2", "a").as_deref(), Some("float"));
    assert_eq!(class_of("let a = 7 // 2", "a").as_deref(), Some("int"));
    assert_eq!(class_of("let a = 1 + 2.0", "a").as_deref(), Some("float"));
    assert_eq!(class_of("let a = \"x\" + \"y\"", "a").as_deref(), Some("string"));
    assert_eq!(class_of("let a = [1] + [2]", "a").as_deref(), Some("list"));
    assert_eq!(class_of("let a = 1 < 2", "a").as_deref(), Some("bool"));
    assert_eq!(class_of("let a = not 1", "a").as_deref(), Some("bool"));
    assert_eq!(class_of("let a = -1", "a").as_deref(), Some("int"));
}

#[test]
fn an_operator_a_class_answers_for_is_unknown() {
    // `op add` may return anything at all, so `m + m` is not a `Money`
    // because the operands were. Assuming otherwise is the kind of guess
    // that is right often enough to be trusted and wrong without warning.
    let src = "class Money {\n  op init(c) { self.c = c }\n  op add(o) { return self.c }\n}\nlet m = Money(1)\nlet a = m + m\n";
    assert_eq!(of(src, "a"), Type::Unknown);
}

#[test]
fn a_comparison_on_a_class_is_still_a_bool() {
    // Because the evaluator makes it one: whatever `op cmp` returns is read
    // for its sign and turned into a bool before anyone sees it.
    let src = "class Money {\n  op init(c) { self.c = c }\n  op cmp(o) { return self.c - o.c }\n}\nlet m = Money(1)\nlet a = m < m\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("bool"));
}

#[test]
fn indexing_says_only_what_it_can() {
    assert_eq!(class_of("let a = \"abc\"[0]", "a").as_deref(), Some("string"));
    assert_eq!(of("let a = [1, 2][0]", "a"), Type::Unknown);
    assert_eq!(class_of("let a = [1, 2][0:1]", "a").as_deref(), Some("list"));
    assert_eq!(class_of("let a = \"abc\"[1:]", "a").as_deref(), Some("string"));
}

#[test]
fn a_loop_variable_takes_the_elements_it_can_see() {
    let src = "for n in [1, 2] { print(n) }\n";
    assert_eq!(types(src).of_name("n", at(src, "print")).class_name(), Some("int"));

    let src = "for n in [1, \"two\"] { print(n) }\n";
    assert_eq!(types(src).of_name("n", at(src, "print")), Type::Unknown);

    let src = "let xs = [1, 2]\nfor n in xs { print(n) }\n";
    // A list is not a `list[T]`, so what is in one held by a name is not
    // written down anywhere for this to read.
    assert_eq!(types(src).of_name("n", at(src, "print")), Type::Unknown);
}

#[test]
fn a_name_assigned_a_second_type_holds_neither() {
    let src = "let a = 1\na = \"one\"\n";
    assert_eq!(of(src, "a"), Type::Unknown);
}

#[test]
fn a_name_assigned_the_same_type_keeps_it() {
    let src = "let a = 1\na = 2\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("int"));
}

#[test]
fn a_local_does_not_answer_for_a_name_outside_it() {
    let src = "fn f() {\n  let x = 1\n  return x\n}\nlet y = 2\n";
    // `x` is gone by the end of the file, and nothing is offered in its
    // place.
    assert_eq!(of(src, "x"), Type::Unknown);
}

#[test]
fn the_innermost_binding_wins() {
    let src = "let x = 1\nfn f() {\n  let x = \"inner\"\n  print(x)\n}\n";
    let types = types(src);
    assert_eq!(types.of_name("x", at(src, "print")).class_name(), Some("string"));
    assert_eq!(types.of_name("x", src.len() as u32).class_name(), Some("int"));
}

#[test]
fn a_name_is_not_known_before_it_is_bound() {
    let src = "let a = 1\nlet b = 2\n";
    assert_eq!(types(src).of_name("b", 0), Type::Unknown);
}

#[test]
fn a_function_declared_below_is_still_callable_above() {
    // The forward reference the resolver goes out of its way to allow. A
    // pass reading the file top to bottom would answer `Unknown` here.
    let src = "let a = two()\nfn two() { return 2 }\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("int"));
}

#[test]
fn an_imported_module_is_a_module() {
    let src = "import math\n";
    assert_eq!(of(src, "math"), Type::module("math"));
}

#[test]
fn a_module_constant_is_what_building_it_produces() {
    let src = "import math\nlet a = math.pi\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("float"));
}

#[test]
fn a_module_function_is_a_function() {
    let src = "from math import floor\n";
    assert_eq!(class_of(src, "floor").as_deref(), Some("function"));
}

#[test]
fn a_native_is_what_it_says_it_returns() {
    // The case the whole `returns` field exists for. `split` crosses from a
    // string to a list, and nothing about the line it is written on says so.
    assert_eq!(
        class_of("let a = \"x,y\".split(\",\")", "a").as_deref(),
        Some("list")
    );
    assert_eq!(class_of("let a = len(\"ab\")", "a").as_deref(), Some("int"));
    assert_eq!(class_of("let a = type(1)", "a").as_deref(), Some("string"));
    assert_eq!(
        class_of("import math\nlet a = math.floor(2.5)", "a").as_deref(),
        Some("int")
    );
    assert_eq!(
        class_of("let s = \", \"\nlet a = s.join([\"x\"])", "a").as_deref(),
        Some("string")
    );
}

#[test]
fn a_native_that_does_not_say_is_still_unknown() {
    // `abs` keeps the type it was handed and `dict.get` answers with
    // whatever was stored. A table cannot say what those are, and the field
    // being allowed to decline is what keeps the rest of it trustworthy.
    assert_eq!(of("import math\nlet a = math.abs(-1)", "a"), Type::Unknown);
    assert_eq!(of("let a = {\"k\": 1}.get(\"k\", 0)", "a"), Type::Unknown);
    assert_eq!(of("import io\nlet a = io.line()", "a"), Type::Unknown);
}

#[test]
fn a_class_extending_a_builtin_inherits_what_its_methods_return() {
    let src = "class Stack extends list {\n  op init() { super.init() }\n}\nlet s = Stack()\nlet a = s.sort()\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("list"));
}

#[test]
fn a_method_a_program_wrote_beats_the_table_it_inherited() {
    // Dispatch asks the class first, so inference has to as well — a
    // `sort` written here is not the builtin's `sort`.
    let src = "class Odd extends list {\n  op init() { super.init() }\n  override fn sort() { return \"nope\" }\n}\nlet o = Odd()\nlet a = o.sort()\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("string"));
}

#[test]
fn an_extension_on_a_builtin_is_found_beside_its_own_methods() {
    let src = "extend list {\n  fn second() { return self[1] }\n}\nlet xs = [1, 2]\n";
    let types = types(src);
    let end = src.len() as u32;
    // The one the extension added, and the one the builtin already had.
    assert_eq!(types.of_path("xs.second", end).class_name(), Some("function"));
    assert_eq!(types.of_path("xs.sort()", end).class_name(), Some("list"));
}

#[test]
fn a_name_a_module_does_not_declare_is_not_guessed_at() {
    let src = "import math\nlet a = math.nosuch\n";
    assert_eq!(of(src, "a"), Type::Unknown);
}

#[test]
fn a_dotted_path_is_followed_a_segment_at_a_time() {
    let src = "class Inner {\n  op init() { self.n = 1 }\n}\nclass Outer {\n  op init() { self.inner = Inner() }\n}\nlet o = Outer()\n";
    let types = types(src);
    let end = src.len() as u32;
    assert_eq!(types.of_path("o", end).class_name(), Some("Outer"));
    assert_eq!(types.of_path("o.inner", end).class_name(), Some("Inner"));
    assert_eq!(types.of_path("o.inner.n", end).class_name(), Some("int"));
    assert_eq!(types.of_path("o.inner.nope", end), Type::Unknown);
}

#[test]
fn a_path_tells_a_call_from_a_name() {
    // The parentheses are the whole of the difference, which is why the
    // caller has to keep them: `Box` is a class, `Box()` is a `Box`,
    // `b.twin` is the method, and `b.twin()` is another `Box`.
    let src = "class Box {\n  op init() { self.n = 1 }\n  fn twin() { return Box() }\n}\nlet b = Box()\n";
    let types = types(src);
    let end = src.len() as u32;
    // A class object reaches what its instances have, because the
    // language lets it: `print(Box.twin)` writes `<fn twin>`. What it does
    // not reach is a field, which only an instance ever assigned.
    assert_eq!(types.of_path("Box", end).class_name(), Some("Box"));
    assert!(types.names_a_class("Box", end));
    assert!(!types.names_a_class("Box()", end));
    assert!(!types.names_a_class("b", end));
    assert_eq!(types.of_path("Box()", end).class_name(), Some("Box"));
    assert_eq!(types.of_path("b.twin", end).class_name(), Some("function"));
    assert_eq!(types.of_path("b.twin()", end).class_name(), Some("Box"));
    assert_eq!(types.of_path("b.twin().n", end).class_name(), Some("int"));
}

#[test]
fn a_path_through_a_function_call_is_followed_too() {
    let src = "class Box {\n  op init() { self.n = 1 }\n}\nfn make() { return Box() }\n";
    let types = types(src);
    let end = src.len() as u32;
    assert_eq!(types.of_path("make()", end).class_name(), Some("Box"));
    assert_eq!(types.of_path("make().n", end).class_name(), Some("int"));
    assert_eq!(types.of_path("string()", end).class_name(), Some("string"));
}

#[test]
fn an_extension_adds_methods_the_pass_can_see() {
    let src = "class Box {\n  op init() { self.n = 1 }\n}\nextend Box {\n  fn tag() { return \"box\" }\n}\nlet b = Box()\nlet a = b.tag()\n";
    assert_eq!(class_of(src, "a").as_deref(), Some("string"));
}

#[test]
fn a_cycle_in_what_was_written_does_not_hang_the_walk() {
    // `extends` cycles are refused at run time rather than by the resolver,
    // so this pass can be handed one. The same guard the resolver's own
    // walk needed, for the same reason.
    let src = "class A extends B {\n  fn a() { return 1 }\n}\nclass B extends A {\n  fn b() { return 2 }\n}\n";
    let types = types(src);
    assert_eq!(types.of_field("A", "nothing"), Type::Unknown);
    assert!(types.has_method("A", "b"));
    assert!(!types.has_method("A", "nothing"));
}

#[test]
fn every_builtin_that_can_be_called_names_a_type() {
    // The constructors are read off `BUILTINS` rather than listed here, so
    // this pins that the reading agrees with the list: a type that can be
    // called is a type a call produces, and `nil` and `class` — keywords,
    // and so uncallable — are neither.
    for builtin in BUILTINS {
        let expected = builtin.conversion().is_some().then_some(builtin.name());
        assert_eq!(
            builtin_constructor(builtin.name()),
            expected,
            "{}",
            builtin.name()
        );
    }
    assert_eq!(builtin_constructor("nil"), None);
    assert_eq!(builtin_constructor("Point"), None);
}

#[test]
fn a_class_declaration_encloses_the_offsets_inside_it() {
    let src = "class A {\n    private let x = 1\n}\nlet a = A()\n";
    let types = types(src);
    let inside = src.find("private").expect("the field is written") as u32;
    assert_eq!(types.class_at(inside), Some("A"));
    // Past the closing brace is outside every class, which is what makes
    // top-level code an outsider.
    assert_eq!(types.class_at(src.len() as u32), None);
}

#[test]
fn what_may_be_offered_follows_where_the_cursor_is() {
    let src = "class Base {\n\
                   private let hidden = 1\n\
                   protected let shared = 2\n\
                   let open = 3\n\
               }\n\
               class Sub extends Base {\n\
                   fn m() {\n\
                       return 1\n\
                   }\n\
               }\n";
    let types = types(src);

    // Public reaches everywhere, including from nowhere in particular.
    assert!(types.may_offer(Visibility::Public, "Base", None));
    assert!(types.may_offer(Visibility::Public, "Base", Some("Sub")));

    // Outside every class, the two restricting words withhold.
    assert!(!types.may_offer(Visibility::Private, "Base", None));
    assert!(!types.may_offer(Visibility::Protected, "Base", None));

    // Inside the declaring class, everything.
    assert!(types.may_offer(Visibility::Private, "Base", Some("Base")));
    assert!(types.may_offer(Visibility::Protected, "Base", Some("Base")));

    // A subclass is the one row that separates the two words.
    assert!(!types.may_offer(Visibility::Private, "Base", Some("Sub")));
    assert!(types.may_offer(Visibility::Protected, "Base", Some("Sub")));

    // An unrelated class is outside, whichever word was written.
    assert!(!types.may_offer(Visibility::Protected, "Base", Some("Elsewhere")));
}

#[test]
fn a_declared_field_is_a_member_carrying_the_word_it_was_written_with() {
    let src = "class A {\n\
                   private let hidden = 1\n\
                   let open = 2\n\
                   op init() {\n\
                       self.invented = 3\n\
                   }\n\
               }\n";
    let members = types(src).members_of("A");
    let reach = |name: &str| {
        members
            .iter()
            .find(|symbol| symbol.name == name)
            .unwrap_or_else(|| panic!("`{name}` should be a member of `A`, got {members:?}"))
            .visibility
    };
    assert_eq!(reach("hidden"), Visibility::Private);
    assert_eq!(reach("open"), Visibility::Public);
    // A field an `init` assigned into existence was declared by nothing, so it
    // carries no word — and the default is the one that refuses nobody.
    assert_eq!(reach("invented"), Visibility::Public);
}

#[test]
fn an_is_guard_narrows_the_name_for_its_block() {
    let src = "fn probe(v: string?) {\n\
                   if v is string {\n\
                       let inside = v\n\
                   }\n\
                   let outside = v\n\
               }\n";
    let types = types(src);
    let at = |needle: &str| src.find(needle).expect("the marker is in the source") as u32;

    // Inside the guard `v` is a `string`; outside it is still `string?`, which
    // is what makes the narrowing worth having rather than a global claim.
    assert_eq!(types.of_name("v", at("let inside")), Type::class("string"));
    assert_eq!(
        types.of_name("v", at("let outside")),
        Type::class("string").nullable()
    );
}

#[test]
fn a_guard_narrows_through_the_left_of_an_and() {
    // `if x is string and len(x) > 0` is the form that makes a guard worth
    // writing, so narrowing that and not a bare `is` would be a strange rule.
    let src = "fn probe(v: string?) {\n\
                   if v is string and len(v) > 0 {\n\
                       let inside = v\n\
                   }\n\
               }\n";
    let at = src.find("let inside").expect("the marker is there") as u32;
    assert_eq!(types(src).of_name("v", at), Type::class("string"));
}

#[test]
fn a_coalesce_answers_the_type_that_survives_it() {
    // The left without its `nil`, joined with the right — so the whole
    // expression is the non-nullable type when both sides agree.
    let src = "fn probe(v: string?) {\n\
                   let name = v ?? \"anon\"\n\
                   let after = 1\n\
               }\n";
    let at = src.find("let after").expect("the marker is there") as u32;
    assert_eq!(types(src).of_name("name", at), Type::class("string"));
}

/// What the static check says about a program, as messages.
fn warnings(src: &str) -> Vec<String> {
    let program = crate::compile(src).expect("the test program compiles");
    let types = infer(&program);
    crate::sema::check::check(&program, &types)
        .iter()
        .map(|err| err.message.clone())
        .collect()
}

#[test]
fn a_literal_that_cannot_hold_is_reported_before_it_runs() {
    assert_eq!(
        warnings("let x: int = \"s\"\n"),
        vec!["`x` is `int`, but this is a string"]
    );
    assert_eq!(
        warnings("let x: int = nil\n"),
        vec!["`x` is `int`, which does not admit `nil`"]
    );
    // A field is a binding too.
    assert_eq!(
        warnings("class C {\n    let n: int = \"s\"\n}\n"),
        vec!["`n` is `int`, but this is a string"]
    );
}

#[test]
fn what_the_pass_cannot_decide_it_does_not_report() {
    // The whole design of the check: silence wherever it could be wrong, so a
    // squiggle that does appear is worth believing.
    let quiet = [
        // §4.1's widening.
        "let x: float = 1\n",
        // A nullable annotation holding its `nil`.
        "let x: int? = nil\n",
        // The top type.
        "let x: any = 1\n",
        "let x: _? = nil\n",
        // The pass cannot see a parameter's value, so it knows nothing here.
        "fn f(n) {\n    let x: int = n\n}\n",
        // A subclass holds as its parent.
        "class A {\n    op init() { }\n}\nclass B extends A {\n    op init() { }\n}\nlet x: A = B()\n",
    ];
    for src in quiet {
        assert_eq!(warnings(src), Vec::<String>::new(), "for {src:?}");
    }
}

#[test]
fn a_literals_elements_are_checked_against_the_annotation() {
    // The elements are right there in the source, so "the pass could be wrong"
    // is no defence — it can see exactly what is in the list.
    // Named the way the run-time check names it, because both are now looking
    // at the same thing: the element, not the joined type of all of them.
    assert_eq!(
        warnings("let xs: list[int] = [\"a\"]\n"),
        vec!["item 0 is `int`, but this is a string"]
    );
    assert_eq!(
        warnings("let d: dict[string, int] = {\"a\": \"b\"}\n"),
        vec!["the value is `int`, but this is a string"]
    );

    // One bad element among good ones. Asking what the elements *agree* on
    // answers "nothing" and says only that the pass cannot name the element
    // type — while the question that matters is whether each one fits.
    assert_eq!(
        warnings("let xs: list[int] = [1, \"a\"]\n"),
        vec!["item 1 is `int`, but this is a string"]
    );
    // Every one of them, not just the first.
    assert_eq!(
        warnings("let xs: list[int] = [\"a\", 2, \"c\"]\n"),
        vec![
            "item 0 is `int`, but this is a string",
            "item 2 is `int`, but this is a string",
        ]
    );

    let quiet = [
        // Agreeing.
        "let xs: list[int] = [1, 2]\n",
        // Widening, one level down.
        "let xs: list[float] = [1, 2]\n",
        // An empty literal says nothing about its elements.
        "let xs: list[int] = []\n",
        // The annotation says nothing about them either.
        "let xs: list = [\"a\"]\n",
        // A nullable element admits its `nil`.
        "let xs: list[int?] = [nil]\n",
        // Nesting compares all the way down, and agrees all the way down.
        "let xs: list[list[int]] = [[1], [2]]\n",
    ];
    for src in quiet {
        assert_eq!(warnings(src), Vec::<String>::new(), "for {src:?}");
    }

    // And it does disagree all the way down when it should.
    assert_eq!(
        warnings("let xs: list[list[int]] = [[\"a\"]]\n"),
        vec!["item 0 is `int`, but this is a string"]
    );
    // The `dict[K]` shorthand leaves values unconstrained, so only the keys are
    // asked about — §3.10, and the same rule the run-time check follows.
    assert_eq!(
        warnings("let d: dict[string] = {\"a\": nil, \"b\": 1}\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        warnings("let d: dict[string] = {1: \"x\"}\n"),
        vec!["the key is `string`, but this is an int"]
    );
}

#[test]
fn a_narrowing_int_is_told_how_to_choose() {
    assert_eq!(
        warnings("let n: int = 3.7\n"),
        vec!["`n` is `int`, but this is a float"]
    );
}


#[test]
fn a_mutation_of_a_typed_container_is_reported() {
    // The form people actually write: a list is built empty or short and then
    // filled, so `push` is where the wrong element usually arrives.
    assert_eq!(
        warnings("let xs: list[int] = [1, 2, 3]\nxs.push(\"test\")\n"),
        vec!["the item is `int`, but this is a string"]
    );
    assert_eq!(
        warnings("let xs: list[int] = []\nxs[0] = \"a\"\n"),
        vec!["the item is `int`, but this is a string"]
    );
    assert_eq!(
        warnings("let d: dict[string, int] = {}\nd[\"k\"] = \"v\"\n"),
        vec!["the value is `int`, but this is a string"]
    );

    let quiet = [
        "let xs: list[int] = []\nxs.push(4)\n",
        // Widening, as everywhere else.
        "let xs: list[float] = []\nxs.push(1)\n",
        // A container the pass cannot name the contents of.
        "let xs = []\nxs.push(\"anything\")\n",
        "let xs: list = []\nxs.push(\"anything\")\n",
        // `push` on something that is not a list at all is not this check's
        // business — the receiver is the mistake and it is reported elsewhere.
        "let n = 1\n",
    ];
    for src in quiet {
        assert_eq!(warnings(src), Vec::<String>::new(), "for {src:?}");
    }
}


#[test]
fn a_name_bound_once_is_reported_when_it_is_written_to() {
    // The resolver refuses this for a local and cannot see a global — each REPL
    // entry is its own compilation, so `final k = 1` and `k = 2` typed a line
    // apart are two programs. Inside one file they are one, which is the case
    // an editor has in hand.
    assert_eq!(
        warnings("final k = 1\nk = 2\n"),
        vec!["cannot reassign `k`"]
    );
    assert_eq!(
        warnings("const k = 1\nk = 2\n"),
        vec!["cannot reassign `k`"]
    );
    // A parameter is a binding the caller fills in, and takes the same words.
    assert_eq!(
        warnings("fn f(const n) {\n    n = 2\n}\n"),
        vec!["cannot reassign `n`"]
    );
    assert_eq!(
        warnings("fn f(final n: int) {\n    n = 2\n}\n"),
        vec!["cannot reassign `n`"]
    );
}

#[test]
fn mutating_what_const_froze_is_reported() {
    assert_eq!(
        warnings("const xs = [1]\nxs.push(2)\n"),
        vec!["cannot modify `xs`"]
    );
    assert_eq!(
        warnings("fn f(const xs: list[int]) {\n    xs.push(1)\n}\n"),
        vec!["cannot modify `xs`"]
    );
    // `final` is the other axis: the name is fixed and the object is not, so a
    // `final` list still grows.
    assert_eq!(warnings("final xs = [1]\nxs.push(2)\n"), Vec::<String>::new());
}

#[test]
fn shadowing_is_respected_by_the_binding_checks() {
    // A `let` inside a block must not inherit the `final` outside it. One false
    // squiggle costs more than ten missing ones, so this is the case that has
    // to work before the checks above are worth having at all.
    let quiet = [
        "final k = 1\nfn f() {\n    let k = 2\n    k = 3\n}\n",
        "fn f() {\n    let k = 1\n    k = 2\n}\nfinal k = 9\n",
        "const xs = [1]\nfn f() {\n    let xs = [2]\n    xs.push(3)\n}\n",
        // An unbound name says nothing — it is somebody else's mistake.
        "fn f(n) {\n    n = 2\n}\n",
    ];
    for src in quiet {
        assert_eq!(warnings(src), Vec::<String>::new(), "for {src:?}");
    }
}

#[test]
fn a_call_is_checked_against_the_parameters_it_names() {
    assert_eq!(
        warnings("fn f(n: int) {\n    return n\n}\nlet a = f(\"s\")\n"),
        vec!["`n` is `int`, but this is a string"]
    );
    // A method, with the receiver left out of the count.
    assert_eq!(
        warnings(
            "class C {\n    op init() { }\n    fn take(n: int) {\n        return n\n    }\n}\n\
             let c = C()\nlet a = c.take(\"s\")\n"
        ),
        vec!["`n` is `int`, but this is a string"]
    );
    // The library, whose parameters name a set of types.
    assert_eq!(
        warnings("let s = \"a,b\".split(5)\n"),
        vec!["`separator` is `string`, but this is an int"]
    );
    assert_eq!(
        warnings("from math import floor\nlet n = floor(\"x\")\n"),
        vec!["`n` is `int` or `float`, but this is a string"]
    );

    let quiet = [
        "fn f(n: int) {\n    return n\n}\nlet a = f(1)\n",
        // An unannotated parameter takes whatever it is handed.
        "fn f(n) {\n    return n\n}\nlet a = f(\"s\")\n",
        // Widening, one level down as everywhere.
        "fn f(n: float) {\n    return n\n}\nlet a = f(1)\n",
        "from math import floor\nlet n = floor(2)\n",
    ];
    for src in quiet {
        assert_eq!(warnings(src), Vec::<String>::new(), "for {src:?}");
    }
    assert_eq!(
        warnings("fn f(n: int) {\n    return n\n}\nlet a = f()\n"),
        vec!["expected 1 arguments, got 0"]
    );
}

#[test]
fn reaching_a_hidden_member_is_reported() {
    let src = "class Account {\n\
                   private let balance = 0\n\
                   protected let owner = \"nobody\"\n\
                   let open = 1\n\
               }\n\
               let a = Account()\n";
    assert_eq!(
        warnings(&format!("{src}let n = a.balance\n")),
        vec!["`balance` is private to `Account`"]
    );
    assert_eq!(
        warnings(&format!("{src}let n = a.owner\n")),
        vec!["`owner` is protected to `Account`"]
    );
    assert_eq!(warnings(&format!("{src}let n = a.open\n")), Vec::<String>::new());
}

#[test]
fn a_reassignment_is_checked_against_the_declarations_annotation() {
    assert_eq!(
        warnings("let x: int = 0\nx = \"s\"\n"),
        vec!["`x` is `int`, but this is a string"]
    );
    let quiet = [
        "let x: int = 0\nx = 5\n",
        // Widening.
        "let x: float = 0.0\nx = 5\n",
        // An unannotated `let` takes whatever it is given, and always could.
        "let x = 0\nx = \"s\"\n",
    ];
    for src in quiet {
        assert_eq!(warnings(src), Vec::<String>::new(), "for {src:?}");
    }
}

#[test]
fn an_unknown_type_annotation_is_reported() {
    assert_eq!(
        warnings("let x: Nonexistent = 1\n"),
        vec!["unknown type `Nonexistent`"]
    );
}

#[test]
fn extending_a_complete_class_is_reported() {
    assert_eq!(
        warnings("complete class Parser {\n  op init(s) { self.s = s }\n}\nextend Parser {\n  fn peek() { return self.s }\n}\n"),
        vec!["cannot extend complete class `Parser`"]
    );
}

#[test]
fn modifying_a_field_on_a_const_variable_is_reported() {
    assert_eq!(
        warnings("class Point {\n  op init(x) { self.x = x }\n}\nconst p = Point(1)\np.x = 2\n"),
        vec!["cannot modify field of `p`"]
    );
}

#[test]
fn extension_overriding_existing_method_is_reported() {
    assert_eq!(
        warnings("class Point {\n  op init(x) { self.x = x }\n  fn move() { }\n}\nextend Point {\n  fn move() { }\n}\n"),
        vec!["`move` is already a method of `Point`"]
    );
}

fn all_errors(src: &str) -> Vec<String> {
    let (stmts, mut errors) = crate::compile_recovering(src);
    if !stmts.is_empty() {
        let types = infer(&stmts);
        errors.extend(crate::sema::check::check(&stmts, &types));
    }
    errors.iter().map(|err| err.message.clone()).collect()
}

#[test]
fn inheriting_from_final_class_is_refused() {
    let src = "final class Money { op init(cents) { self.cents = cents } }\nclass Debt extends Money {}\n";
    assert_eq!(all_errors(src), vec!["cannot inherit from final class `Money`"]);
}

#[test]
fn import_unknown_member_is_refused() {
    let src = "from math import floor, florr\n";
    assert_eq!(all_errors(src), vec!["module `math` has no member `florr`"]);
}

#[test]
fn invalid_in_operator_is_refused() {
    let src = "print(1 in 2)\n";
    assert_eq!(all_errors(src), vec!["`in` is not supported on an int"]);
}

#[test]
fn malformed_generic_type_annotation_is_refused() {
    let src = "let xs: list[int, string] = []\n";
    assert_eq!(all_errors(src), vec!["`list` takes 1 argument, but 2 were written"]);
}

#[test]
fn missing_return_value_is_refused() {
    let src = "fn g(): int {}\ng()\n";
    assert_eq!(all_errors(src), vec!["function `g` declares return type `int` but might return without a value"]);
}

#[test]
fn arity_mismatch_is_refused() {
    let src = "fn f(a, b) { return a }\nprint(f(1))\n";
    assert_eq!(all_errors(src), vec!["expected 2 arguments, got 1"]);
}

#[test]
fn unhashable_dict_key_is_refused() {
    let src = "class Point { op init() {} }\nlet d: dict[Point, int] = {}\n";
    assert_eq!(all_errors(src), vec!["`Point` cannot be a dict key"]);
}

#[test]
fn nested_const_modification_is_refused() {
    let src = "const xs = [1, [2, 3]]\nxs[1].push(9)\n";
    assert_eq!(all_errors(src), vec!["cannot modify `xs`"]);
}

#[test]
fn op_string_invalid_return_type_is_refused() {
    let src = "class Bad { op init() {} op string() { return 42 } }\n";
    assert_eq!(all_errors(src), vec!["`op string` must return a string"]);
}

#[test]
fn op_len_invalid_return_type_is_refused() {
    let src = "class Sized { op len() { return \"three\" } }\n";
    assert_eq!(all_errors(src), vec!["`op len` must return an int"]);
}

#[test]
fn op_iter_invalid_return_type_is_refused() {
    let src = "class Countdown { op iter() { return {\"a\": 1} } }\n";
    assert_eq!(all_errors(src), vec!["`op iter` must return a list"]);
}

#[test]
fn op_int_invalid_return_type_is_refused() {
    let src = "class Bad { op int() { return \"nope\" } }\n";
    assert_eq!(all_errors(src), vec!["`op int` must return an int"]);
}

#[test]
fn le_operator_without_cmp_is_refused() {
    let src = "class Version { op init(n) { self.n = n } op lt(other) { return self.n < other.n } }\nlet a = Version(1) <= Version(2)\n";
    assert_eq!(all_errors(src), vec!["`<=` is not supported on `Version`"]);
}

#[test]
fn reflected_comparison_without_cmp_is_refused() {
    let src = "class Rank { op init(n) { self.n = n } op lt(other) { return self.n < other.n } }\nlet a = 1 < Rank(2)\n";
    assert_eq!(all_errors(src), vec!["`<` is not supported between an int and `Rank`"]);
}

#[test]
fn div_zero_is_refused() {
    let src = "print(1 / 0)\n";
    assert_eq!(all_errors(src), vec!["division by zero"]);
}

#[test]
fn bitwise_float_is_refused() {
    let src = "print(1.5 & 2)\n";
    assert_eq!(all_errors(src), vec!["bitwise operators are not supported on float"]);
}

#[test]
fn extended_float_bitwise_is_allowed() {
    let src = "extend float { op bit_and(val) { return int(self) & val } }\nprint(1.5 & 1)\n";
    assert_eq!(all_errors(src), Vec::<String>::new());
}

#[test]
fn extend_native_op_override_is_refused() {
    let src = "extend string { op add(val) { return self } }\n";
    assert_eq!(
        all_errors(src),
        vec!["`string` natively supports `op add` and cannot be overridden by an extension"]
    );
}

#[test]
fn primitive_incompatible_binary_op_is_refused() {
    let src = "print(1 + \"str\")\n";
    assert_eq!(all_errors(src), vec!["`+` is not supported between an int and a string"]);
}

#[test]
fn user_class_unsupported_binary_op_is_refused() {
    let src = "class Point3D { op init() {} }\nlet p3d = Point3D()\nprint(p3d - p3d)\n";
    assert_eq!(all_errors(src), vec!["`-` is not supported for `Point3D`"]);
}

#[test]
fn return_type_annotation_mismatch_is_refused() {
    let src = "class Point3D { op init() {} }\nextend Point3D { op add(other) : float { return self } }\n";
    assert_eq!(all_errors(src), vec!["`add`’s return is `float`, but this is a Point3D"]);
}
#[test]
fn asymmetric_arithmetic_is_refused() {
    let src = "class Money { op init(c) { self.c = c } }\nprint(500 - Money(200))\n";
    assert_eq!(all_errors(src), vec!["`-` is not supported between an int and `Money`"]);
}

#[test]
fn extends_function_is_refused() {
    let src = "class Callable extends function {}\n";
    assert_eq!(all_errors(src), vec!["cannot inherit from builtin type `function`"]);
}

#[test]
fn extends_self_is_refused() {
    let src = "class A extends A {}\n";
    assert_eq!(all_errors(src), vec!["class `A` cannot inherit from itself"]);
}

#[test]
fn extends_variable_is_refused() {
    let src = "let n = 1\nclass C extends n {}\n";
    assert_eq!(all_errors(src), vec!["cannot inherit from variable `n`"]);
}

#[test]
fn op_eq_invalid_return_type_is_refused() {
    let src = "class Sloppy { op init(n) { self.n = n } op eq(other) { return 1 } }\n";
    assert_eq!(all_errors(src), vec!["`op eq` must return a bool"]);
}

#[test]
fn op_cmp_invalid_return_type_is_refused() {
    let src = "class Weight { op init(kg) { self.kg = kg } op cmp(other) { return 1.5 } }\n";
    assert_eq!(all_errors(src), vec!["`op cmp` must return an int"]);
}

#[test]
fn op_bool_invalid_return_type_is_refused() {
    let src = "class Bad { op init() {} op bool() { return [1, 2] } }\n";
    assert_eq!(all_errors(src), vec!["`op bool` must return a bool"]);
}

#[test]
fn non_callable_invocation_is_refused() {
    let src = "let x = 1\nx()\n";
    assert_eq!(all_errors(src), vec!["an int is not callable"]);
}

#[test]
fn construct_function_is_refused() {
    let src = "function(print)\n";
    assert_eq!(all_errors(src), vec!["builtin type `function` cannot be instantiated"]);
}

#[test]
fn field_on_builtin_is_refused() {
    let src = "let n = 1\nn.field = 2\n";
    assert_eq!(all_errors(src), vec!["`int` has no method `field`"]);
}

#[test]
fn non_existent_builtin_method_is_refused() {
    let src = "let xs = [1]\nxs.nope()\n";
    assert_eq!(all_errors(src), vec!["`list` has no method `nope`"]);
}

#[test]
fn slice_non_sequence_is_refused() {
    let src = "let n = 5\nprint(n[1:2])\n";
    assert_eq!(all_errors(src), vec!["cannot slice an int"]);
}

#[test]
fn throw_non_error_is_refused() {
    let src = "throw 10\n";
    assert_eq!(all_errors(src), vec!["cannot throw an int"]);
}

#[test]
fn custom_module_import_names_resolution() {
    let gates_src = "class Had {\n  op init(target) { self.target = target }\n  fn apply(state) { return state }\n}\nfn create_engine(num_qubits) { return Had(0) }\n";
    let main_src = "from gates import Had, create_engine\nlet h = Had(1)\nlet eng = create_engine(2)\n";

    let gates_stmts = crate::compile(gates_src).expect("gates compiles");
    let main_stmts = crate::compile(main_src).expect("main compiles");

    let resolver = |name: &str| {
        if name == "gates" {
            Some(gates_stmts.clone())
        } else {
            None
        }
    };

    let types = infer_with_resolver(&main_stmts, &resolver);
    let end = main_src.len() as u32;

    assert_eq!(types.of_name("h", end).class_name(), Some("Had"));
    assert_eq!(types.of_name("eng", end).class_name(), Some("Had"));

    // Check that Had's methods were transferred to main's scope
    assert!(types.has_method("Had", "apply"));

    // Check fn_decl for create_engine
    let fn_decl = types.function("create_engine").expect("fn decl registered");
    assert_eq!(fn_decl.name, "create_engine");
    assert_eq!(fn_decl.params.len(), 1);
    assert_eq!(fn_decl.params[0].name, "num_qubits");
}

#[test]
fn custom_module_dot_import_resolution() {
    let gates_src = "class Had {\n  op init(target) { self.target = target }\n}\nfn create_engine(num_qubits) { return Had(0) }\n";
    let main_src = "import gates\nlet h = gates.Had(1)\nlet eng = gates.create_engine(2)\n";

    let gates_stmts = crate::compile(gates_src).expect("gates compiles");
    let main_stmts = crate::compile(main_src).expect("main compiles");

    let resolver = |name: &str| {
        if name == "gates" {
            Some(gates_stmts.clone())
        } else {
            None
        }
    };

    let types = infer_with_resolver(&main_stmts, &resolver);
    let end = main_src.len() as u32;

    assert_eq!(types.of_name("gates", end), Type::module("gates"));
    assert_eq!(types.of_path("gates.Had()", end).class_name(), Some("Had"));
    assert_eq!(types.of_path("gates.create_engine()", end).class_name(), Some("Had"));

    let symbols = types.module_symbols("gates");
    assert!(symbols.iter().any(|s| s.name == "Had" && s.kind == Kind::Class));
    assert!(symbols.iter().any(|s| s.name == "create_engine" && s.kind == Kind::Function));
}



#[test]
fn overloaded_op_is_not_reported_missing() {
    let src = "class V { op init() {}\nconst op sub(other: V): V { return self }\nconst op sub(scalar: float): V { return self } }\nlet a = V()\nlet d = a - a\n";
    assert_eq!(all_errors(src), Vec::<String>::new());
}

#[test]
fn overloaded_cmp_still_permits_le() {
    let src = "class C { op init() {}\nconst op cmp(other: C): int { return 0 }\nconst op cmp(other: float): int { return 0 }\nconst op lt(other: C): bool { return false } }\nlet a = C()\nlet b = C()\nlet r = a <= b\n";
    assert_eq!(all_errors(src), Vec::<String>::new());
}




