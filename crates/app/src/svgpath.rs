//! Small parser for the SVG path `d` attribute, enough for brand logos (simple-icons). Pure (no
//! Win32): the result is absolute segments that `paint` then turns into Direct2D geometry.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    /// Two control points, then the end point.
    Cubic([(f32, f32); 3]),
    /// Control point, then the end point.
    Quad([(f32, f32); 2]),
    Arc { to: (f32, f32), rx: f32, ry: f32, rotation: f32, large: bool, sweep: bool },
    Close,
}

struct Cursor<'a> {
    s: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b',' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }

    fn command(&mut self) -> Option<u8> {
        self.skip();
        let c = *self.s.get(self.i)?;
        c.is_ascii_alphabetic().then(|| {
            self.i += 1;
            c
        })
    }

    /// Compact SVG numbers: "-.5", "2.6471.079" (two numbers), "1e-3".
    fn number(&mut self) -> Option<f32> {
        self.skip();
        let start = self.i;
        let at = |i: usize| self.s.get(i).copied().unwrap_or(0);
        let mut i = self.i;
        if matches!(at(i), b'+' | b'-') {
            i += 1;
        }
        let mut dot = false;
        while at(i).is_ascii_digit() || (at(i) == b'.' && !dot) {
            dot |= at(i) == b'.';
            i += 1;
        }
        if matches!(at(i), b'e' | b'E') && (at(i + 1).is_ascii_digit() || matches!(at(i + 1), b'+' | b'-')) {
            i += 2;
            while at(i).is_ascii_digit() {
                i += 1;
            }
        }
        let v = std::str::from_utf8(&self.s[start..i]).ok()?.parse().ok()?;
        self.i = i;
        Some(v)
    }

    /// Arc flags may be glued together without a separator ("a1 1 0 01.5.5").
    fn flag(&mut self) -> Option<bool> {
        self.skip();
        let c = *self.s.get(self.i)?;
        matches!(c, b'0' | b'1').then(|| {
            self.i += 1;
            c == b'1'
        })
    }

    fn at_end(&mut self) -> bool {
        self.skip();
        self.i >= self.s.len()
    }
}

/// Parses a path; stops silently at unrecognized data (the logo is still partially drawn).
type Point = (f32, f32);

