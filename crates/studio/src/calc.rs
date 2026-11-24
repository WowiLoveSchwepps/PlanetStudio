//! Saisie numérique « à la Adobe » : les champs acceptent une expression
//! (`89+90`, `12.5*2`, `(10-4)/3`, `-45`…) évaluée à la validation.

use eframe::egui;

/// `egui::DragValue` dont la saisie clavier accepte les expressions arithmétiques.
pub fn drag<'a, N: egui::emath::Numeric>(value: &'a mut N) -> egui::DragValue<'a> {
    egui::DragValue::new(value).custom_parser(eval)
}

/// Évalue une expression simple : `+ - * /`, parenthèses, signe unaire.
/// Tolère espaces, virgule décimale, `°`, `%`, `×`, `x`, `÷` et le signe moins U+2212.
pub fn eval(text: &str) -> Option<f64> {
    let mut chars = Vec::with_capacity(text.len());
    for c in text.chars() {
        match c {
            c if c.is_whitespace() => {}
            '°' | '%' => {}
            '−' => chars.push('-'),
            ',' => chars.push('.'),
            '×' | 'x' | 'X' => chars.push('*'),
            '÷' | ':' => chars.push('/'),
            '0'..='9' | '.' | '+' | '-' | '*' | '/' | '(' | ')' | 'e' | 'E' => chars.push(c),
            _ => return None,
        }
    }
    let mut p = Parser { s: &chars, i: 0 };
    let v = p.expr()?;
    (p.i == chars.len() && v.is_finite()).then_some(v)
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(op @ ('+' | '-')) = self.peek() {
            self.i += 1;
            let r = self.term()?;
            v = if op == '+' { v + r } else { v - r };
        }
        Some(v)
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.factor()?;
        while let Some(op @ ('*' | '/')) = self.peek() {
            self.i += 1;
            let r = self.factor()?;
            v = if op == '*' { v * r } else { v / r };
        }
        Some(v)
    }

    fn factor(&mut self) -> Option<f64> {
        match self.peek()? {
            '-' => {
                self.i += 1;
                Some(-self.factor()?)
            }
            '+' => {
                self.i += 1;
                self.factor()
            }
            '(' => {
                self.i += 1;
                let v = self.expr()?;
                (self.peek()? == ')').then(|| self.i += 1)?;
                Some(v)
            }
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.i;
        while let Some(c) = self.peek() {
            let exp_sign = matches!(c, '+' | '-')
                && self.i > start
                && matches!(self.s[self.i - 1], 'e' | 'E');
            if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || exp_sign {
                self.i += 1;
            } else {
                break;
            }
        }
        if start == self.i {
            return None;
        }
        self.s[start..self.i].iter().collect::<String>().parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::eval;

    #[test]
    fn expressions() {
        assert_eq!(eval("89.0+90"), Some(179.0));
        assert_eq!(eval("89.0°+90"), Some(179.0));
        assert_eq!(eval("-0.0-45"), Some(-45.0));
        assert_eq!(eval("10 * 2 + 1"), Some(21.0));
        assert_eq!(eval("(10-4)/3"), Some(2.0));
        assert_eq!(eval("1,5*2"), Some(3.0));
        assert_eq!(eval("2x3"), Some(6.0));
        assert_eq!(eval("−5"), Some(-5.0));
        assert_eq!(eval("1e2+1"), Some(101.0));
        assert_eq!(eval("--3"), Some(3.0));
        assert_eq!(eval("1/0"), None);
        assert_eq!(eval("abc"), None);
        assert_eq!(eval("5+"), None);
        assert_eq!(eval(""), None);
    }
}
