//! Length values: specified lengths in any unit (including `calc()`, `min()`, `max()`
//! and `clamp()`), and their computed form, a linear combination of px and %.

use super::parser::ComponentValue;
use super::tokenizer::Token;
use std::rc::Rc;

/// A specified length as a linear combination of units. `calc()` expressions that only
/// use `+`, `-`, and multiplication/division by numbers reduce to this form.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LinearLength {
    /// Absolute px (pt, in, cm, ... are converted at parse time).
    pub px: f32,
    /// `em` (relative to the element's font size; parent's for `font-size` itself).
    pub em: f32,
    /// `rem`
    pub rem: f32,
    /// `ex` (approximated as 0.5em)
    pub ex: f32,
    /// `ch` (approximated as 0.5em)
    pub ch: f32,
    /// `vw`
    pub vw: f32,
    /// `vh`
    pub vh: f32,
    /// `vmin`
    pub vmin: f32,
    /// `vmax`
    pub vmax: f32,
    /// Percentage (of a property-specific basis).
    pub pct: f32,
}

impl LinearLength {
    /// A pure px length.
    pub fn px(v: f32) -> Self {
        LinearLength {
            px: v,
            ..Default::default()
        }
    }

    /// A pure percentage.
    pub fn pct(v: f32) -> Self {
        LinearLength {
            pct: v,
            ..Default::default()
        }
    }

    fn add(self, o: Self, sign: f32) -> Self {
        LinearLength {
            px: self.px + sign * o.px,
            em: self.em + sign * o.em,
            rem: self.rem + sign * o.rem,
            ex: self.ex + sign * o.ex,
            ch: self.ch + sign * o.ch,
            vw: self.vw + sign * o.vw,
            vh: self.vh + sign * o.vh,
            vmin: self.vmin + sign * o.vmin,
            vmax: self.vmax + sign * o.vmax,
            pct: self.pct + sign * o.pct,
        }
    }

    fn scale(self, k: f32) -> Self {
        LinearLength {
            px: self.px * k,
            em: self.em * k,
            rem: self.rem * k,
            ex: self.ex * k,
            ch: self.ch * k,
            vw: self.vw * k,
            vh: self.vh * k,
            vmin: self.vmin * k,
            vmax: self.vmax * k,
            pct: self.pct * k,
        }
    }

    /// Whether this is not a plain negative value. Mixed `calc()` expressions such as
    /// `calc(100% - 3em)` are accepted (they are clamped at used-value time).
    pub fn is_non_negative(&self) -> bool {
        let parts = [
            self.px, self.em, self.rem, self.ex, self.ch, self.vw, self.vh, self.vmin, self.vmax, self.pct,
        ];
        let nonzero: Vec<f32> = parts.into_iter().filter(|v| *v != 0.0).collect();
        !(nonzero.len() == 1 && nonzero[0] < 0.0)
    }

    /// Resolves font- and viewport-relative units, leaving `px + pct%`.
    pub fn compute(&self, ctx: &LengthContext) -> Lin {
        let vmin = ctx.viewport_width.min(ctx.viewport_height);
        let vmax = ctx.viewport_width.max(ctx.viewport_height);
        Lin {
            px: self.px
                + (self.em + 0.5 * self.ex + 0.5 * self.ch) * ctx.font_size
                + self.rem * ctx.root_font_size
                + self.vw * ctx.viewport_width / 100.0
                + self.vh * ctx.viewport_height / 100.0
                + self.vmin * vmin / 100.0
                + self.vmax * vmax / 100.0,
            pct: self.pct,
        }
    }
}

/// Inputs for resolving relative length units.
#[derive(Clone, Copy, Debug)]
pub struct LengthContext {
    /// Font size used for `em` (px).
    pub font_size: f32,
    /// Root element font size used for `rem` (px).
    pub root_font_size: f32,
    /// Viewport width (px).
    pub viewport_width: f32,
    /// Viewport height (px).
    pub viewport_height: f32,
}

/// A specified length-percentage.
#[derive(Clone, Debug, PartialEq)]
pub enum SpecifiedLength {
    /// A (possibly calc-reduced) linear combination.
    Linear(LinearLength),
    /// `min(a, b, ...)`
    Min(Vec<SpecifiedLength>),
    /// `max(a, b, ...)`
    Max(Vec<SpecifiedLength>),
    /// `clamp(min, val, max)`
    Clamp(Box<[SpecifiedLength; 3]>),
}

