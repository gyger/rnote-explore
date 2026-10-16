//! Arc and partial path operations on flat `[x, y, ...]` points
//! (slide-format.md, sections 7.3 and 7.4; reference: Manim's `ops.py`).

use super::timeline::Subpath;
use std::f64::consts::PI;

/// Point equality tolerance of `ops.split_subpaths` (`COMPUTED_ATOL`, `_RTOL`).
const ATOL: f64 = 2e-4;
const RTOL: f64 = 1e-5;
/// Numbers per cubic curve: 4 points of x and y.
const CUBIC_LEN: usize = 8;

fn rotate(x: f64, y: f64, angle: f64) -> (f64, f64) {
    let (s, c) = angle.sin_cos();
    (c * x - s * y, s * x + c * y)
}

/// Points at `alpha` of an arc motion from `start` to `end`.
///
/// Without `center` each point moves on its own arc over its chord
/// (`path_along_arc`); with it, all points rotate about it (`path_along_circles`).
pub(crate) fn arc_points(
    start: &[f64],
    end: &[f64],
    alpha: f64,
    angle: f64,
    center: Option<[f64; 2]>,
) -> Vec<f64> {
    let mut out = vec![0.0; start.len()];

    for i in (0..start.len()).step_by(2) {
        let (sx, sy, ex, ey) = (start[i], start[i + 1], end[i], end[i + 1]);

        let (x, y) = match center {
            None => {
                let (hx, hy) = ((ex - sx) / 2.0, (ey - sy) / 2.0);
                let offset = if angle != PI {
                    1.0 / (angle / 2.0).tan()
                } else {
                    0.0
                };
                let (mx, my) = (sx + hx - hy * offset, sy + hy + hx * offset);
                let (rx, ry) = rotate(sx - mx, sy - my, alpha * angle);
                (mx + rx, my + ry)
            }
            Some([cx, cy]) => {
                let (dx, dy) = rotate(ex - cx, ey - cy, -angle);
                let bx = sx + (cx + dx - sx) * alpha;
                let by = sy + (cy + dy - sy) * alpha;
                let (rx, ry) = rotate(bx - cx, by - cy, alpha * angle);
                (cx + rx, cy + ry)
            }
        };
        out[i] = x;
        out[i + 1] = y;
    }
    out
}

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

/// Whether points `i` and `j` coincide within the computed-path tolerance.
fn close(p: &[f64], i: usize, j: usize) -> bool {
    (p[2 * i] - p[2 * j]).abs() <= ATOL + RTOL * p[2 * j].abs()
        && (p[2 * i + 1] - p[2 * j + 1]).abs() <= ATOL + RTOL * p[2 * j + 1].abs()
}

/// Subpaths as Cairo would draw them: a new one wherever a cubic does not
/// start at the previous end (`ops.split_subpaths`).
pub(crate) fn split_subpaths(points: &[f64]) -> Vec<Subpath> {
    let n = points.len() / 2;
    if n < 4 {
        return vec![];
    }

    let mut bounds = vec![0];
    bounds.extend((4..n).step_by(4).filter(|&i| !close(points, i - 1, i)));
    bounds.push(n);

    bounds
        .windows(2)
        .filter(|w| w[1] - w[0] >= 4)
        .map(|w| Subpath {
            start: w[0],
            end: w[1],
            closed: close(points, w[0], w[1] - 1),
        })
        .collect()
}
