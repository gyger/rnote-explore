//! Cutting cubic Bézier paths to a parameter range, as ManimGX reveals them.

/// Numbers per cubic curve: 4 points of x and y.
const CUBIC_LEN: usize = 8;

/// One cubic (8 numbers) restricted to the parameter range `[a, b]`.
fn partial_cubic(p: &[f64], a: f64, b: f64) -> [f64; CUBIC_LEN] {
    if a == 1.0 {
        return [p[6], p[7], p[6], p[7], p[6], p[7], p[6], p[7]];
    }
    if b == 0.0 {
        return [p[0], p[1], p[0], p[1], p[0], p[1], p[0], p[1]];
    }

    // Bernstein weights of the restricted curve's control points, as in runtime.js.
    let (ma, mb) = (1.0 - a, 1.0 - b);
    let m = [
        [ma * ma * ma, 3.0 * ma * ma * a, 3.0 * ma * a * a, a * a * a],
        [
            ma * ma * mb,
            2.0 * ma * a * mb + ma * ma * b,
            a * a * mb + 2.0 * ma * a * b,
            a * a * b,
        ],
        [
            ma * mb * mb,
            a * mb * mb + 2.0 * ma * mb * b,
            2.0 * a * mb * b + ma * b * b,
            a * b * b,
        ],
        [mb * mb * mb, 3.0 * mb * mb * b, 3.0 * mb * b * b, b * b * b],
    ];

    let mut out = [0.0; CUBIC_LEN];
    for (r, weights) in m.iter().enumerate() {
        for (k, w) in weights.iter().enumerate() {
            out[2 * r] += w * p[2 * k];
            out[2 * r + 1] += w * p[2 * k + 1];
        }
    }
    out
}

/// Curve index and residue of `alpha` over `n` curves (`integer_interpolate`).
fn split_index(n: usize, alpha: f64) -> (usize, f64) {
    if alpha >= 1.0 {
        return (n - 1, 1.0);
    }
    if alpha <= 0.0 {
        return (0, 0.0);
    }
    let v = n as f64 * alpha;
    (v.trunc() as usize, v.fract())
}

/// Part of a path between `a` and `b` (`VMobject.pointwise_become_partial`).
pub(crate) fn partial_points(points: &[f64], a: f64, b: f64) -> Vec<f64> {
    let n = points.len() / CUBIC_LEN;
    if (a <= 0.0 && b >= 1.0) || n == 0 {
        return points.to_vec();
    }

    let (li, lr) = split_index(n, a);
    let (ui, ur) = split_index(n, b);
    let curve = |i: usize| &points[CUBIC_LEN * i..CUBIC_LEN * (i + 1)];
    if li == ui {
        return partial_cubic(curve(li), lr, ur).to_vec();
    }

    let mut out = partial_cubic(curve(li), lr, 1.0).to_vec();
    out.extend_from_slice(&points[CUBIC_LEN * (li + 1)..CUBIC_LEN * ui]);
    out.extend_from_slice(&partial_cubic(curve(ui), 0.0, ur));
    out
}