impl SpecifiedLength {
    /// A px length.
    pub fn px(v: f32) -> Self {
        SpecifiedLength::Linear(LinearLength::px(v))
    }

    /// Computes to a [`LengthPercentage`].
    pub fn compute(&self, ctx: &LengthContext) -> LengthPercentage {
        match self {
            SpecifiedLength::Linear(l) => {
                let c = l.compute(ctx);
                LengthPercentage::Lin(c)
            }
            SpecifiedLength::Min(v) => combine(v.iter().map(|x| x.compute(ctx)).collect(), CalcOp::Min),
            SpecifiedLength::Max(v) => combine(v.iter().map(|x| x.compute(ctx)).collect(), CalcOp::Max),
            SpecifiedLength::Clamp(b) => {
                let [lo, val, hi] = &**b;
                let inner = combine(vec![val.compute(ctx), hi.compute(ctx)], CalcOp::Min);
                combine(vec![lo.compute(ctx), inner], CalcOp::Max)
            }
        }
    }

    /// Whether this is a plain non-negative value (calc trees are accepted).
    pub fn is_non_negative(&self) -> bool {
        match self {
            SpecifiedLength::Linear(l) => l.is_non_negative(),
            _ => true,
        }
    }
}

fn combine(items: Vec<LengthPercentage>, op: CalcOp) -> LengthPercentage {
    // If no item depends on a percentage, evaluate now.
    if items
        .iter()
        .all(|i| matches!(i, LengthPercentage::Lin(l) if l.pct == 0.0))
    {
        let vals = items.iter().map(|i| i.resolve(0.0));
        let v = match op {
            CalcOp::Min => vals.fold(f32::INFINITY, f32::min),
            CalcOp::Max => vals.fold(f32::NEG_INFINITY, f32::max),
        };
        return LengthPercentage::px(v);
    }
    LengthPercentage::Calc(Rc::new(CalcNode { op, items }))
}

/// `px + pct%`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Lin {
    /// Absolute part.
    pub px: f32,
    /// Percentage part (in percent units).
    pub pct: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum CalcOp {
    Min,
    Max,
}

/// A `min()`/`max()` node whose operands depend on a percentage basis.
#[derive(Clone, Debug, PartialEq)]
pub struct CalcNode {
    op: CalcOp,
    items: Vec<LengthPercentage>,
}

/// A computed length-percentage, resolved against a basis during layout.
#[derive(Clone, Debug, PartialEq)]
pub enum LengthPercentage {
    /// `px + pct% * basis`
    Lin(Lin),
    /// A min/max expression involving percentages.
    Calc(Rc<CalcNode>),
}

impl Default for LengthPercentage {
    fn default() -> Self {
        LengthPercentage::px(0.0)
    }
}

impl LengthPercentage {
    /// A px value.
    pub fn px(v: f32) -> Self {
        LengthPercentage::Lin(Lin { px: v, pct: 0.0 })
    }

    /// A percentage.
    pub fn pct(v: f32) -> Self {
        LengthPercentage::Lin(Lin { px: 0.0, pct: v })
    }

    /// Resolves against a percentage basis.
    ///
    /// Results are truncated to 1/64 px, the precision of Blink's `LayoutUnit`, so that
    /// rounding at paint time agrees with Chrome on `.5` ties.
    pub fn resolve(&self, basis: f32) -> f32 {
        layout_unit(self.resolve_exact(basis))
    }

    fn resolve_exact(&self, basis: f32) -> f32 {
        match self {
            LengthPercentage::Lin(l) => l.px + l.pct * basis / 100.0,
            LengthPercentage::Calc(n) => {
                let vals = n.items.iter().map(|i| i.resolve_exact(basis));
                match n.op {
                    CalcOp::Min => vals.fold(f32::INFINITY, f32::min),
                    CalcOp::Max => vals.fold(f32::NEG_INFINITY, f32::max),
                }
            }
        }
    }

    /// Whether resolving needs a percentage basis.
    pub fn has_percentage(&self) -> bool {
        match self {
            LengthPercentage::Lin(l) => l.pct != 0.0,
            LengthPercentage::Calc(_) => true,
        }
    }

    /// Resolves, treating percentages against an unknown basis as `None`.
    pub fn resolve_opt(&self, basis: Option<f32>) -> Option<f32> {
        match basis {
            Some(b) => Some(self.resolve(b)),
            None if !self.has_percentage() => Some(self.resolve(0.0)),
            None => None,
        }
    }

