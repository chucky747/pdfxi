//! Unit tests for [`super::Matrix`].

use super::Matrix;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn identity_leaves_points_unchanged() {
    assert_eq!(Matrix::IDENTITY.apply(3.0, -4.0), (3.0, -4.0));
    assert!(Matrix::IDENTITY.is_identity());
    assert!(Matrix::default().is_identity());
}

#[test]
fn rotation_degrees_are_counter_clockwise() {
    let ninety = Matrix::rotation_degrees(90).expect("90 is a right angle");
    let (x, y) = ninety.apply(1.0, 0.0);
    assert!(close(x, 0.0) && close(y, 1.0), "got ({x}, {y})");

    let two_seventy = Matrix::rotation_degrees(270).expect("270 is a right angle");
    let (x, y) = two_seventy.apply(1.0, 0.0);
    assert!(close(x, 0.0) && close(y, -1.0), "got ({x}, {y})");

    assert!(Matrix::rotation_degrees(45).is_none());
    assert_eq!(Matrix::rotation_degrees(360), Some(Matrix::IDENTITY));
    assert_eq!(Matrix::rotation_degrees(-90), Matrix::rotation_degrees(270));
}

#[test]
fn rotation_into_box_keeps_the_box_in_the_first_quadrant() {
    // A 40 x 200 local box rotated 90 degrees must occupy [0,200] x [0,40].
    let matrix = Matrix::rotation_into_box(90, 40.0, 200.0).expect("valid box");
    let (x0, y0, x1, y1) = matrix
        .transform_bbox(0.0, 0.0, 40.0, 200.0)
        .expect("finite bounds");
    assert!(close(x0, 0.0) && close(y0, 0.0), "({x0}, {y0})");
    assert!(close(x1, 200.0) && close(y1, 40.0), "({x1}, {y1})");

    let matrix = Matrix::rotation_into_box(180, 200.0, 40.0).expect("valid box");
    let (x0, y0, x1, y1) = matrix
        .transform_bbox(0.0, 0.0, 200.0, 40.0)
        .expect("finite bounds");
    assert!(close(x0, 0.0) && close(y0, 0.0), "({x0}, {y0})");
    assert!(close(x1, 200.0) && close(y1, 40.0), "({x1}, {y1})");

    let matrix = Matrix::rotation_into_box(270, 40.0, 200.0).expect("valid box");
    let (x0, y0, x1, y1) = matrix
        .transform_bbox(0.0, 0.0, 40.0, 200.0)
        .expect("finite bounds");
    assert!(close(x0, 0.0) && close(y0, 0.0), "({x0}, {y0})");
    assert!(close(x1, 200.0) && close(y1, 40.0), "({x1}, {y1})");

    assert!(Matrix::rotation_into_box(90, 0.0, 200.0).is_none());
    assert!(Matrix::rotation_into_box(45, 40.0, 200.0).is_none());
}

#[test]
fn composition_applies_self_then_next() {
    let rotate = Matrix::rotation_degrees(90).expect("right angle");
    let shift = Matrix::translation(10.0, 5.0);
    let composed = rotate.then(shift);
    let direct = {
        let (x, y) = rotate.apply(2.0, 3.0);
        shift.apply(x, y)
    };
    assert_eq!(composed.apply(2.0, 3.0), direct);
}

#[test]
fn inverse_round_trips_and_rejects_singular_matrices() {
    let matrix = Matrix::new(2.0, 0.5, -0.25, 3.0, 7.0, -4.0);
    let inverse = matrix.inverse().expect("invertible");
    let (x, y) = inverse.apply(11.0, -2.0);
    let (x, y) = matrix.apply(x, y);
    assert!(close(x, 11.0) && close(y, -2.0), "({x}, {y})");

    let singular = Matrix::new(1.0, 2.0, 2.0, 4.0, 0.0, 0.0);
    assert!(singular.inverse().is_none());
    assert!(
        Matrix::new(f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0)
            .inverse()
            .is_none()
    );
}

#[test]
fn fit_bbox_maps_the_appearance_box_onto_the_target_rect() {
    // Identity appearance matrix: the BBox is stretched onto the rect.
    let placement = Matrix::fit_bbox(
        Matrix::IDENTITY,
        (0.0, 0.0, 100.0, 50.0),
        10.0,
        20.0,
        200.0,
        100.0,
    )
    .expect("valid placement");
    assert_eq!(placement.apply(0.0, 0.0), (10.0, 20.0));
    assert_eq!(placement.apply(100.0, 50.0), (210.0, 120.0));

    // A rotated appearance of a 40 x 200 box is already inside [0,200] x [0,40];
    // the placement is then a pure translation that keeps the whole quad inside
    // the widget rectangle.
    let rotation = Matrix::rotation_into_box(90, 40.0, 200.0).expect("valid box");
    let placement = Matrix::fit_bbox(rotation, (0.0, 0.0, 40.0, 200.0), 300.0, 450.0, 200.0, 40.0)
        .expect("valid placement");
    let (x0, y0, x1, y1) = placement
        .transform_bbox(0.0, 0.0, 40.0, 200.0)
        .expect("finite bounds");
    assert!(close(x0, 300.0) && close(y0, 450.0), "({x0}, {y0})");
    assert!(close(x1, 500.0) && close(y1, 490.0), "({x1}, {y1})");
}

#[test]
fn fit_bbox_rejects_degenerate_and_non_finite_input() {
    assert!(
        Matrix::fit_bbox(
            Matrix::IDENTITY,
            (0.0, 0.0, 0.0, 10.0),
            0.0,
            0.0,
            100.0,
            100.0
        )
        .is_none()
    );
    assert!(
        Matrix::fit_bbox(
            Matrix::IDENTITY,
            (0.0, 0.0, 10.0, 10.0),
            0.0,
            0.0,
            0.0,
            100.0
        )
        .is_none()
    );
    assert!(
        Matrix::fit_bbox(
            Matrix::IDENTITY,
            (0.0, 0.0, 10.0, 10.0),
            0.0,
            0.0,
            f64::INFINITY,
            100.0
        )
        .is_none()
    );
}

#[test]
fn non_finite_matrices_are_never_identity() {
    let matrix = Matrix::new(1.0, 0.0, 0.0, 1.0, f64::NAN, 0.0);
    assert!(!matrix.is_finite());
    assert!(!matrix.is_identity());
    assert!(matrix.transform_bbox(0.0, 0.0, 1.0, 1.0).is_none());
}
