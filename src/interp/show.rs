//! How a value prints.
//!
//! On [`Interp`] rather than on `Value`, because printing is a question a class
//! is allowed to answer: `op string` is ordinary Quince code, so rendering can
//! allocate, run a program, and fail. That is also why everything here returns a
//! `Result` while nothing in it can yet produce an `Err` — the shape is what the
//! slots need, and they are wired one step from here.
//!
//! There is a second, smaller renderer on `Value` — the `_base` family — for
//! error messages, which must never ask a class anything. The two have to agree
//! wherever no class has overridden anything, and
//! `the_base_renderer_agrees_with_this_one` is what says so.
//!
//! Every arm matches on a *clone* of the base value rather than a borrow of it.
//! Recursion here needs `&mut self`, and a borrow of the heap taken to look at a
//! value cannot be held across that.

use crate::color::Style;
use crate::error::Result;
use crate::interp::Interp;
use crate::runtime::value::Value;
use crate::syntax::ast::Op;

/// How wide a collection may print before it is broken over lines.
const MAX_WIDTH: usize = 80;

/// One indent level, as it appears in pretty output.
const INDENT: &str = "    ";

/// Whether a class may say how its own values print.
///
/// Two callers need `Nothing`, and for the same reason: they are the tools you
/// reach for when an `op string` is what went wrong. `:vars` lists the
/// environment, and an error message names a value — if either ran the op, a
/// broken one would break the thing you were using to find it.
///
/// Passed explicitly rather than defaulted, so that every site states which it
/// is asking for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Reach `op string`, which is how `print(x)` behaves.
    Class,
    /// Render structurally, asking nothing.
    Nothing,
}

impl Interp {
    /// How a value prints, which a class may answer with `op string`.
    pub fn display(&mut self, value: &Value, ask: Ask) -> Result<String> {
        self.display_styled(value, false, ask)
    }

    /// How a value prints, with optional ANSI syntax highlighting.
    pub fn display_styled(
        &mut self,
        value: &Value,
        color: bool,
        ask: Ask,
    ) -> Result<String> {
        // Asked before the payload is unwrapped, because the class is what holds
        // the answer: an `op string` on a subclass of `string` has to beat the
        // string it carries, or declaring it would do nothing.
        if ask == Ask::Class
            && let Some(method) = self.slot(value, Op::Str)
        {
            let answer = self.call_op(method, value, Vec::new())?;
            return match answer.base(&self.heap) {
                // Used verbatim, unpainted and unquoted, in every position — see
                // `repr_styled`. The class said how it prints, and the language
                // editing that text would be the language disagreeing.
                Value::Str(text) => Ok(text.to_string()),
                other => Err(self.op_returned(Op::Str, value, "a string", other)),
            };
        }

        let base = value.base(&self.heap).clone();
        let text = match &base {
            Value::Nil => Style::DIM.paint("nil", color),
            Value::Bool(b) => Style::YELLOW.paint(b, color),
            Value::Int(n) => Style::CYAN.paint(n, color),
            // Keeps floats distinguishable from ints in output: `1.0`, not `1`.
            Value::Float(n) if n.fract() == 0.0 && n.is_finite() => {
                Style::CYAN.paint(format!("{n:.1}"), color)
            }
            Value::Float(n) => Style::CYAN.paint(n, color),
            Value::Str(s) => Style::GREEN.paint(s, color),
            Value::List(id) => {
                let id = *id;
                let mut items = Vec::new();
                let mut i = 0;
                while let Some(item) = self.heap.list(id).get(i).cloned() {
                    items.push(self.repr_styled(&item, color, ask)?);
                    i += 1;
                }
                format!(
                    "{}{}{}",
                    Style::BOLD.paint("[", color),
                    items.join(", "),
                    Style::BOLD.paint("]", color)
                )
            }
            Value::Dict(id) => {
                let mut entries = Vec::new();
                for (key, value) in self.entries(*id) {
                    entries.push(format!(
                        "{}: {}",
                        self.repr_styled(&key, color, ask)?,
                        self.repr_styled(&value, color, ask)?
                    ));
                }
                format!(
                    "{}{}{}",
                    Style::BOLD.paint("{", color),
                    entries.join(", "),
                    Style::BOLD.paint("}", color)
                )
            }
            Value::Function(id) => Style::MAGENTA
                .paint(format!("<fn {}>", self.heap.function(*id).decl.name), color),
            // The name and the count, because the candidates share the name and
            // how many there are is the only other thing worth saying.
            Value::Overload(id) => Style::MAGENTA.paint(
                format!(
                    "<fn {} and {} more>",
                    base.callable_name(&self.heap),
                    self.heap.overload(*id).len().saturating_sub(1)
                ),
                color,
            ),
            Value::Native(native) => {
                Style::MAGENTA.paint(format!("<builtin {}>", native.name), color)
            }
            Value::BoundMethod(id) => {
                let bound = self.heap.bound_method(*id);
                Style::MAGENTA.paint(
                    format!(
                        "<method {} of {}>",
                        bound.method.callable_name(&self.heap),
                        bound.receiver.type_name(&self.heap)
                    ),
                    color,
                )
            }
            Value::Class(id) => {
                Style::MAGENTA.paint(format!("<class {}>", self.heap.class(*id).name), color)
            }
            // Not `base`'s type name but the value's: an instance carrying no
            // payload is its own base, and what it should say is its class.
            Value::Instance(_) => {
                Style::MAGENTA.paint(format!("<{} instance>", value.type_name(&self.heap)), color)
            }
            Value::Module(id) => {
                let text = match self.heap.globals(*id).name() {
                    Some(name) => format!("<module {name}>"),
                    None => "<module>".to_string(),
                };
                Style::MAGENTA.paint(text, color)
            }
        };
        Ok(text)
    }