    /// Whether this is exactly zero.
    pub fn is_zero(&self) -> bool {
        matches!(self, LengthPercentage::Lin(l) if l.px == 0.0 && l.pct == 0.0)
    }
}

/// Truncates a length to 1/64 px (Blink's `LayoutUnit` precision).
#[inline]
pub fn layout_unit(v: f32) -> f32 {
    (v * 64.0).trunc() / 64.0
}

/// Converts an absolute unit to px. Returns `None` for unknown units.
fn unit_to_linear(value: f32, unit: &str) -> Option<LinearLength> {
    let u = unit.to_ascii_lowercase();
    let mut l = LinearLength::default();
    match u.as_str() {
        "px" => l.px = value,
        "pt" => l.px = value * 96.0 / 72.0,
        "pc" => l.px = value * 16.0,
        "in" => l.px = value * 96.0,
        "cm" => l.px = value * 96.0 / 2.54,
        "mm" => l.px = value * 96.0 / 25.4,
        "q" => l.px = value * 96.0 / 101.6,
        "em" => l.em = value,
        "rem" => l.rem = value,
        "ex" => l.ex = value,
        "ch" => l.ch = value,
        "vw" | "svw" | "lvw" | "dvw" => l.vw = value,
        "vh" | "svh" | "lvh" | "dvh" => l.vh = value,
        "vmin" => l.vmin = value,
        "vmax" => l.vmax = value,
        _ => return None,
    }
    Some(l)
}

