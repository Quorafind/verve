//! Transforms at the script boundary: the `DOMMatrix` objects `getTransform` answers and
//! `setTransform`, `addPath` and `CanvasPattern.setTransform` take, and tiny-skia's 2D `Transform`.

use boa_engine::{Context, JsResult, JsValue};
use tiny_skia::Transform;

use crate::bind::platform::{Matrix, from_2d, make_matrix, matrix_of};
use verve::native::get;

/// The 2D part of a `DOMMatrix`'s sixteen numbers, column by column as CSS writes them.
pub fn to_transform(matrix: &Matrix) -> Transform {
    let [m11, m12, _, _, m21, m22, _, _, _, _, _, _, m41, m42, _, _] = matrix.map(|n| n as f32);
    Transform::from_row(m11, m12, m21, m22, m41, m42)
}

fn to_matrix(transform: Transform) -> Matrix {
    let Transform {
        sx,
        kx,
        ky,
        sy,
        tx,
        ty,
    } = transform;
    from_2d(
        sx.into(),
        ky.into(),
        kx.into(),
        sy.into(),
        tx.into(),
        ty.into(),
    )
}

/// A `DOMMatrix` for `getTransform`.
pub fn matrix_object(transform: Transform, context: &mut Context) -> JsResult<JsValue> {
    make_matrix(context, &to_matrix(transform))
}

/// A `DOMMatrix2DInit` argument: a `DOMMatrix`, or a dictionary of `a`–`f` (or `m11`…`m42`)
/// with the identity's value for what is missing. `null` and `undefined` are the identity.
pub fn transform_argument(value: &JsValue, context: &mut Context) -> JsResult<Transform> {
    if value.is_null_or_undefined() {
        return Ok(Transform::identity());
    }
    if !get(value, "m11", context)?.is_undefined() {
        return Ok(to_transform(&matrix_of(value, context)?));
    }
    // (name, the other spelling, the identity's value)
    let members = [
        ("a", "m11", 1.),
        ("b", "m12", 0.),
        ("c", "m21", 0.),
        ("d", "m22", 1.),
        ("e", "m41", 0.),
        ("f", "m42", 0.),
    ];
    let mut numbers = [0.; 6];
    for (slot, (name, other, default)) in numbers.iter_mut().zip(members) {
        let mut member = get(value, name, context)?;
        if member.is_undefined() {
            member = get(value, other, context)?;
        }
        *slot = match member.is_undefined() {
            true => default,
            false => member.to_number(context)?,
        };
    }
    let [a, b, c, d, e, f] = numbers;
    Ok(to_transform(&from_2d(a, b, c, d, e, f)))
}