pub fn parse(d: &str) -> Vec<Seg> {
    let mut c = Cursor { s: d.as_bytes(), i: 0 };
    let mut out = Vec::new();
    let (mut cur, mut start) = ((0.0f32, 0.0f32), (0.0f32, 0.0f32));
    // Last control point, for S/T (reflected).
    let (mut last_cubic, mut last_quad): (Option<Point>, Option<Point>) = (None, None);
    let mut cmd = 0u8;
    while !c.at_end() {
        if let Some(next) = c.command() {
            cmd = next;
        } else if cmd == 0 {
            break;
        }
        let rel = cmd.is_ascii_lowercase();
        let base = if rel { cur } else { (0.0, 0.0) };
        let point = |c: &mut Cursor| -> Option<(f32, f32)> { Some((c.number()? + base.0, c.number()? + base.1)) };
        let seg = match cmd.to_ascii_uppercase() {
            b'M' => {
                let Some(p) = point(&mut c) else { break };
                start = p;
                // A following pair without a command letter is a line.
                cmd = if rel { b'l' } else { b'L' };
                Seg::Move(p.0, p.1)
            }
            b'L' => {
                let Some(p) = point(&mut c) else { break };
                Seg::Line(p.0, p.1)
            }
            b'H' => {
                let Some(x) = c.number() else { break };
                Seg::Line(x + base.0, cur.1)
            }
            b'V' => {
                let Some(y) = c.number() else { break };
                Seg::Line(cur.0, y + base.1)
            }
            b'C' => {
                let (Some(a), Some(b), Some(e)) = (point(&mut c), point(&mut c), point(&mut c)) else { break };
                Seg::Cubic([a, b, e])
            }
            b'S' => {
                let (Some(b), Some(e)) = (point(&mut c), point(&mut c)) else { break };
                let a = last_cubic.map_or(cur, |k| (2.0 * cur.0 - k.0, 2.0 * cur.1 - k.1));
                Seg::Cubic([a, b, e])
            }
            b'Q' => {
                let (Some(a), Some(e)) = (point(&mut c), point(&mut c)) else { break };
                Seg::Quad([a, e])
            }
            b'T' => {
                let Some(e) = point(&mut c) else { break };
                let a = last_quad.map_or(cur, |k| (2.0 * cur.0 - k.0, 2.0 * cur.1 - k.1));
                Seg::Quad([a, e])
            }
            b'A' => {
                let (Some(rx), Some(ry), Some(rotation)) = (c.number(), c.number(), c.number()) else { break };
                let (Some(large), Some(sweep)) = (c.flag(), c.flag()) else { break };
                let Some(to) = point(&mut c) else { break };
                Seg::Arc { to, rx: rx.abs(), ry: ry.abs(), rotation, large, sweep }
            }
            b'Z' => Seg::Close,
            _ => break,
        };
        (last_cubic, last_quad) = (None, None);
        match seg {
            Seg::Move(x, y) | Seg::Line(x, y) => cur = (x, y),
            Seg::Cubic([_, b, e]) => {
                last_cubic = Some(b);
                cur = e;
            }
            Seg::Quad([a, e]) => {
                last_quad = Some(a);
                cur = e;
            }
            Seg::Arc { to, .. } => cur = to,
            Seg::Close => cur = start,
        }
        out.push(seg);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_numbers_and_relative_moves() {
        let segs = parse("m4.7 15.9 4.7-2.6.079-.23Z");
        assert_eq!(segs[0], Seg::Move(4.7, 15.9));
        assert!(matches!(segs[1], Seg::Line(x, y) if (x - 9.4).abs() < 1e-4 && (y - 13.3).abs() < 1e-4));
        assert!(matches!(segs[2], Seg::Line(x, y) if (x - 9.479).abs() < 1e-4 && (y - 13.07).abs() < 1e-4));
        assert_eq!(segs[3], Seg::Close);
    }

    #[test]
    fn horizontal_vertical_and_close_returns_to_start() {
        let segs = parse("M1 1h3v2H1zl1 1");
        assert_eq!(&segs[..4], &[Seg::Move(1.0, 1.0), Seg::Line(4.0, 1.0), Seg::Line(4.0, 3.0), Seg::Line(1.0, 3.0)]);
        assert_eq!(segs[5], Seg::Line(2.0, 2.0));
    }

    #[test]
    fn arcs_with_packed_flags() {
        let segs = parse("M0 0a.79.79 0 0 0-.407-.667A1 1 0 01.5.5");
        assert!(matches!(segs[1], Seg::Arc { rx, large: false, sweep: false, to, .. } if (rx - 0.79).abs() < 1e-6 && (to.0 + 0.407).abs() < 1e-6));
        assert!(matches!(segs[2], Seg::Arc { large: false, sweep: true, to: (x, y), .. } if x == 0.5 && y == 0.5));
    }

    #[test]
    fn smooth_curves_reflect_previous_control() {
        let segs = parse("M0 0Q1 2 2 0t2 0");
        assert_eq!(segs[2], Seg::Quad([(3.0, -2.0), (4.0, 0.0)]));
        let segs = parse("M0 0C0 1 1 1 1 0S2-1 2 0");
        assert_eq!(segs[2], Seg::Cubic([(1.0, -1.0), (2.0, -1.0), (2.0, 0.0)]));
    }

    #[test]
    fn brand_paths_parse_completely() {
        // The parser stops at unrecognized data; if every `z` is reached, the path was read in full.
        for (name, d) in crate::brands::ALL {
            let segs = parse(d);
            let closes = segs.iter().filter(|s| **s == Seg::Close).count();
            assert_eq!(closes, d.matches(['z', 'Z']).count(), "{name}");
            assert!(segs.len() > 3, "{name}");
        }
    }
}