/// Parses a single component value as a length-percentage. Unitless zero is accepted.
/// With `quirky_unitless`, any unitless number is taken as px (HTML attributes).
pub fn parse_length(v: &ComponentValue) -> Option<SpecifiedLength> {
    match v {
        ComponentValue::Token(Token::Dimension { value, unit, .. }) => {
            unit_to_linear(*value, unit).map(SpecifiedLength::Linear)
        }
        ComponentValue::Token(Token::Percentage(p)) => Some(SpecifiedLength::Linear(LinearLength::pct(*p))),
        ComponentValue::Token(Token::Number { value, .. }) if *value == 0.0 => Some(SpecifiedLength::px(0.0)),
        ComponentValue::Function { name, args } => match name.to_ascii_lowercase().as_str() {
            "calc" | "-webkit-calc" => match parse_calc_sum(&strip_ws(args))? {
                CalcValue::Length(l) => Some(l),
                CalcValue::Number(0.0) => Some(SpecifiedLength::px(0.0)),
                CalcValue::Number(_) => None,
            },
            "min" | "max" => {
                let items: Option<Vec<SpecifiedLength>> = split_commas(args)
                    .into_iter()
                    .map(|a| match parse_calc_sum(&a)? {
                        CalcValue::Length(l) => Some(l),
                        CalcValue::Number(0.0) => Some(SpecifiedLength::px(0.0)),
                        _ => None,
                    })
                    .collect();
                let items = items?;
                if items.is_empty() {
                    return None;
                }
                Some(if name.eq_ignore_ascii_case("min") {
                    SpecifiedLength::Min(items)
                } else {
                    SpecifiedLength::Max(items)
                })
            }
            "clamp" => {
                let parts = split_commas(args);
                if parts.len() != 3 {
                    return None;
                }
                let mut ls = Vec::new();
                for p in parts {
                    match parse_calc_sum(&p)? {
                        CalcValue::Length(l) => ls.push(l),
                        CalcValue::Number(0.0) => ls.push(SpecifiedLength::px(0.0)),
                        _ => return None,
                    }
                }
                let [a, b, c]: [SpecifiedLength; 3] = ls.try_into().ok()?;
                Some(SpecifiedLength::Clamp(Box::new([a, b, c])))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Parses a plain number, also accepting `calc()` that reduces to a number.
pub fn parse_number(v: &ComponentValue) -> Option<f32> {
    match v {
        ComponentValue::Token(Token::Number { value, .. }) => Some(*value),
        ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("calc") => {
            match parse_calc_sum(&strip_ws(args))? {
                CalcValue::Number(n) => Some(n),
                _ => None,
            }
        }
        _ => None,
    }
}

fn strip_ws(args: &[ComponentValue]) -> Vec<ComponentValue> {
    args.to_vec()
}

fn split_commas(args: &[ComponentValue]) -> Vec<Vec<ComponentValue>> {
    args.split(|a| matches!(a, ComponentValue::Token(Token::Comma)))
        .map(|s| s.to_vec())
        .collect()
}

#[derive(Clone, Debug)]
enum CalcValue {
    Number(f32),
    Length(SpecifiedLength),
}

fn calc_add(a: CalcValue, b: CalcValue, sign: f32) -> Option<CalcValue> {
    match (a, b) {
        (CalcValue::Number(x), CalcValue::Number(y)) => Some(CalcValue::Number(x + sign * y)),
        (CalcValue::Length(SpecifiedLength::Linear(x)), CalcValue::Length(SpecifiedLength::Linear(y))) => {
            Some(CalcValue::Length(SpecifiedLength::Linear(x.add(y, sign))))
        }
        // Treat a bare zero added to a length as a no-op.
        (CalcValue::Length(l), CalcValue::Number(n)) | (CalcValue::Number(n), CalcValue::Length(l)) if n == 0.0 => {
            Some(CalcValue::Length(l))
        }
        // Non-linear (min/max inside calc) sums: approximate by evaluating min/max of sums.
        (CalcValue::Length(a), CalcValue::Length(b)) => {
            let neg_b = scale_spec(b, sign)?;
            Some(CalcValue::Length(add_nonlinear(a, neg_b)))
        }
        _ => None,
    }
}

fn scale_spec(l: SpecifiedLength, k: f32) -> Option<SpecifiedLength> {
    Some(match l {
        SpecifiedLength::Linear(x) => SpecifiedLength::Linear(x.scale(k)),
        SpecifiedLength::Min(v) if k >= 0.0 => {
            SpecifiedLength::Min(v.into_iter().map(|x| scale_spec(x, k)).collect::<Option<_>>()?)
        }
        SpecifiedLength::Max(v) if k >= 0.0 => {
            SpecifiedLength::Max(v.into_iter().map(|x| scale_spec(x, k)).collect::<Option<_>>()?)
        }
        SpecifiedLength::Min(v) => {
            SpecifiedLength::Max(v.into_iter().map(|x| scale_spec(x, k)).collect::<Option<_>>()?)
        }
        SpecifiedLength::Max(v) => {
            SpecifiedLength::Min(v.into_iter().map(|x| scale_spec(x, k)).collect::<Option<_>>()?)
        }
        SpecifiedLength::Clamp(b) => {
            let [lo, val, hi] = *b;
            let lo = scale_spec(lo, k)?;
            let val = scale_spec(val, k)?;
            let hi = scale_spec(hi, k)?;
            if k >= 0.0 {
                SpecifiedLength::Clamp(Box::new([lo, val, hi]))
            } else {
                SpecifiedLength::Clamp(Box::new([hi, val, lo]))
            }
        }
    })
}

/// `a + b` where at least one side is min/max: distribute over the non-linear side.
fn add_nonlinear(a: SpecifiedLength, b: SpecifiedLength) -> SpecifiedLength {
    match (a, b) {
        (SpecifiedLength::Linear(x), SpecifiedLength::Linear(y)) => SpecifiedLength::Linear(x.add(y, 1.0)),
        (SpecifiedLength::Min(v), other) | (other, SpecifiedLength::Min(v)) => {
            SpecifiedLength::Min(v.into_iter().map(|i| add_nonlinear(i, other.clone())).collect())
        }
        (SpecifiedLength::Max(v), other) | (other, SpecifiedLength::Max(v)) => {
            SpecifiedLength::Max(v.into_iter().map(|i| add_nonlinear(i, other.clone())).collect())
        }
        (SpecifiedLength::Clamp(c), other) | (other, SpecifiedLength::Clamp(c)) => {
            let [lo, val, hi] = *c;
            SpecifiedLength::Clamp(Box::new([
                add_nonlinear(lo, other.clone()),
                add_nonlinear(val, other.clone()),
                add_nonlinear(hi, other),
            ]))
        }
    }
}

/// `sum := product (('+' | '-') product)*`
fn parse_calc_sum(tokens: &[ComponentValue]) -> Option<CalcValue> {
    let items: Vec<&ComponentValue> = tokens.iter().filter(|t| !t.is_whitespace()).collect();
    let mut i = 0;
    let mut acc = parse_calc_product(&items, &mut i)?;
    while i < items.len() {
        let sign = match items[i] {
            ComponentValue::Token(Token::Delim('+')) => {
                i += 1;
                1.0
            }
            ComponentValue::Token(Token::Delim('-')) => {
                i += 1;
                -1.0
            }
            // Lenient: `100% -20px` (signed number right after a term).
            ComponentValue::Token(Token::Dimension { signed: true, .. })
            | ComponentValue::Token(Token::Number { signed: true, .. }) => 1.0,
            _ => return None,
        };
        let rhs = parse_calc_product(&items, &mut i)?;
        acc = calc_add(acc, rhs, sign)?;
    }
    Some(acc)
}

fn parse_calc_product(items: &[&ComponentValue], i: &mut usize) -> Option<CalcValue> {
    let mut acc = parse_calc_factor(items, i)?;
    while *i < items.len() {
        let mul = match items[*i] {
            ComponentValue::Token(Token::Delim('*')) => true,
            ComponentValue::Token(Token::Delim('/')) => false,
            _ => break,
        };
        *i += 1;
        let rhs = parse_calc_factor(items, i)?;
        acc = match (acc, rhs, mul) {
            (CalcValue::Number(a), CalcValue::Number(b), true) => CalcValue::Number(a * b),
            (CalcValue::Number(a), CalcValue::Number(b), false) if b != 0.0 => CalcValue::Number(a / b),
            (CalcValue::Length(l), CalcValue::Number(b), true) | (CalcValue::Number(b), CalcValue::Length(l), true) => {
                CalcValue::Length(scale_spec(l, b)?)
            }
            (CalcValue::Length(l), CalcValue::Number(b), false) if b != 0.0 => {
                CalcValue::Length(scale_spec(l, 1.0 / b)?)
            }
            _ => return None,
        };
    }
    Some(acc)
}

fn parse_calc_factor(items: &[&ComponentValue], i: &mut usize) -> Option<CalcValue> {
    let v = *items.get(*i)?;
    *i += 1;
    match v {
        ComponentValue::Token(Token::Number { value, .. }) => Some(CalcValue::Number(*value)),
        ComponentValue::Block { open: '(', contents } => parse_calc_sum(contents),
        ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("calc") => parse_calc_sum(args),
        other => parse_length(other).map(CalcValue::Length),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::parser::parse_component_values;

    fn ctx() -> LengthContext {
        LengthContext {
            font_size: 20.0,
            root_font_size: 10.0,
            viewport_width: 1000.0,
            viewport_height: 500.0,
        }
    }

    fn len(s: &str) -> LengthPercentage {
        let v = parse_component_values(s);
        parse_length(&v[0])
            .unwrap_or_else(|| panic!("bad length {s}"))
            .compute(&ctx())
    }

    #[test]
    fn units() {
        assert_eq!(len("10px").resolve(0.0), 10.0);
        assert_eq!(len("2em").resolve(0.0), 40.0);
        assert_eq!(len("3rem").resolve(0.0), 30.0);
        assert_eq!(len("10vw").resolve(0.0), 100.0);
        assert_eq!(len("10vh").resolve(0.0), 50.0);
        assert_eq!(len("12pt").resolve(0.0), 16.0);
        assert_eq!(len("1in").resolve(0.0), 96.0);
        assert_eq!(len("50%").resolve(300.0), 150.0);
        assert_eq!(len("0").resolve(300.0), 0.0);
    }

    #[test]
    fn calc_expressions() {
        assert_eq!(len("calc(100% - 20px)").resolve(200.0), 180.0);
        assert_eq!(len("calc(2 * (1em + 5px))").resolve(0.0), 50.0);
        assert!((len("calc(100%/3)").resolve(300.0) - 100.0).abs() < 1e-3);
        assert_eq!(len("calc(50vw - 2rem)").resolve(0.0), 480.0);
        assert_eq!(len("min(100px, 50%)").resolve(100.0), 50.0);
        assert_eq!(len("min(100px, 50%)").resolve(1000.0), 100.0);
        assert_eq!(len("max(1em, 5px)").resolve(0.0), 20.0);
        assert_eq!(len("clamp(10px, 50%, 30px)").resolve(100.0), 30.0);
        assert_eq!(len("calc(min(10px, 5%) + 1px)").resolve(100.0), 6.0);
        let v = parse_component_values("calc(1px * 2px)");
        assert!(parse_length(&v[0]).is_none());
    }
}