    /// How a value prints inside a collection, where a string needs quoting to
    /// stay distinguishable from a bare identifier.
    pub fn repr_styled(
        &mut self,
        value: &Value,
        color: bool,
        ask: Ask,
    ) -> Result<String> {
        // A class that says how it prints says so in every position, so the
        // quoting below is reached only when nothing answered. Checked before the
        // payload for the same reason `display_styled` checks first: a class
        // extending `string` would otherwise have its `op string` beaten by the
        // string it carries, and inside a list it would come back quoted — the
        // one place the rule "the class's text is used verbatim" would break.
        if ask == Ask::Class && self.slot(value, Op::Str).is_some() {
            return self.display_styled(value, color, ask);
        }

        // A payload-carrying instance reprs as its base type, so `[Username("marc")]`
        // shows `["marc"]`. Nothing in the output distinguishes it from a plain
        // string, which is the same trade Python makes: `repr` stays the literal you
        // would write, and `type(x)` is how you ask what class it is.
        match value.base(&self.heap) {
            Value::Str(s) => Ok(Style::GREEN.paint(format!("{s:?}"), color)),
            _ => self.display_styled(value, color, ask),
        }
    }

    /// How a value prints when the REPL echoes it, where a collection too wide
    /// for one line is broken over several.
    pub fn display_pretty(
        &mut self,
        value: &Value,
        color: bool,
        ask: Ask,
    ) -> Result<String> {
        let unstyled = self.display_styled(value, false, ask)?;
        if unstyled.len() <= MAX_WIDTH && !unstyled.contains('\n') {
            return self.display_styled(value, color, ask);
        }
        match value.base(&self.heap) {
            Value::List(_) | Value::Dict(_) => self.format_pretty(value, color, 0, ask),
            _ => self.display_styled(value, color, ask),
        }
    }

    fn format_pretty(
        &mut self,
        value: &Value,
        color: bool,
        indent: usize,
        ask: Ask,
    ) -> Result<String> {
        let pad = INDENT.repeat(indent);
        let inner_pad = INDENT.repeat(indent + 1);
        let base = value.base(&self.heap).clone();
        match &base {
            Value::List(id) => {
                let id = *id;
                let items: Vec<Value> = self.heap.list(id).clone();
                if items.is_empty() {
                    return Ok(format!(
                        "{}{}",
                        Style::BOLD.paint("[", color),
                        Style::BOLD.paint("]", color)
                    ));
                }
                let is_flat = items
                    .iter()
                    .all(|item| !matches!(item.base(&self.heap), Value::List(_) | Value::Dict(_)));

                if is_flat {
                    // Several items to a line, wrapped at the same width that
                    // decided to break in the first place, so a long list of small
                    // values stays readable instead of becoming one item per line.
                    let mut lines = Vec::new();
                    let mut current_line = String::from(&inner_pad);
                    let mut current_len = inner_pad.len();

                    for (i, item) in items.iter().enumerate() {
                        // Measured unstyled and printed styled: ANSI escapes take
                        // no width on screen, so counting them would wrap early.
                        let item_unstyled = self.repr_base(item);
                        let item_styled = self.repr_styled(item, color, ask)?;
                        let comma = if i + 1 < items.len() { "," } else { "" };
                        let sep_len = if i + 1 < items.len() { 2 } else { 0 };

                        if current_len > inner_pad.len()
                            && current_len + item_unstyled.len() + comma.len() > MAX_WIDTH
                        {
                            lines.push(current_line);
                            current_line = format!("{inner_pad}{item_styled}{comma}");
                            current_len = inner_pad.len() + item_unstyled.len() + comma.len();
                        } else {
                            if current_len > inner_pad.len() {
                                current_line.push(' ');
                            }
                            current_line.push_str(&item_styled);
                            if !comma.is_empty() {
                                current_line.push(',');
                            }
                            current_len += item_unstyled.len() + sep_len;
                        }
                    }
                    if !current_line.is_empty() {
                        lines.push(current_line);
                    }

                    Ok(format!(
                        "{}\n{}\n{}{}",
                        Style::BOLD.paint("[", color),
                        lines.join("\n"),
                        pad,
                        Style::BOLD.paint("]", color)
                    ))
                } else {
                    let mut lines = Vec::new();
                    for item in &items {
                        let formatted = match item.base(&self.heap) {
                            Value::List(_) | Value::Dict(_) => {
                                self.format_pretty(item, color, indent + 1, ask)?
                            }
                            _ => format!("{inner_pad}{}", self.repr_styled(item, color, ask)?),
                        };
                        lines.push(formatted);
                    }
                    Ok(format!(
                        "{}\n{}\n{}{}",
                        Style::BOLD.paint("[", color),
                        lines.join(",\n"),
                        pad,
                        Style::BOLD.paint("]", color)
                    ))
                }
            }
            Value::Dict(id) => {
                let entries = self.entries(*id);
                if entries.is_empty() {
                    return Ok(format!(
                        "{}{}",
                        Style::BOLD.paint("{", color),
                        Style::BOLD.paint("}", color)
                    ));
                }
                let mut lines = Vec::new();
                for (key, val) in &entries {
                    let key_str = self.repr_styled(key, color, ask)?;
                    let val_str = match val.base(&self.heap) {
                        Value::List(_) | Value::Dict(_) => {
                            self.format_pretty(val, color, indent + 1, ask)?
                        }
                        _ => self.repr_styled(val, color, ask)?,
                    };
                    lines.push(format!("{inner_pad}{key_str}: {val_str}"));
                }
                Ok(format!(
                    "{}\n{}\n{}{}",
                    Style::BOLD.paint("{", color),
                    lines.join(",\n"),
                    pad,
                    Style::BOLD.paint("}", color)
                ))
            }
            _ => Ok(format!("{pad}{}", self.display_styled(value, color, ask)?)),
        }
    }

