/// The body of `restore_cached_render`.
fn restore_body() -> &'static str {
    let (_, rest) = include_str!("../app_render.rs")
        .split_once("pub(super) fn restore_cached_render(")
        .expect("restore_cached_render is no longer a method here");
    rest.split_once("\n    }")
        .map(|(body, _)| body)
        .expect("restore_cached_render has no recognisable body")
}

#[test]
fn a_restored_image_still_says_what_it_depicts() {
    let body = restore_body();
    let meta = body
        .find("RadarTextureMeta {")
        .expect("restore_cached_render no longer describes the texture it places");
    let fields = &body[meta..];
    for field in ["product,", "elevation,"] {
        assert!(
            fields.contains(field),
            "a restored image carries no `{field}`, so a pane switched while \
                 suspended comes back showing the old product with nothing saying \
                 so; `stale_image_on_screen` reads this metadata and nothing else",
        );
    }
    // The values come from the *cached render*, not from the pane's live
    // selection — which is the whole distinction the notice rests on.
    for source in [
        "let product = cached.product;",
        "let elevation = cached.elevation;",
    ] {
        assert!(
            body.contains(source),
            "`{source}` is gone: the restored image would be described by \
                 whatever the pane has selected rather than by what it depicts",
        );
    }
}