    /// A dict's entries as values, detached from the heap.
    ///
    /// Cloned for the same reason everything here is: rendering an entry can call
    /// back into the interpreter, and a borrow of the dict cannot survive that.
    fn entries(&self, id: crate::runtime::heap::ObjId) -> Vec<(Value, Value)> {
        self.heap
            .dict(id)
            .iter()
            .map(|(key, value)| (key.to_value(), value.clone()))
            .collect()
    }

    /// The unstyled structural rendering, for measuring width.
    fn repr_base(&self, value: &Value) -> String {
        value.repr_base(&self.heap)
    }
}

#[cfg(test)]
mod tests {
    use super::Ask;
    use crate::runtime::heap::Object;
    use crate::interp::Interp;
    use crate::runtime::value::Value;

    /// Values covering every arm, including nesting, for comparing the two
    /// renderers against each other.
    fn every_shape(interp: &mut Interp) -> Vec<Value> {
        let inner = Value::List(interp.heap.alloc(Object::List(vec![
            Value::Int(1),
            Value::from("two"),
        ])));
        let nested = Value::List(interp.heap.alloc(Object::List(vec![
            inner.clone(),
            Value::Nil,
        ])));
        let mut dict = crate::runtime::dict::Dict::new();
        dict.insert(crate::runtime::dict::Key::Str(std::rc::Rc::from("k")), inner.clone());
        let dict = Value::Dict(interp.heap.alloc(Object::Dict(dict)));
        vec![
            Value::Nil,
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(0),
            Value::Int(-7),
            Value::Float(1.0),
            Value::Float(1.5),
            Value::from(""),
            Value::from("hi"),
            Value::from("with \"quotes\""),
            inner,
            nested,
            dict,
            Value::List(interp.heap.alloc(Object::List(Vec::new()))),
        ]
    }

    /// The renderer error messages use has to agree with the real one.
    ///
    /// They are separate implementations on purpose — one can ask a class and the
    /// other must not — and the cost of that is exactly this: two places that
    /// decide how a float or an empty list looks. A test is the only thing making
    /// them one answer.
    #[test]
    fn the_base_renderer_agrees_with_this_one() {
        let mut interp = Interp::new();
        for value in every_shape(&mut interp) {
            let base = value.display_base(&interp.heap);
            let real = interp
                .display(&value, Ask::Class)
                .expect("no class is involved");
            assert_eq!(base, real, "display disagrees for {value:?}");

            let base = value.repr_base(&interp.heap);
            let real = interp
                .repr_styled(&value, false, Ask::Class)
                .expect("no class is involved");
            assert_eq!(base, real, "repr disagrees for {value:?}");
        }
    }

    #[test]
    fn short_character_lists_print_on_single_line() {
        let mut interp = Interp::new();
        let items: Vec<Value> = "marc@gmail.com"
            .chars()
            .map(|c| Value::from(c.to_string().as_str()))
            .collect();
        let list = Value::List(interp.heap.alloc(Object::List(items)));

        let printed = interp
            .display_pretty(&list, false, Ask::Class)
            .expect("no class");
        assert_eq!(
            printed,
            r#"["m", "a", "r", "c", "@", "g", "m", "a", "i", "l", ".", "c", "o", "m"]"#
        );
    }
}
